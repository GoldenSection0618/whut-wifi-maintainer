use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("配置文件读写失败: {0}")]
    Io(#[from] io::Error),
    #[error("配置 TOML 格式或字段类型错误（为保护凭据，不显示原文）")]
    Parse,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("无法序列化配置")]
    Serialize,
}

// Deliberately no Debug: the configuration contains credentials.
#[derive(Deserialize, Serialize)]
pub struct Config {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub wired: bool,
    #[serde(default)]
    pub wired_interface: Option<String>,
    #[serde(default)]
    pub monitor: MonitorSettings,
    #[serde(default)]
    pub portal: PortalSettings,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MonitorSettings {
    pub interval_secs: u64,
    pub timeout_secs: u64,
    pub http_url: String,
    pub http_expected_body: String,
    pub https_url: String,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            interval_secs: 30,
            timeout_secs: 5,
            http_url: "http://www.msftconnecttest.com/connecttest.txt".into(),
            http_expected_body: "Microsoft Connect Test".into(),
            https_url: "https://www.baidu.com/".into(),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct PortalSettings {
    pub fallback_nas_id: String,
}

impl Default for PortalSettings {
    fn default() -> Self {
        Self {
            fallback_nas_id: "52".into(),
        }
    }
}

pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl Config {
    pub fn new(credentials: Credentials) -> Self {
        Self {
            username: credentials.username,
            password: credentials.password,
            wired: false,
            wired_interface: None,
            monitor: MonitorSettings::default(),
            portal: PortalSettings::default(),
        }
    }

    pub fn wired_interface(&self) -> Option<&str> {
        self.wired
            .then_some(self.wired_interface.as_deref())
            .flatten()
    }

    pub fn update_credentials(&mut self, credentials: Credentials) {
        self.username = credentials.username;
        self.password = credentials.password;
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.username.trim().is_empty() || self.password.is_empty() {
            return Err(ConfigError::Invalid("账号或密码为空"));
        }
        if self.wired
            && !self.wired_interface().is_some_and(|name| {
                !name.is_empty()
                    && name.len() < 16
                    && !name
                        .chars()
                        .any(|c| c.is_whitespace() || c == '/' || c == '\0')
            })
        {
            return Err(ConfigError::Invalid("有线模式需要有效的 wired_interface"));
        }
        if !(1..=86400).contains(&self.monitor.interval_secs)
            || !(1..=120).contains(&self.monitor.timeout_secs)
        {
            return Err(ConfigError::Invalid(
                "检查间隔应为 1–86400 秒，超时应为 1–120 秒",
            ));
        }
        for (value, scheme) in [
            (&self.monitor.http_url, "http"),
            (&self.monitor.https_url, "https"),
        ] {
            let url =
                reqwest::Url::parse(value).map_err(|_| ConfigError::Invalid("探测地址格式错误"))?;
            if url.scheme() != scheme
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(ConfigError::Invalid("探测地址协议不匹配，或包含凭据、片段"));
            }
        }
        if self.monitor.http_expected_body.trim().is_empty()
            || self.monitor.http_expected_body.len() > 65536
        {
            return Err(ConfigError::Invalid("HTTP 预期正文必须非空且不超过 64 KiB"));
        }
        if self.portal.fallback_nas_id.is_empty()
            || !self
                .portal
                .fallback_nas_id
                .bytes()
                .all(|b| b.is_ascii_digit())
        {
            return Err(ConfigError::Invalid("备用 nasId 必须为非空数字字符串"));
        }
        Ok(())
    }
}

pub struct ConfigStore {
    path: PathBuf,
}

pub enum SaveOutcome {
    Saved,
    #[cfg_attr(not(unix), allow(dead_code))]
    DurabilityUnconfirmed(io::Error),
}

impl ConfigStore {
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self, ConfigError> {
        let executable = std::env::current_exe()?;
        let default = executable
            .parent()
            .ok_or(ConfigError::Invalid("无法确定程序目录"))?
            .join("config.toml");
        Self::from_candidates(explicit, &[default, PathBuf::from("config.toml")])
    }

    fn from_candidates(
        explicit: Option<PathBuf>,
        candidates: &[PathBuf],
    ) -> Result<Self, ConfigError> {
        if let Some(path) = explicit {
            return Ok(Self {
                path: std::path::absolute(path)?,
            });
        }
        for path in candidates {
            // Permission errors must not silently select another configuration.
            if path.try_exists()? {
                return Ok(Self {
                    path: std::path::absolute(path)?,
                });
            }
        }
        Ok(Self {
            path: std::path::absolute(&candidates[0])?,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Config, ConfigError> {
        let content = fs::read_to_string(&self.path)?;
        let config: Config = toml::from_str(&content).map_err(|_| ConfigError::Parse)?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, config: &Config) -> Result<SaveOutcome, ConfigError> {
        config.validate()?;
        let content = toml::to_string(config).map_err(|_| ConfigError::Serialize)?;
        Ok(atomic_write(&self.path, &content, |file| file.sync_all())?)
    }
}

fn atomic_write(
    path: &Path,
    content: &str,
    sync: impl FnOnce(&fs::File) -> io::Result<()>,
) -> io::Result<SaveOutcome> {
    atomic_write_with_sync(path, content, sync, |directory| directory.sync_all())
}

fn atomic_write_with_sync(
    path: &Path,
    content: &str,
    sync: impl FnOnce(&fs::File) -> io::Result<()>,
    sync_directory: impl FnOnce(&fs::File) -> io::Result<()>,
) -> io::Result<SaveOutcome> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    #[cfg(unix)]
    let directory = fs::File::open(parent)?;
    #[cfg(not(unix))]
    let _ = sync_directory;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(content.as_bytes())?;
    sync(temporary.as_file())?;
    temporary.persist(path).map_err(|err| err.error)?;
    #[cfg(unix)]
    if let Err(error) = sync_directory(&directory) {
        return Ok(SaveOutcome::DurabilityUnconfirmed(error));
    }
    Ok(SaveOutcome::Saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn directory_sync_failure_reports_committed_but_not_confirmed_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "original").unwrap();
        let outcome = atomic_write_with_sync(
            &path,
            "replacement",
            |f| f.sync_all(),
            |_| Err(io::Error::other("injected directory sync failure")),
        )
        .unwrap();
        assert!(matches!(outcome, SaveOutcome::DurabilityUnconfirmed(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
    }

    fn sample() -> Config {
        toml::from_str("username='student'\npassword='old'\nwired=true\nwired_interface='wan'")
            .unwrap()
    }

    #[test]
    fn legacy_configuration_uses_defaults() {
        let config: Config = toml::from_str("username='student'\npassword='password'").unwrap();
        config.validate().unwrap();
        assert!(!config.wired);
        assert_eq!(config.monitor.interval_secs, 30);
        assert_eq!(config.monitor.timeout_secs, 5);
        assert_eq!(config.portal.fallback_nas_id, "52");
    }

    #[test]
    fn explicit_path_never_falls_back() {
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("config.toml");
        fs::write(&existing, "username='student'\npassword='old'").unwrap();
        let store =
            ConfigStore::from_candidates(Some(directory.path().join("missing")), &[existing])
                .unwrap();
        assert!(
            matches!(store.load(), Err(ConfigError::Io(err)) if err.kind() == io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn saves_to_loaded_path_and_preserves_settings() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("missing");
        let second = directory.path().join("config.toml");
        fs::write(&second, toml::to_string(&sample()).unwrap()).unwrap();
        let store = ConfigStore::from_candidates(None, &[first.clone(), second.clone()]).unwrap();
        let mut config = store.load().unwrap();
        config.update_credentials(Credentials {
            username: "new".into(),
            password: "new\0密码\t\"\\".into(),
        });
        store.save(&config).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.password, config.password);
        assert_eq!(loaded.wired_interface(), Some("wan"));
        assert_eq!(store.path(), second);
        assert!(!first.exists());
    }

    #[test]
    fn failed_write_preserves_original_and_removes_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "original").unwrap();
        assert!(
            atomic_write(&path, "replacement", |_| Err(io::Error::other(
                "injected sync failure"
            )))
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn creates_and_replaces_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        for existing in [false, true] {
            if existing {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            }
            atomic_write(&path, "secret", |f| f.sync_all()).unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn rejects_invalid_interface_timing_and_probe_protocols() {
        let mut config = sample();
        config.wired_interface = Some(" ".into());
        assert!(config.validate().is_err());
        config.wired_interface = Some("wan".into());
        config.monitor.timeout_secs = 0;
        assert!(config.validate().is_err());
        config.monitor.timeout_secs = 5;
        config.monitor.https_url = "http://example.com".into();
        assert!(config.validate().is_err());
    }
}

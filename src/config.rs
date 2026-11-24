use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize)]
pub struct Config {
    pub username: String,
    pub password: String,
    /// Linux/OpenWrt 有线模式：必须显式开启并绑定 WAN 接口后才启用。
    #[serde(default)]
    pub wired: bool,
    /// 有线模式绑定的接口名（如 eth0、eth0.2、pppoe-wan）。
    #[serde(default)]
    pub wired_interface: Option<String>,
    #[serde(skip)]
    source_path: Option<PathBuf>,
    #[serde(skip)]
    credentials_need_save: bool,
}

#[cfg_attr(windows, derive(Deserialize))]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl Config {
    fn has_credentials(&self) -> bool {
        !self.username.trim().is_empty() && !self.password.is_empty()
    }

    pub fn wired_interface(&self) -> Option<&str> {
        self.wired
            .then_some(self.wired_interface.as_deref())
            .flatten()
            .filter(|iface| {
                !iface.is_empty()
                    && iface.len() < 16
                    && *iface != "."
                    && *iface != ".."
                    && !iface
                        .chars()
                        .any(|c| c.is_whitespace() || c == '/' || c == ':' || c == '\0')
            })
    }

    #[cfg(any(not(windows), test))]
    fn validate_wired(&self) -> Result<(), Box<dyn Error>> {
        if !self.wired {
            return Err("Linux/OpenWrt 必须明确设置 wired = true".into());
        }
        if self.wired_interface().is_none() {
            return Err(
                "必须设置有效的 wired_interface：非空、少于 16 字节，且不能包含空白、斜线或空字符"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn update_credentials(&mut self, credentials: Credentials) {
        if self.username != credentials.username || self.password != credentials.password {
            self.username = credentials.username;
            self.password = credentials.password;
            self.credentials_need_save = true;
        }
    }
}

fn config_path() -> PathBuf {
    // 发布版将配置放在 exe 同目录，便于随程序一起移动。
    if let Ok(exe_path) = std::env::current_exe()
        && let Some(exe_dir) = exe_path.parent()
    {
        return exe_dir.join("config.toml");
    }

    PathBuf::from("config.toml")
}

fn config_candidates() -> Vec<PathBuf> {
    let beside_exe = config_path();
    let in_working_dir = std::env::current_dir()
        .map(|dir| dir.join("config.toml"))
        .unwrap_or_else(|_| PathBuf::from("config.toml"));
    if beside_exe == in_working_dir {
        vec![beside_exe]
    } else {
        vec![beside_exe, in_working_dir]
    }
}

fn load_config() -> Result<Option<Config>, Box<dyn Error>> {
    load_config_from(config_candidates())
}

pub(crate) fn load_config_from(paths: Vec<PathBuf>) -> Result<Option<Config>, Box<dyn Error>> {
    for path in paths {
        if path.try_exists()? {
            let content = read_config_file(&path, protect_config_file)?;
            let mut config: Config = toml::from_str(&content)
                .map_err(|_| "配置 TOML 格式或字段类型错误（为保护凭据，不显示原文）")?;

            config.source_path = Some(path);
            return Ok(Some(config));
        }
    }

    Ok(None)
}

fn read_config_file(
    path: &Path,
    protect: impl FnOnce(&fs::File) -> io::Result<()>,
) -> io::Result<String> {
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "配置路径必须指向普通文件",
        ));
    }
    let mut file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "配置路径必须指向普通文件",
        ));
    }
    // Protect the opened file before reading any credentials, even malformed ones.
    protect(&file).map_err(|error| {
        io::Error::new(error.kind(), format!("无法将配置权限收紧为 0600: {error}"))
    })?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

fn protect_config_file(file: &fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

pub fn prompt_credentials() -> Result<Credentials, Box<dyn Error>> {
    read_credentials(&mut io::stdin().lock(), &mut io::stdout().lock())
}

fn read_credentials(
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<Credentials, Box<dyn Error>> {
    write!(output, "请输入校园网账号: ")?;
    output.flush()?;

    let mut username = String::new();
    if input.read_line(&mut username)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "账号输入已结束").into());
    }
    let username = username.trim().to_string();

    write!(output, "请输入校园网密码: ")?;
    output.flush()?;

    let mut password = String::new();
    if input.read_line(&mut password)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "密码输入已结束").into());
    }
    let password = password.trim_end_matches(['\r', '\n']).to_string();

    if username.is_empty() || password.is_empty() {
        return Err("账号或密码为空".into());
    }

    Ok(Credentials { username, password })
}

pub fn save_changed_credentials(config: &mut Config) -> Result<(), Box<dyn Error>> {
    if !config.credentials_need_save {
        return Ok(());
    }

    let new_path;
    let path = match &config.source_path {
        Some(path) => path,
        None => {
            new_path = config_path();
            &new_path
        }
    };
    let content = toml::to_string(config)?;

    write_config_file(path, &content)?;
    status!("[*] 账号密码已保存到: {}", path.display());
    config.credentials_need_save = false;
    Ok(())
}

#[cfg(unix)]
fn write_config_file(path: &std::path::Path, content: &str) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    // 配置含明文账号密码：创建时即限制为仅所有者可读写，
    // 已存在的文件也一并收紧权限。
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    // mode 只约束新文件；已有文件必须先通过句柄收紧权限，再写入凭据。
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.set_len(0)?;
    file.write_all(content.as_bytes())
}

#[cfg(not(unix))]
fn write_config_file(path: &std::path::Path, content: &str) -> io::Result<()> {
    fs::write(path, content)
}

/// 有线模式必须预先配置；保留具体错误，避免将无效配置误报为未启用。
#[cfg(not(windows))]
pub fn load_wired_config() -> Result<Config, Box<dyn Error>> {
    match load_config() {
        Ok(Some(config)) if config.has_credentials() => {
            config.validate_wired()?;
            let path = config
                .source_path
                .as_ref()
                .expect("loaded config has a path");
            println!("[*] 已读取本地配置: {}", path.display());
            Ok(config)
        }
        Ok(_) => Err(
            "请先在 config.toml 中填写账号密码，并设置 wired = true 和 wired_interface。".into(),
        ),
        Err(err) => Err(err),
    }
}

#[cfg(windows)]
pub fn load_or_prompt_config() -> Result<Option<Config>, Box<dyn Error>> {
    let mut config = match load_config()? {
        Some(config) if config.has_credentials() => {
            let path = config
                .source_path
                .as_ref()
                .expect("loaded config has a path");
            status!("[*] 已读取本地账号密码配置: {}", path.display());
            return Ok(Some(config));
        }
        Some(config) => config,
        None => Config {
            username: String::new(),
            password: String::new(),
            wired: false,
            wired_interface: None,
            source_path: None,
            credentials_need_save: false,
        },
    };

    let path = if let Some(path) = &config.source_path {
        status!("[!] 配置中缺少账号或密码，请在下方填写。");
        path.clone()
    } else {
        status!("[!] 未找到配置文件，请在下方填写账号密码。");
        config_path()
    };
    status!("[*] 验证通过后将保存到 {}", path.display());
    let credentials = match crate::interaction::window_credentials() {
        Some(Some(credentials)) => credentials,
        Some(None) => return Ok(None),
        None => prompt_credentials()?,
    };
    config.update_credentials(credentials);
    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "whut-config-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn updates_are_saved_to_the_loaded_configuration_path() {
        for previous_password in ["old", ""] {
            let directory = test_directory();
            let path = directory.join("config.toml");
            fs::write(
                &path,
                format!("username='student'\npassword='{previous_password}'"),
            )
            .unwrap();
            let mut config = load_config_from(vec![directory.join("missing.toml"), path.clone()])
                .unwrap()
                .unwrap();
            config.update_credentials(Credentials {
                username: "new-student".into(),
                password: " new\0密码\t\"\\ ".into(),
            });
            save_changed_credentials(&mut config).unwrap();
            let content = fs::read_to_string(&path).unwrap();
            let saved: Config = toml::from_str(&content).unwrap();
            assert_eq!(saved.username, "new-student");
            assert_eq!(saved.password, config.password);
            assert!(!content.contains("source_path"));
            fs::remove_file(path).unwrap();
            fs::remove_dir(directory).unwrap();
        }
    }

    #[test]
    fn password_whitespace_is_preserved_with_windows_line_endings() {
        let mut input = io::Cursor::new("  student  \r\n  password with spaces  \r\n");
        let mut output = Vec::new();
        let credentials = read_credentials(&mut input, &mut output).unwrap();
        assert_eq!(credentials.username, "student");
        assert_eq!(credentials.password, "  password with spaces  ");
        assert!(
            !String::from_utf8(output)
                .unwrap()
                .contains("password with spaces")
        );
    }

    #[test]
    fn closed_input_is_distinct_from_an_empty_entry() {
        for input in ["", "student\r\n"] {
            let error = read_credentials(&mut io::Cursor::new(input), &mut Vec::new())
                .err()
                .unwrap();
            assert_eq!(
                error.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }
        let error = read_credentials(&mut io::Cursor::new("student\r\n\r\n"), &mut Vec::new())
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "账号或密码为空");
    }

    #[test]
    fn malformed_configuration_does_not_expose_credentials() {
        let directory = test_directory();
        let path = directory.join("config.toml");
        fs::write(&path, "password = private-fixture-password").unwrap();
        let error = load_config_from(vec![path.clone()]).err().unwrap();
        assert!(!error.to_string().contains("private-fixture-password"));
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn loading_protects_existing_files_before_validation_without_rewriting() {
        use std::os::unix::fs::PermissionsExt;
        let directory = test_directory();
        let path = directory.join("config.toml");
        for content in [
            "username='student'\npassword='secret'\nwired=true\nwired_interface='wan'",
            "username=''\npassword='secret'",
            "password = invalid-secret",
        ] {
            fs::write(&path, content).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            let _ = load_config_from(vec![path.clone()]);
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), content);
        }
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn permission_failure_prevents_loading_credentials() {
        let directory = test_directory();
        let path = directory.join("config.toml");
        fs::write(&path, "secret-that-must-not-be-reported").unwrap();
        let error = read_config_file(&path, |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected permission failure",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("0600"));
        assert!(
            !error
                .to_string()
                .contains("secret-that-must-not-be-reported")
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn configuration_directory_is_rejected_without_changing_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = test_directory();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        let error = read_config_file(&directory, protect_config_file).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn invalid_wired_configuration_is_rejected_at_startup() {
        for settings in [
            "",
            "wired=false\nwired_interface='wan'",
            "wired=true",
            "wired=true\nwired_interface=''",
            "wired=true\nwired_interface=' '",
            "wired=true\nwired_interface=' wan'",
            "wired=true\nwired_interface='wan '",
            "wired=true\nwired_interface='a/b'",
            "wired=true\nwired_interface='1234567890123456'",
        ] {
            let config: Config = toml::from_str(&format!(
                "username='student'\npassword='secret'\n{settings}"
            ))
            .unwrap();
            assert!(config.validate_wired().is_err(), "accepted {settings:?}");
        }
        wired_config().validate_wired().unwrap();
    }

    fn wired_config() -> Config {
        toml::from_str(
            "username = 'student'\npassword = 'old'\nwired = true\nwired_interface = 'wan'",
        )
        .unwrap()
    }

    #[test]
    fn wired_mode_requires_both_opt_in_and_interface() {
        let mut config: Config =
            toml::from_str("username = 'student'\npassword = 'password'").unwrap();
        assert!(config.wired_interface().is_none());
        config.wired_interface = Some("wan".into());
        assert!(config.wired_interface().is_none());
        config.wired = true;
        assert_eq!(config.wired_interface(), Some("wan"));
        config.wired_interface = Some(" \t".into());
        assert!(config.wired_interface().is_none());
        config.wired_interface = None;
        assert!(config.wired_interface().is_none());
    }

    #[test]
    fn credential_update_preserves_wired_settings_when_saved() {
        let mut config = wired_config();
        config.update_credentials(Credentials {
            username: "new-student".into(),
            // 包括 Rust Debug 转义与 TOML 转义不同的字符。
            password: "new\0密码\t\"\\".into(),
        });
        let saved = toml::to_string(&config).unwrap();
        let loaded: Config = toml::from_str(&saved).unwrap();
        assert_eq!(loaded.username, "new-student");
        assert_eq!(loaded.password, config.password);
        assert_eq!(loaded.wired_interface(), Some("wan"));
    }

    #[cfg(unix)]
    #[test]
    fn config_file_is_private_when_created_and_rewritten() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "whut-config-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        write_config_file(&path, "initial-long-password").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_config_file(&path, "short").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "short");
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}

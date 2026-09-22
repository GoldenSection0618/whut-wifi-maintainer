use serde::Deserialize;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub username: String,
    pub password: String,
    /// Linux/OpenWrt 有线模式：必须显式开启并绑定 WAN 接口后才启用。
    #[serde(default)]
    pub wired: bool,
    /// 有线模式绑定的接口名（如 eth0、eth0.2、pppoe-wan）。
    #[serde(default)]
    pub wired_interface: Option<String>,
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
    vec![config_path(), PathBuf::from("config.toml")]
}

fn load_config() -> Result<Option<(Config, PathBuf)>, Box<dyn Error>> {
    for path in config_candidates() {
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let config: Config = toml::from_str(&content)?;

            if config.username.trim().is_empty() || config.password.is_empty() {
                return Ok(None);
            }

            return Ok(Some((config, path)));
        }
    }

    Ok(None)
}

pub fn prompt_config() -> Result<Config, Box<dyn Error>> {
    print!("请输入校园网账号: ");
    io::stdout().flush()?;

    let mut username = String::new();
    io::stdin().read_line(&mut username)?;
    let username = username.trim().to_string();

    print!("请输入校园网密码: ");
    io::stdout().flush()?;

    let mut password = String::new();
    io::stdin().read_line(&mut password)?;
    let password = password.trim().to_string();

    if username.is_empty() || password.is_empty() {
        return Err("账号或密码为空".into());
    }

    Ok(Config {
        username,
        password,
        wired: false,
        wired_interface: None,
    })
}

pub fn save_config(config: &Config) -> Result<(), Box<dyn Error>> {
    let path = config_path();
    let mut content = format!(
        "username = {:?}\npassword = {:?}\n",
        config.username, config.password
    );
    // 保留有线模式设置，避免交互重输后丢失。
    if config.wired {
        content.push_str("wired = true\n");
        if let Some(iface) = &config.wired_interface {
            content.push_str(&format!("wired_interface = {iface:?}\n"));
        }
    }

    write_config_file(&path, &content)?;
    println!("[*] 账号密码已保存到: {}", path.display());
    Ok(())
}

#[cfg(unix)]
fn write_config_file(path: &std::path::Path, content: &str) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    // 配置含明文账号密码：创建时即限制为仅所有者可读写，
    // 已存在的文件也一并收紧权限。
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(content.as_bytes()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn write_config_file(path: &std::path::Path, content: &str) -> io::Result<()> {
    fs::write(path, content)
}

/// 静默读取配置（不提示输入），供 Linux 有线模式检测使用。
#[cfg(not(windows))]
pub fn try_load_config() -> Option<Config> {
    load_config().ok().flatten().map(|(config, _)| config)
}

fn exit_after_config_error(err: Box<dyn Error>) -> ! {
    eprintln!("[!] {err}");
    eprintln!("[!] 程序将在 10 秒后退出。");
    thread::sleep(Duration::from_secs(10));
    std::process::exit(1);
}

pub fn load_or_prompt_config() -> Config {
    match load_config() {
        Ok(Some((config, path))) => {
            println!("[*] 已读取本地账号密码配置: {}", path.display());
            config
        }
        Ok(None) => {
            println!("[!] 未找到本地账号密码配置，请输入一次。");
            prompt_config().unwrap_or_else(|err| exit_after_config_error(err))
        }
        Err(err) => exit_after_config_error(err),
    }
}

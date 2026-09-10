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

    Ok(Config { username, password })
}

pub fn save_config(config: &Config) -> Result<(), Box<dyn Error>> {
    let path = config_path();
    let content = format!(
        "username = {:?}\npassword = {:?}\n",
        config.username, config.password
    );

    fs::write(&path, content)?;
    println!("[*] 账号密码已保存到: {}", path.display());
    Ok(())
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

use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN, REFERER, USER_AGENT};
use serde::Deserialize;
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

const CHECK_INTERVAL_SECS: u64 = 30;

const CONNECT_TEST_URL: &str = "http://www.msftconnecttest.com/connecttest.txt";
const REDIRECT_URL: &str = "http://www.msftconnecttest.com/redirect";
const CSRF_TOKEN_URL: &str = "http://172.30.21.100/api/csrf-token";
const LOGIN_URL: &str = "http://172.30.21.100/api/account/login";
const AUTH_ORIGIN: &str = "http://172.30.21.100";
const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Deserialize)]
struct Config {
    username: String,
    password: String,
}

struct AuthContext {
    referer: String,
    nas_id: String,
    from_redirect: bool,
}

enum LoginOutcome {
    Verified,
    Rejected,
    Inconclusive,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RuntimeState {
    Unknown,
    NetworkOk,
    NetworkUnavailable,
    AlreadyOnline,
}

#[cfg(windows)]
fn configure_console() {
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleOutputCP(65001);
        windows_sys::Win32::System::Console::SetConsoleCP(65001);
    }
}

#[cfg(not(windows))]
fn configure_console() {}

fn config_path() -> PathBuf {
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

fn prompt_config() -> Result<Config, Box<dyn Error>> {
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

fn save_config(config: &Config) -> Result<(), Box<dyn Error>> {
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

fn load_or_prompt_config() -> Config {
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

fn startup_config() -> Config {
    let mut config = load_or_prompt_config();

    if is_network_ok() {
        println!("[+] 当前网络已连接，本次启动只确认账号密码配置存在。");
        println!("[*] 如果后续掉线，程序会在重新认证时检查账号密码是否正确。");
        return config;
    }

    loop {
        println!("[*] 当前网络未连接，正在检查账号密码是否可用于认证...");

        match do_login(&config, true) {
            Ok(LoginOutcome::Verified) => {
                if let Err(err) = save_config(&config) {
                    println!("[!] 账号密码可用，但保存失败: {err}");
                }
                return config;
            }
            Ok(LoginOutcome::Rejected) => {
                println!("[!] 当前账号密码认证失败，请重新输入。");
                match prompt_config() {
                    Ok(new_config) => config = new_config,
                    Err(err) => println!("[!] 未更新账号密码: {err}"),
                }
            }
            Ok(LoginOutcome::Inconclusive) => {
                println!("[+] 当前设备已经在线，无法进一步验证密码；后续掉线时再检查。");
                return config;
            }
            Err(err) => {
                println!("[!] 无法完成认证检查: {err}");
                println!("[!] 10 秒后重试。");
                thread::sleep(Duration::from_secs(10));
            }
        }
    }
}

fn prompt_until_verified(config: &mut Config) -> RuntimeState {
    loop {
        match prompt_config() {
            Ok(new_config) => *config = new_config,
            Err(err) => {
                println!("[!] 未更新账号密码: {err}");
                continue;
            }
        }

        match do_login(config, true) {
            Ok(LoginOutcome::Verified) => {
                if let Err(err) = save_config(config) {
                    println!("[!] 账号密码可用，但保存失败: {err}");
                }
                println!("[+] 认证成功，网络已恢复。");
                return RuntimeState::NetworkOk;
            }
            Ok(LoginOutcome::Rejected) => {
                println!("[!] 账号密码仍然不正确，请重新输入。");
            }
            Ok(LoginOutcome::Inconclusive) => {
                println!("[!] 当前设备已经在线，无法确认刚输入的密码是否正确；配置未更新。");
                return RuntimeState::AlreadyOnline;
            }
            Err(err) => {
                println!("[!] 重试认证异常: {err}");
                return RuntimeState::NetworkUnavailable;
            }
        }
    }
}

fn build_client() -> Result<Client, Box<dyn Error>> {
    Ok(Client::builder()
        .cookie_store(true)
        .user_agent(USER_AGENT_VALUE)
        .build()?)
}

fn is_network_ok() -> bool {
    let client = match Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent(USER_AGENT_VALUE)
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    match client.get(CONNECT_TEST_URL).send() {
        Ok(resp) if resp.status().is_success() => match resp.text() {
            Ok(text) => text.trim() == "Microsoft Connect Test",
            Err(_) => false,
        },
        _ => false,
    }
}

fn extract_nas_id(final_url: &str) -> String {
    reqwest::Url::parse(final_url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "nasId")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or_else(|| "52".to_string())
}

fn auth_context(client: &Client, verbose: bool) -> AuthContext {
    match client
        .get(REDIRECT_URL)
        .timeout(Duration::from_secs(10))
        .send()
    {
        Ok(resp) => {
            let final_url = resp.url().to_string();
            let nas_id = extract_nas_id(&final_url);

            if verbose {
                println!("[*] 已重定向到认证页面: {final_url}");
                println!("[*] nasId = {nas_id}");
            }

            AuthContext {
                referer: final_url,
                nas_id,
                from_redirect: true,
            }
        }
        Err(err) => {
            if verbose {
                println!("[!] 重定向检查失败: {err}");
                println!("[*] 改用认证服务器和默认 nasId = 52 继续尝试");
            }

            AuthContext {
                referer: AUTH_ORIGIN.to_string(),
                nas_id: "52".to_string(),
                from_redirect: false,
            }
        }
    }
}

fn response_message(result: &Value) -> Option<&str> {
    result
        .get("authMsg")
        .and_then(Value::as_str)
        .filter(|msg| !msg.is_empty())
        .or_else(|| {
            result
                .get("msg")
                .and_then(Value::as_str)
                .filter(|msg| !msg.is_empty())
        })
}

fn do_login(config: &Config, verbose: bool) -> Result<LoginOutcome, Box<dyn Error>> {
    let client = build_client()?;
    let auth_context = auth_context(&client, verbose);

    let csrf_json: Value = client
        .get(CSRF_TOKEN_URL)
        .timeout(Duration::from_secs(5))
        .send()?
        .error_for_status()?
        .json()?;
    let Some(csrf_token) = csrf_json.get("csrf_token").and_then(Value::as_str) else {
        println!("[!] 获取 CSRF token 失败");
        return Ok(LoginOutcome::Rejected);
    };

    if verbose {
        println!("[*] 已获取 CSRF token");
    }

    let mut headers = HeaderMap::new();
    headers.insert("X-CSRF-Token", HeaderValue::from_str(csrf_token)?);
    headers.insert(
        "X-Requested-With",
        HeaderValue::from_static("XMLHttpRequest"),
    );
    headers.insert(REFERER, HeaderValue::from_str(&auth_context.referer)?);
    headers.insert(ORIGIN, HeaderValue::from_static(AUTH_ORIGIN));
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded; charset=UTF-8"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));

    let login_resp = client
        .post(LOGIN_URL)
        .headers(headers)
        .form(&[
            ("username", config.username.as_str()),
            ("password", config.password.as_str()),
            ("nasId", auth_context.nas_id.as_str()),
        ])
        .timeout(Duration::from_secs(10))
        .send()?
        .error_for_status()?;

    let result: Value = login_resp.json()?;

    if verbose {
        println!("[*] 登录响应: {result}");
    }

    if result.get("code").and_then(Value::as_i64) == Some(0)
        || result.get("msg").and_then(Value::as_str) == Some("success")
    {
        if !auth_context.from_redirect && is_network_ok() {
            Ok(LoginOutcome::Inconclusive)
        } else {
            if verbose {
                println!("[+] 登录成功");
            }
            Ok(LoginOutcome::Verified)
        }
    } else {
        if let Some(message) = response_message(&result) {
            println!("[-] 登录失败: {message}");
        } else {
            println!("[-] 登录失败: {result}");
        }
        Ok(LoginOutcome::Rejected)
    }
}

fn main() {
    configure_console();

    let mut config = startup_config();
    let mut state = RuntimeState::Unknown;

    println!("WHUT WiFi 保持器已启动，每 {CHECK_INTERVAL_SECS} 秒检查一次网络。");

    loop {
        if is_network_ok() {
            if state != RuntimeState::NetworkOk {
                println!("[+] 网络正常。");
                state = RuntimeState::NetworkOk;
            }
        } else {
            if state != RuntimeState::NetworkUnavailable {
                println!("[!] 网络不可用，正在尝试认证。");
                state = RuntimeState::NetworkUnavailable;
            }

            match do_login(&config, false) {
                Ok(LoginOutcome::Verified) => {
                    println!("[+] 认证成功，网络已恢复。");
                    state = RuntimeState::NetworkOk;
                }
                Ok(LoginOutcome::Rejected) => {
                    println!("[!] 认证失败，请重新输入账号密码。");
                    state = prompt_until_verified(&mut config);
                }
                Ok(LoginOutcome::Inconclusive) => {
                    if state != RuntimeState::AlreadyOnline {
                        println!("[+] 当前设备已经在线，本轮无需重新认证。");
                        state = RuntimeState::AlreadyOnline;
                    }
                }
                Err(err) => {
                    if state != RuntimeState::NetworkUnavailable {
                        println!("[!] 认证异常: {err}");
                    }
                }
            }
        }

        thread::sleep(Duration::from_secs(CHECK_INTERVAL_SECS));
    }
}

mod config;
mod monitor;
mod network;
mod portal_auth;
mod protocol;
mod runtime;
#[cfg(test)]
mod test_support;
mod unified_auth;
mod wifi;

use clap::Parser;
use config::{Config, ConfigError, ConfigStore, Credentials};
use network::{HttpClients, ProbeReport, RequestError};
use portal_auth::PortalLoginOutcome;
use runtime::{Event, Next, Runtime};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};
use unified_auth::CredentialVerification;

#[derive(Parser)]
#[command(version, about = "WHUT 校园网保持器")]
struct Cli {
    /// 使用指定配置；不存在或无效时直接失败，不查找其他文件。
    #[arg(long)]
    config: Option<PathBuf>,
    /// 后台模式：凭据错误时退出，绝不请求终端输入。
    #[arg(long)]
    non_interactive: bool,
    /// 只校验配置，不联网、不改写正文；Unix 收紧权限为 0600。
    #[arg(long)]
    check_config: bool,
    /// 显示版本、源码提交、目标架构和软件包修订号，然后退出。
    #[arg(long)]
    build_info: bool,
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

fn prompt_credentials() -> Result<Credentials, String> {
    print!("请输入校园网账号: ");
    io::stdout().flush().map_err(|_| "无法显示输入提示")?;
    let mut username = String::new();
    if io::stdin()
        .read_line(&mut username)
        .map_err(|_| "读取账号失败")?
        == 0
    {
        return Err("输入已结束，请修正配置后重启".into());
    }
    print!("请输入校园网密码: ");
    io::stdout().flush().map_err(|_| "无法显示输入提示")?;
    let mut password = String::new();
    if io::stdin()
        .read_line(&mut password)
        .map_err(|_| "读取密码失败")?
        == 0
    {
        return Err("输入已结束，请修正配置后重启".into());
    }
    // Remove the line ending, not meaningful spaces in a password.
    let password = password.trim_end_matches(['\r', '\n']).to_owned();
    if username.trim().is_empty() || password.is_empty() {
        return Err("账号或密码为空".into());
    }
    Ok(Credentials {
        username: username.trim().into(),
        password,
    })
}

fn replacement_credentials(interactive: bool) -> Result<Credentials, String> {
    if !interactive {
        return Err("凭据被明确拒绝；后台模式已退出，请修正配置后重启服务".into());
    }
    loop {
        match prompt_credentials() {
            Ok(value) => return Ok(value),
            Err(error) if error == "账号或密码为空" => eprintln!("[!] {error}"),
            Err(error) => return Err(error),
        }
    }
}

fn campus_connection(config: &Config) -> Result<wifi::CampusWifi, &'static str> {
    #[cfg(windows)]
    {
        let _ = config;
        wifi::current_campus_wifi().ok_or("当前未连接 WHUT-WLAN、WHUT-DORM 或 WHUT-ISP")
    }
    #[cfg(not(windows))]
    {
        wifi::current_campus_wifi_detailed(config).map_err(|reason| reason.message())
    }
}

fn report_change(previous: &mut String, message: &str) {
    if previous != message {
        println!("{message}");
        *previous = message.to_owned();
    }
}

fn validate_platform(config: &Config) -> Result<(), String> {
    #[cfg(not(windows))]
    if config.wired_interface().is_none() {
        return Err("Linux/OpenWrt 必须设置 wired = true 并绑定 wired_interface".into());
    }
    #[cfg(windows)]
    if config.wired {
        return Err("Windows 使用校园 Wi-Fi 检测，请设置 wired = false".into());
    }
    Ok(())
}

fn run(cli: Cli) -> Result<(), String> {
    if cli.build_info {
        println!(
            "version={}\nsource_commit={}\ntarget={}\npackage_release={}",
            env!("CARGO_PKG_VERSION"),
            env!("WHUT_BUILD_COMMIT"),
            env!("WHUT_BUILD_TARGET"),
            env!("WHUT_PACKAGE_RELEASE")
        );
        return Ok(());
    }
    if !cli.check_config {
        wifi::initialize_windows_runtime();
    }
    let explicit = cli.config.is_some();
    let interactive = !cli.non_interactive && io::stdin().is_terminal();
    let store = ConfigStore::resolve(cli.config).map_err(|e| e.to_string())?;
    let mut pending_credentials = false;
    let mut config = match store.load() {
        Ok(config) => config,
        Err(ConfigError::Io(error))
            if error.kind() == io::ErrorKind::NotFound
                && !explicit
                && interactive
                && !cli.check_config
                && cfg!(windows) =>
        {
            // Preserve Windows' SSID gate before requesting credentials.
            while wifi_connection_missing() {
                println!("[*] 等待连接校园 Wi-Fi。");
                std::thread::sleep(Duration::from_secs(10));
            }
            pending_credentials = true;
            Config::new(replacement_credentials(true)?)
        }
        Err(error) => return Err(error.to_string()),
    };
    config.validate().map_err(|e| e.to_string())?;
    validate_platform(&config)?;
    if cli.check_config {
        println!("[+] 配置有效: {}", store.path().display());
        return Ok(());
    }
    let clients = HttpClients::new(&config).map_err(|e| e.to_string())?;
    let interval = Duration::from_secs(config.monitor.interval_secs);
    let mut runtime = Runtime::new(interval, pending_credentials, interactive);
    let mut network = LiveNetwork { clients };
    let mut store = store;
    let started = Instant::now();
    let mut previous = String::new();
    println!("WHUT 校园网保持器已启动；配置: {}", store.path().display());
    loop {
        let cycle = runtime
            .step(&config, &mut network, &mut store, || started.elapsed())
            .map_err(|error| error.to_string())?;
        for event in cycle.events {
            report_change(&mut previous, &event_message(event));
        }
        match cycle.next {
            Next::Wait(delay) => std::thread::sleep(delay),
            Next::RequestCredentials => {
                config.update_credentials(replacement_credentials(interactive)?);
                runtime.credentials_changed();
            }
            Next::ExitCredentialsRejected => {
                return Err("凭据被明确拒绝；后台模式已退出，请修正配置后重启服务".into());
            }
        }
    }
}

struct LiveNetwork {
    clients: HttpClients,
}

impl runtime::Network for LiveNetwork {
    fn connection(&mut self, config: &Config) -> Result<wifi::CampusWifi, &'static str> {
        campus_connection(config)
    }
    fn probe(&mut self, config: &Config) -> ProbeReport {
        network::probe(&self.clients.probe, &config.monitor)
    }
    fn authenticate(&mut self, config: &Config) -> Result<PortalLoginOutcome, RequestError> {
        portal_auth::reachable(&self.clients.portal)?;
        portal_auth::login(
            &self.clients.portal,
            &self.clients.discovery,
            &config.username,
            &config.password,
            &config.portal.fallback_nas_id,
        )
    }
    fn verify(&mut self, config: &Config) -> Result<CredentialVerification, RequestError> {
        network::credential_session(config).and_then(|session| {
            unified_auth::verify_credentials(&session, &config.username, &config.password)
        })
    }
}

impl runtime::CredentialStore for ConfigStore {
    fn save(&mut self, config: &Config) -> Result<config::SaveOutcome, ConfigError> {
        ConfigStore::save(self, config)
    }
}

fn event_message(event: Event) -> String {
    match event {
        Event::Disconnected(reason) => reason.into(),
        Event::Healthy => "[+] HTTP 与 HTTPS 探测均通过，网络正常。".into(),
        Event::Partial(report) => format!(
            "[!] 网络部分可达；HTTP={:?} HTTPS={:?}，本轮不重新认证。",
            report.http, report.https
        ),
        Event::Waiting => "[!] 外网探测未通过，等待下一轮确认或重试间隔。".into(),
        Event::Recovered => "[+] 认证请求已被接受，HTTP 与 HTTPS 探测均通过，网络已恢复。".into(),
        Event::AcceptedButUnreachable => "[!] 认证请求已被接受，但外网尚未完全恢复。".into(),
        Event::BalanceInsufficient(connection) => wifi::balance_insufficient_tip(connection).into(),
        Event::AuthInconclusive => "[!] 门户未返回明确认证结果，将稍后重试。".into(),
        Event::AuthFailed(error) => format!("[!] 认证请求失败: {error}"),
        Event::VerificationInconclusive => {
            "[!] 凭据校验结果不明确；继续监测网络，稍后重试。".into()
        }
        Event::VerificationFailed(error) => {
            format!("[!] 凭据校验暂不可用: {error}；继续监测网络。")
        }
        Event::CredentialsSaved => "[+] 新凭据已验证并保存。".into(),
        Event::DurabilityUnconfirmed(error) => {
            format!("[!] 新配置已替换，但目录同步失败，断电持久性未确认: {error}")
        }
    }
}

fn wifi_connection_missing() -> bool {
    #[cfg(windows)]
    {
        wifi::current_campus_wifi().is_none()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn main() -> ExitCode {
    configure_console();
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("[!] {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_credentials_never_read_stdin() {
        assert!(replacement_credentials(false).is_err());
    }
    #[test]
    fn cli_supports_explicit_path_and_offline_validation() {
        let cli = Cli::try_parse_from([
            "whut",
            "--config",
            "selected.toml",
            "--non-interactive",
            "--check-config",
        ])
        .unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("selected.toml")));
        assert!(cli.non_interactive && cli.check_config);
    }
}

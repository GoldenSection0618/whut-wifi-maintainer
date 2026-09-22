mod config;
mod network;
mod portal_auth;
mod unified_auth;
mod wifi;

#[cfg(windows)]
use config::load_or_prompt_config;
use config::{Config, prompt_config, save_config};
use network::is_network_ok;
use portal_auth::{PortalLoginOutcome, login};
use std::io::IsTerminal;
use std::thread;
use std::time::Duration;
use unified_auth::{CredentialVerification, verify_credentials};
#[cfg(windows)]
use wifi::current_campus_wifi;
#[cfg(not(windows))]
use wifi::current_campus_wifi_detailed;
use wifi::{CampusWifi, balance_insufficient_tip, initialize_windows_runtime};

const CHECK_INTERVAL_SECS: u64 = 30;
const CAMPUS_CHECK_INTERVAL_SECS: u64 = 10;
pub(crate) const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[cfg(windows)]
const OUTSIDE_CAMPUS_MESSAGE: &str =
    "[*] 当前未接入校园 Wi-Fi，等待连接 WHUT-WLAN、WHUT-DORM 或 WHUT-ISP。";

// 等待间隔只应用于非交互 stdin（如路由器后台运行），
// 交互终端用户应能立即重新输入。
fn sleep_if_non_interactive() {
    if !std::io::stdin().is_terminal() {
        thread::sleep(Duration::from_secs(CAMPUS_CHECK_INTERVAL_SECS));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RuntimeState {
    Unknown,
    OutsideCampus(&'static str),
    NetworkOk,
    NetworkUnavailable,
    AlreadyOnline,
    BalanceInsufficient,
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

fn prompt_until_verified(config: &mut Config, campus_wifi: CampusWifi) -> RuntimeState {
    loop {
        match prompt_config() {
            Ok(new_config) => config.update_credentials(new_config),
            Err(err) => {
                println!("[!] 未更新账号密码: {err}");
                sleep_if_non_interactive();
                continue;
            }
        }

        match login(
            &config.username,
            &config.password,
            true,
            config.wired_interface(),
        ) {
            Ok(PortalLoginOutcome::Verified) => {
                if let Err(err) = save_config(config) {
                    println!("[!] 账号密码可用，但保存失败: {err}");
                }
                println!("[+] 认证成功，网络已恢复。");
                return RuntimeState::NetworkOk;
            }
            Ok(PortalLoginOutcome::Rejected) => {
                println!("[!] 账号密码仍然不正确，请重新输入。");
            }
            Ok(PortalLoginOutcome::Inconclusive) => {
                println!("[!] 当前设备已经在线，无法确认刚输入的密码是否正确；配置未更新。");
                return RuntimeState::AlreadyOnline;
            }
            Ok(PortalLoginOutcome::BalanceInsufficient) => {
                println!("[!] {}", balance_insufficient_tip(campus_wifi));
                return RuntimeState::BalanceInsufficient;
            }
            Err(err) => {
                println!("[!] 重试认证异常: {err}");
                return RuntimeState::NetworkUnavailable;
            }
        }
    }
}

fn main() {
    configure_console();
    initialize_windows_runtime();

    #[cfg(windows)]
    let mut config = None;
    #[cfg(not(windows))]
    let mut config = config::load_wired_config().unwrap_or_else(|error| {
        eprintln!("[!] {error}");
        std::process::exit(1);
    });
    let mut credentials_verified = false;
    let mut state = RuntimeState::Unknown;

    println!("WHUT WiFi 保持器已启动。");

    loop {
        // Windows 先检查 SSID；Linux 先读取显式有线配置。
        // 网络检测通过前不发起认证请求。
        #[cfg(windows)]
        let (campus_wifi, outside_message) = (current_campus_wifi(), OUTSIDE_CAMPUS_MESSAGE);
        #[cfg(not(windows))]
        let (campus_wifi, outside_message) = match current_campus_wifi_detailed(&config) {
            Ok(wifi) => (Some(wifi), ""),
            Err(reason) => (None, reason.message()),
        };

        let Some(campus_wifi) = campus_wifi else {
            if state != RuntimeState::OutsideCampus(outside_message) {
                println!("{outside_message}");
                state = RuntimeState::OutsideCampus(outside_message);
            }

            thread::sleep(Duration::from_secs(CAMPUS_CHECK_INTERVAL_SECS));
            continue;
        };

        #[cfg(windows)]
        let config = config.get_or_insert_with(load_or_prompt_config);
        #[cfg(not(windows))]
        let config = &mut config;
        let network_ok = is_network_ok(config.wired_interface());

        // 每次进程启动后只在已有网络时校验一次统一认证凭据。
        if !credentials_verified && network_ok {
            println!("[*] 当前网络已连接，正在通过统一认证校验账号密码...");

            match verify_credentials(&config.username, &config.password, config.wired_interface()) {
                Ok(CredentialVerification::Valid) => {
                    if let Err(err) = save_config(config) {
                        println!("[!] 账号密码校验成功，但保存失败: {err}");
                    }
                    println!("[+] 账号密码校验成功。");
                    credentials_verified = true;
                }
                Ok(CredentialVerification::Invalid) => {
                    println!("[!] 账号密码校验失败，请重新输入。");
                    match prompt_config() {
                        Ok(new_config) => config.update_credentials(new_config),
                        Err(err) => {
                            println!("[!] 未更新账号密码: {err}");
                            sleep_if_non_interactive();
                        }
                    }
                    continue;
                }
                Ok(CredentialVerification::Inconclusive) => {
                    println!("[!] 统一认证未返回明确结果，10 秒后重试。");
                    thread::sleep(Duration::from_secs(CAMPUS_CHECK_INTERVAL_SECS));
                    continue;
                }
                Err(err) => {
                    println!("[!] 无法完成统一认证校验: {err}");
                    println!("[!] 10 秒后重试。");
                    thread::sleep(Duration::from_secs(CAMPUS_CHECK_INTERVAL_SECS));
                    continue;
                }
            }
        }

        if network_ok {
            if state != RuntimeState::NetworkOk {
                println!("[+] 网络正常。");
                state = RuntimeState::NetworkOk;
            }
        } else {
            if state != RuntimeState::NetworkUnavailable
                && state != RuntimeState::BalanceInsufficient
            {
                println!("[!] 网络不可用，正在尝试认证。");
                state = RuntimeState::NetworkUnavailable;
            }

            match login(
                &config.username,
                &config.password,
                false,
                config.wired_interface(),
            ) {
                Ok(PortalLoginOutcome::Verified) => {
                    println!("[+] 认证成功，网络已恢复。");
                    state = RuntimeState::NetworkOk;
                    credentials_verified = true;
                }
                Ok(PortalLoginOutcome::Rejected) => {
                    println!("[!] 认证失败，请重新输入账号密码。");
                    state = prompt_until_verified(config, campus_wifi);
                    credentials_verified = state == RuntimeState::NetworkOk;
                }
                Ok(PortalLoginOutcome::Inconclusive) => {
                    if state != RuntimeState::AlreadyOnline {
                        println!("[+] 当前设备已经在线，本轮无需重新认证。");
                        state = RuntimeState::AlreadyOnline;
                    }
                }
                Ok(PortalLoginOutcome::BalanceInsufficient) => {
                    if state != RuntimeState::BalanceInsufficient {
                        println!("[!] {}", balance_insufficient_tip(campus_wifi));
                        state = RuntimeState::BalanceInsufficient;
                    }
                }
                Err(err) => {
                    if state != RuntimeState::NetworkUnavailable
                        && state != RuntimeState::BalanceInsufficient
                    {
                        println!("[!] 认证异常: {err}");
                    }
                    state = RuntimeState::NetworkUnavailable;
                }
            }
        }

        thread::sleep(Duration::from_secs(CHECK_INTERVAL_SECS));
    }
}

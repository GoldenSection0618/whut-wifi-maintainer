mod access;
mod config;
mod network;
mod portal_auth;
mod unified_auth;

use access::{Access, AccessBlocker, CampusNetwork};
use config::{Config, prompt_credentials, save_config};
use network::is_network_ok;
use portal_auth::{PortalLoginOutcome, login};
use std::io::IsTerminal;
use std::thread;
use std::time::Duration;
use unified_auth::{CredentialVerification, verify_credentials};

const CHECK_INTERVAL_SECS: u64 = 30;
const CAMPUS_CHECK_INTERVAL_SECS: u64 = 10;

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
    OutsideCampus(AccessBlocker),
    NetworkOk,
    NetworkUnavailable,
    AlreadyOnline,
    BalanceInsufficient,
}

fn prompt_and_update_credentials(config: &mut Config) -> bool {
    match prompt_credentials() {
        Ok(credentials) => {
            config.update_credentials(credentials);
            true
        }
        Err(err) => {
            println!("[!] 未更新账号密码: {err}");
            sleep_if_non_interactive();
            false
        }
    }
}

fn verify_configured_credentials(config: &mut Config) -> bool {
    println!("[*] 当前网络已连接，正在通过统一认证校验账号密码...");

    match verify_credentials(&config.username, &config.password, config.wired_interface()) {
        Ok(CredentialVerification::Valid) => {
            if let Err(err) = save_config(config) {
                println!("[!] 账号密码校验成功，但保存失败: {err}");
            }
            println!("[+] 账号密码校验成功。");
            true
        }
        Ok(CredentialVerification::Invalid) => {
            println!("[!] 账号密码校验失败，请重新输入。");
            prompt_and_update_credentials(config);
            false
        }
        Ok(CredentialVerification::Inconclusive) => {
            println!("[!] 统一认证未返回明确结果，10 秒后重试。");
            thread::sleep(Duration::from_secs(CAMPUS_CHECK_INTERVAL_SECS));
            false
        }
        Err(err) => {
            println!("[!] 无法完成统一认证校验: {err}");
            println!("[!] 10 秒后重试。");
            thread::sleep(Duration::from_secs(CAMPUS_CHECK_INTERVAL_SECS));
            false
        }
    }
}

fn prompt_until_verified(config: &mut Config, campus_network: CampusNetwork) -> RuntimeState {
    loop {
        if !prompt_and_update_credentials(config) {
            continue;
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
                println!("[!] {}", campus_network.balance_insufficient_tip());
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
    let mut access = Access::new();
    let mut credentials_verified = false;
    let mut state = RuntimeState::Unknown;

    println!("WHUT WiFi 保持器已启动。");

    loop {
        let (campus_network, config) = match access.connected_config() {
            Ok(connection) => connection,
            Err(reason) => {
                if state != RuntimeState::OutsideCampus(reason) {
                    println!("{}", reason.message());
                    state = RuntimeState::OutsideCampus(reason);
                }
                thread::sleep(Duration::from_secs(CAMPUS_CHECK_INTERVAL_SECS));
                continue;
            }
        };
        let network_ok = is_network_ok(config.wired_interface());

        // 每次进程启动后只在已有网络时校验一次统一认证凭据。
        if !credentials_verified && network_ok {
            if !verify_configured_credentials(config) {
                continue;
            }
            credentials_verified = true;
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
                    state = prompt_until_verified(config, campus_network);
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
                        println!("[!] {}", campus_network.balance_insufficient_tip());
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

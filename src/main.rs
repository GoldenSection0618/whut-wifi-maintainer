#[macro_use]
mod interaction;
#[cfg(windows)]
mod windows_worker;

mod access;
mod config;
mod network;
mod portal_auth;
mod unified_auth;

use access::{Access, AccessBlocker, CampusNetwork, Connection};
use config::{Config, Credentials, prompt_credentials, save_changed_credentials};
use network::{ConnectivityFailure, check_connectivity};
use portal_auth::{PortalLoginOutcome, login};
use std::error::Error;
use std::io::IsTerminal;
use std::process::ExitCode;
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum RuntimeState {
    #[default]
    Unknown,
    OutsideCampus(AccessBlocker),
    NetworkOk,
    NetworkUnavailable,
    Authenticated,
    AlreadyOnline,
    BalanceInsufficient,
}

#[derive(Default)]
struct Runtime {
    state: RuntimeState,
    credentials_verified: bool,
}

impl Runtime {
    fn reconnect(
        &mut self,
        config: &mut Config,
        campus_network: CampusNetwork,
        connectivity_failure: &ConnectivityFailure,
        attempt: impl FnOnce(&Config) -> Result<PortalLoginOutcome, Box<dyn Error>>,
        prompt: impl FnOnce(&mut Config, CampusNetwork) -> RuntimeState,
        mut report: impl FnMut(&str),
    ) {
        if self.state != RuntimeState::NetworkUnavailable
            && self.state != RuntimeState::BalanceInsufficient
            && self.state != RuntimeState::Authenticated
        {
            report(&format!("[!] {connectivity_failure}，正在尝试认证。"));
        }

        match attempt(config) {
            Ok(PortalLoginOutcome::Verified { network_ok }) => {
                if let Err(err) = save_changed_credentials(config) {
                    report(&format!("[!] 账号密码可用，但保存失败: {err}"));
                }
                self.state = authenticated_state(network_ok, &mut report);
                self.credentials_verified = true;
            }
            Ok(PortalLoginOutcome::Rejected) => {
                report("[!] 认证服务器拒绝了账号或密码。");
                self.credentials_verified = false;
                self.state = prompt(config, campus_network);
                self.credentials_verified = matches!(
                    self.state,
                    RuntimeState::NetworkOk | RuntimeState::Authenticated
                );
            }
            Ok(PortalLoginOutcome::Inconclusive { network_ok }) => {
                let next_state = if network_ok {
                    RuntimeState::NetworkOk
                } else {
                    RuntimeState::NetworkUnavailable
                };
                if self.state != next_state {
                    report(if network_ok {
                        "[+] 联网复查已通过。"
                    } else {
                        "[!] 门户已响应，但联网复查未通过，稍后重试。"
                    });
                }
                self.state = next_state;
            }
            Ok(PortalLoginOutcome::BalanceInsufficient) => {
                if self.state != RuntimeState::BalanceInsufficient {
                    report(&format!(
                        "[!] {}",
                        campus_network.balance_insufficient_tip()
                    ));
                }
                self.state = RuntimeState::BalanceInsufficient;
            }
            Err(err) => {
                if self.state != RuntimeState::NetworkUnavailable
                    && self.state != RuntimeState::BalanceInsufficient
                {
                    report(&format!("[!] 认证异常: {err}"));
                }
                self.state = RuntimeState::NetworkUnavailable;
            }
        }
    }
}

fn authenticated_state(network_ok: bool, report: &mut impl FnMut(&str)) -> RuntimeState {
    if network_ok {
        report("[+] 认证成功，网络已恢复。");
        RuntimeState::NetworkOk
    } else {
        report("[!] 认证成功，但联网检测尚未通过，稍后重试。");
        RuntimeState::Authenticated
    }
}

fn unconfirmed_state(network_ok: bool, report: &mut impl FnMut(&str)) -> RuntimeState {
    if network_ok {
        report("[!] 当前设备已经在线，无法确认账号密码是否有效；配置未更新。");
        RuntimeState::AlreadyOnline
    } else {
        report("[!] 门户返回成功，但无法确认账号密码或联网状态；配置未更新。");
        RuntimeState::NetworkUnavailable
    }
}

fn prompt_credentials_or_report() -> Option<Credentials> {
    match prompt_credentials() {
        Ok(credentials) => Some(credentials),
        Err(err) => {
            status!("[!] 未更新账号密码: {err}");
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof)
            {
                std::process::exit(1);
            }
            sleep_if_non_interactive();
            None
        }
    }
}

fn replacement_credentials() -> Option<Credentials> {
    #[cfg(windows)]
    if let Some(result) = interaction::window_credentials() {
        return result;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("[!] 账号或密码被明确拒绝；后台运行已退出，请修正配置后重启。");
        std::process::exit(2);
    }
    prompt_credentials_or_report()
}

fn verify_configured_credentials(config: &mut Config) -> bool {
    verify_configured_credentials_with(
        config,
        replacement_credentials,
        |credentials, interface| {
            verify_credentials(&credentials.username, &credentials.password, interface)
        },
        || interaction::wait(Duration::from_secs(CAMPUS_CHECK_INTERVAL_SECS)),
    )
}

fn verify_configured_credentials_with(
    config: &mut Config,
    mut prompt: impl FnMut() -> Option<Credentials>,
    mut verify: impl FnMut(&Credentials, Option<&str>) -> Result<CredentialVerification, Box<dyn Error>>,
    mut wait: impl FnMut(),
) -> bool {
    status!("[*] 当前网络已连接，正在通过统一认证校验账号密码...");
    let mut credentials = Credentials {
        username: config.username.clone(),
        password: config.password.clone(),
    };
    loop {
        match verify(&credentials, config.wired_interface()) {
            Ok(CredentialVerification::Valid) => {
                config.update_credentials(credentials);
                if let Err(err) = save_changed_credentials(config) {
                    status!("[!] 账号密码校验成功，但保存失败: {err}");
                }
                status!("[+] 账号密码校验成功。");
                return true;
            }
            Ok(CredentialVerification::Invalid) => {
                status!("[!] 统一认证拒绝了账号或密码。");
                let Some(candidate) = prompt() else {
                    return false;
                };
                credentials = candidate;
            }
            Ok(CredentialVerification::Inconclusive) => {
                status!("[!] 统一认证未返回明确结果，10 秒后重试。");
                wait();
                return false;
            }
            Err(err) => {
                status!("[!] 无法完成统一认证校验: {err}");
                status!("[!] 10 秒后重试。");
                wait();
                return false;
            }
        }
    }
}

fn prompt_until_verified(config: &mut Config, campus_network: CampusNetwork) -> RuntimeState {
    prompt_until_verified_with(
        config,
        campus_network,
        replacement_credentials,
        |credentials, interface| {
            login(
                &credentials.username,
                &credentials.password,
                true,
                interface,
            )
        },
    )
}

fn prompt_until_verified_with(
    config: &mut Config,
    campus_network: CampusNetwork,
    mut prompt: impl FnMut() -> Option<Credentials>,
    mut attempt: impl FnMut(&Credentials, Option<&str>) -> Result<PortalLoginOutcome, Box<dyn Error>>,
) -> RuntimeState {
    loop {
        let Some(credentials) = prompt() else {
            if interaction::stopping() {
                return RuntimeState::Unknown;
            }
            continue;
        };

        match attempt(&credentials, config.wired_interface()) {
            Ok(PortalLoginOutcome::Verified { network_ok }) => {
                config.update_credentials(credentials);
                if let Err(err) = save_changed_credentials(config) {
                    status!("[!] 账号密码可用，但保存失败: {err}");
                }
                return authenticated_state(network_ok, &mut |message| status!("{message}"));
            }
            Ok(PortalLoginOutcome::Rejected) => {
                status!("[!] 账号密码仍然不正确，请重新输入。");
            }
            Ok(PortalLoginOutcome::Inconclusive { network_ok }) => {
                return unconfirmed_state(network_ok, &mut |message| status!("{message}"));
            }
            Ok(PortalLoginOutcome::BalanceInsufficient) => {
                status!("[!] {}", campus_network.balance_insufficient_tip());
                return RuntimeState::BalanceInsufficient;
            }
            Err(err) => {
                status!("[!] 重试认证异常: {err}");
                return RuntimeState::NetworkUnavailable;
            }
        }
    }
}

fn main() -> ExitCode {
    #[cfg(windows)]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--winui-worker")
    {
        windows_worker::connect();
    }
    run_keepalive()
}

fn run_keepalive() -> ExitCode {
    let mut access = Access::new();
    let mut runtime = Runtime::default();

    status!("WHUT WiFi 保持器已启动。");

    while !interaction::stopping() {
        let (campus_network, config) = match access.connected_config() {
            Connection::Active(network, config) => (network, config),
            #[cfg(windows)]
            Connection::Cancelled => return ExitCode::SUCCESS,
            #[cfg(windows)]
            Connection::ConfigError(error) => {
                interaction::report_error(format_args!("[!] 无法加载账号配置: {error}"));
                return ExitCode::FAILURE;
            }
            Connection::Blocked(reason) => {
                if runtime.state != RuntimeState::OutsideCampus(reason) {
                    status!("{}", reason.message());
                    runtime.state = RuntimeState::OutsideCampus(reason);
                }
                interaction::wait(Duration::from_secs(CAMPUS_CHECK_INTERVAL_SECS));
                continue;
            }
        };
        let connectivity = check_connectivity(config.wired_interface());
        if interaction::stopping() {
            return ExitCode::SUCCESS;
        }
        let network_ok = connectivity.is_ok();

        // 每次进程启动后只在已有网络时校验一次统一认证凭据。
        if !runtime.credentials_verified && network_ok {
            if !verify_configured_credentials(config) {
                continue;
            }
            runtime.credentials_verified = true;
        }

        if network_ok {
            if runtime.state != RuntimeState::NetworkOk {
                status!("[+] 网络正常。");
                runtime.state = RuntimeState::NetworkOk;
            }
        } else if let Err(failure) = connectivity {
            runtime.reconnect(
                config,
                campus_network,
                &failure,
                |config| {
                    login(
                        &config.username,
                        &config.password,
                        false,
                        config.wired_interface(),
                    )
                },
                prompt_until_verified,
                |message| status!("{message}"),
            );
        }

        interaction::wait(Duration::from_secs(CHECK_INTERVAL_SECS));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    fn stored_config() -> (Config, PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "whut-auth-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        fs::write(
            &path,
            "# Deployment settings\nusername='stored-student'\npassword='stored-password'",
        )
        .unwrap();
        let config = config::load_config_from(vec![path.clone()])
            .unwrap()
            .unwrap();
        (config, path)
    }

    fn remove_config(path: PathBuf) {
        fs::remove_file(&path).unwrap();
        fs::remove_dir(path.parent().unwrap()).unwrap();
    }

    fn candidate() -> Credentials {
        Credentials {
            username: "candidate-student".into(),
            password: "candidate-password".into(),
        }
    }

    fn failed_connectivity() -> ConnectivityFailure {
        ConnectivityFailure {
            http: network::ProbeFailure::Timeout,
            https: network::ProbeFailure::Connection,
        }
    }

    #[test]
    fn first_transport_failure_is_reported_without_repeated_messages() {
        let (mut config, path) = stored_config();
        let mut runtime = Runtime::default();
        let mut messages = Vec::new();
        for _ in 0..2 {
            runtime.reconnect(
                &mut config,
                CampusNetwork::Dorm,
                &failed_connectivity(),
                |_| Err("fixture portal timeout".into()),
                |_, _| panic!("transport failures must not prompt for credentials"),
                |message| messages.push(message.to_string()),
            );
        }
        assert_eq!(runtime.state, RuntimeState::NetworkUnavailable);
        assert_eq!(messages.len(), 2);
        assert!(messages[0].contains("正在尝试认证"));
        assert!(messages[0].contains("HTTP：请求超时；HTTPS：连接失败"));
        assert!(messages[1].contains("fixture portal timeout"));
        remove_config(path);
    }

    #[test]
    fn online_recheck_does_not_revalidate_or_rewrite_existing_credentials() {
        for credentials_verified in [false, true] {
            let (mut config, path) = stored_config();
            let original = fs::read_to_string(&path).unwrap();
            let mut runtime = Runtime {
                state: RuntimeState::NetworkUnavailable,
                credentials_verified,
            };
            let mut messages = Vec::new();
            runtime.reconnect(
                &mut config,
                CampusNetwork::Dorm,
                &failed_connectivity(),
                |_| Ok(PortalLoginOutcome::Inconclusive { network_ok: true }),
                |_, _| panic!("a positive connectivity recheck must not request credentials"),
                |message| messages.push(message.to_string()),
            );
            assert_eq!(runtime.state, RuntimeState::NetworkOk);
            assert_eq!(runtime.credentials_verified, credentials_verified);
            assert_eq!(messages, ["[+] 联网复查已通过。"]);
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            remove_config(path);
        }
    }

    #[test]
    fn portal_acceptance_saves_credentials_and_reports_actual_connectivity() {
        for network_ok in [false, true] {
            let (mut config, path) = stored_config();
            config.update_credentials(candidate());
            let mut runtime = Runtime::default();
            let mut messages = Vec::new();
            runtime.reconnect(
                &mut config,
                CampusNetwork::Dorm,
                &failed_connectivity(),
                |_| Ok(PortalLoginOutcome::Verified { network_ok }),
                |_, _| panic!("accepted credentials must not prompt"),
                |message| messages.push(message.to_string()),
            );
            let saved: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(saved.username, "candidate-student");
            assert_eq!(saved.password, "candidate-password");
            assert!(runtime.credentials_verified);
            assert_eq!(
                runtime.state,
                if network_ok {
                    RuntimeState::NetworkOk
                } else {
                    RuntimeState::Authenticated
                }
            );
            assert_eq!(
                messages
                    .iter()
                    .any(|message| message.contains("网络已恢复")),
                network_ok
            );
            remove_config(path);
        }
    }

    #[test]
    fn ambiguous_portal_attempts_leave_stored_credentials_unchanged() {
        for network_ok in [false, true] {
            let (mut config, path) = stored_config();
            let original = fs::read_to_string(&path).unwrap();
            let state = prompt_until_verified_with(
                &mut config,
                CampusNetwork::Dorm,
                || Some(candidate()),
                |credentials, _| {
                    assert_eq!(credentials.username, "candidate-student");
                    Ok(PortalLoginOutcome::Inconclusive { network_ok })
                },
            );
            assert_eq!(
                state,
                if network_ok {
                    RuntimeState::AlreadyOnline
                } else {
                    RuntimeState::NetworkUnavailable
                }
            );
            assert_eq!(config.username, "stored-student");
            assert_eq!(config.password, "stored-password");
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            remove_config(path);
        }
    }

    #[test]
    fn failed_portal_attempts_do_not_replace_existing_credentials() {
        let (mut config, path) = stored_config();
        let original = fs::read_to_string(&path).unwrap();
        let state = prompt_until_verified_with(
            &mut config,
            CampusNetwork::Dorm,
            || Some(candidate()),
            |_, _| Err("fixture portal request failure".into()),
        );
        assert_eq!(state, RuntimeState::NetworkUnavailable);
        assert_eq!(config.username, "stored-student");
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        remove_config(path);
    }

    #[test]
    fn unconfirmed_unified_candidates_are_not_applied_or_saved() {
        for transport_failure in [false, true] {
            let (mut config, path) = stored_config();
            let original = fs::read_to_string(&path).unwrap();
            let mut attempts = Vec::new();
            let mut waits = 0;
            let verified = verify_configured_credentials_with(
                &mut config,
                || Some(candidate()),
                |credentials, _| {
                    attempts.push(credentials.username.clone());
                    if attempts.len() == 1 {
                        Ok(CredentialVerification::Invalid)
                    } else if transport_failure {
                        Err("fixture unified service unavailable".into())
                    } else {
                        Ok(CredentialVerification::Inconclusive)
                    }
                },
                || waits += 1,
            );
            assert!(!verified);
            assert_eq!(attempts, ["stored-student", "candidate-student"]);
            assert_eq!(waits, 1);
            assert_eq!(config.username, "stored-student");
            assert_eq!(config.password, "stored-password");
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            remove_config(path);
        }
    }

    #[test]
    fn confirmed_unified_candidates_are_applied_and_saved() {
        let (mut config, path) = stored_config();
        let mut attempts = 0;
        assert!(verify_configured_credentials_with(
            &mut config,
            || Some(candidate()),
            |_, _| {
                attempts += 1;
                Ok(if attempts == 1 {
                    CredentialVerification::Invalid
                } else {
                    CredentialVerification::Valid
                })
            },
            || panic!("confirmed credentials do not require waiting")
        ));
        assert_eq!(config.username, "candidate-student");
        let saved: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.password, "candidate-password");
        remove_config(path);
    }

    #[test]
    fn verified_unchanged_credentials_do_not_rewrite_config() {
        let (mut config, path) = stored_config();
        let original = fs::read_to_string(&path).unwrap();
        assert!(verify_configured_credentials_with(
            &mut config,
            || panic!("valid credentials must not prompt"),
            |_, _| Ok(CredentialVerification::Valid),
            || panic!("valid credentials do not require waiting"),
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        remove_config(path);
    }

    #[test]
    fn background_credential_rejection_exits_without_reading_input_or_saving() {
        const CHILD_FLAG: &str = "WHUT_TEST_BACKGROUND_REJECTION";
        const CONFIG_PATH: &str = "WHUT_TEST_BACKGROUND_CONFIG";
        if let Some(flow) = std::env::var_os(CHILD_FLAG) {
            let path = PathBuf::from(std::env::var_os(CONFIG_PATH).unwrap());
            let mut config = config::load_config_from(vec![path]).unwrap().unwrap();
            if flow == "portal" {
                Runtime::default().reconnect(
                    &mut config,
                    CampusNetwork::Dorm,
                    &failed_connectivity(),
                    |_| Ok(PortalLoginOutcome::Rejected),
                    |config, network| {
                        prompt_until_verified_with(
                            config,
                            network,
                            replacement_credentials,
                            |_, _| panic!("background rejection must not retry credentials"),
                        )
                    },
                    |message| println!("{message}"),
                );
            } else {
                assert_eq!(flow, "unified");
                verify_configured_credentials_with(
                    &mut config,
                    replacement_credentials,
                    |_, _| Ok(CredentialVerification::Invalid),
                    || panic!("background rejection must exit immediately"),
                );
            }
            panic!("background credential rejection must terminate the process");
        }

        for flow in ["portal", "unified"] {
            let (_, path) = stored_config();
            let original = fs::read_to_string(&path).unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::background_credential_rejection_exits_without_reading_input_or_saving",
                    "--nocapture",
                ])
                .env(CHILD_FLAG, flow)
                .env(CONFIG_PATH, &path)
                // Keep the pipe open: attempting to read input would block.
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("background {flow} rejection did not terminate promptly");
                }
                thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&output.stderr).contains("请修正配置后重启"));
            assert!(!String::from_utf8_lossy(&output.stdout).contains("请输入校园网"));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("stored-password"));
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            remove_config(path);
        }
    }

    #[test]
    fn ambiguous_unified_results_and_transport_errors_still_retry() {
        for transport_error in [false, true] {
            let (mut config, path) = stored_config();
            let original = fs::read_to_string(&path).unwrap();
            let mut waits = 0;
            assert!(!verify_configured_credentials_with(
                &mut config,
                || panic!("unconfirmed credentials must not request replacement input"),
                |_, _| {
                    if transport_error {
                        Err("fixture request unavailable".into())
                    } else {
                        Ok(CredentialVerification::Inconclusive)
                    }
                },
                || waits += 1,
            ));
            assert_eq!(waits, 1);
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            remove_config(path);
        }
    }

    #[test]
    fn closed_input_exits_instead_of_reprompting() {
        const CHILD_FLAG: &str = "WHUT_TEST_CLOSED_INPUT";
        if std::env::var_os(CHILD_FLAG).is_some() {
            prompt_credentials_or_report();
            panic!("closed input should terminate the process");
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::closed_input_exits_instead_of_reprompting",
                "--nocapture",
            ])
            .env(CHILD_FLAG, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("closed input did not terminate promptly");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stdout).contains("账号输入已结束"));
    }
}

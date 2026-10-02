#[cfg(windows)]
mod wifi;
#[cfg(not(windows))]
mod wired;

use crate::config::Config;

#[derive(Clone, Copy)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum CampusNetwork {
    Wlan,
    Dorm,
    Isp,
    #[cfg(not(windows))]
    Wired,
}

impl CampusNetwork {
    pub fn balance_insufficient_tip(self) -> &'static str {
        match self {
            Self::Dorm => "WHUT-DORM 余额不足，请到 selfaaa.whut.edu.cn 或 cwsf.whut.edu.cn 充值。",
            Self::Isp => "WHUT-ISP 余额不足，请通过对应运营商渠道充值。",
            Self::Wlan => "WHUT-WLAN 不收费，认证服务器返回了异常计费结果。",
            // 有线接入无法判断线路类型，提示保持中性。
            #[cfg(not(windows))]
            Self::Wired => {
                "校园网余额不足，请按当前线路类型，通过学校或对应运营商的渠道查询并充值。"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessBlocker {
    #[cfg(windows)]
    NotCampusWifi,
    #[cfg(not(windows))]
    NotEnabled,
    #[cfg(not(windows))]
    NoDefaultRoute,
    #[cfg(not(windows))]
    PortalUnreachable,
}

impl AccessBlocker {
    pub fn message(self) -> &'static str {
        match self {
            #[cfg(windows)]
            Self::NotCampusWifi => {
                "[*] 当前未接入校园 Wi-Fi，等待连接 WHUT-WLAN、WHUT-DORM 或 WHUT-ISP。"
            }
            #[cfg(not(windows))]
            Self::NotEnabled => {
                "[*] 有线模式未开启：请在 config.toml 中设置 wired = true 并用 wired_interface 绑定 WAN 接口（如 eth0）后再启动。"
            }
            #[cfg(not(windows))]
            Self::NoDefaultRoute => "[*] 绑定接口暂无可用的 IPv4 默认路由，等待接入校园网。",
            #[cfg(not(windows))]
            Self::PortalUnreachable => {
                "[*] 绑定接口已有默认路由，但无法访问校园网认证门户，等待接入 WHUT 校园网。"
            }
        }
    }
}

pub struct Access {
    #[cfg(windows)]
    config: Option<Config>,
    #[cfg(not(windows))]
    config: Config,
}

impl Access {
    pub fn new() -> Self {
        #[cfg(windows)]
        {
            use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

            // WinRT 网络接口在后续轮询中复用，只在启动时初始化一次。
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleOutputCP(65001);
                windows_sys::Win32::System::Console::SetConsoleCP(65001);
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
            }
            Self { config: None }
        }
        #[cfg(not(windows))]
        {
            let config = crate::config::load_wired_config().unwrap_or_else(|error| {
                eprintln!("[!] {error}");
                std::process::exit(1);
            });
            Self { config }
        }
    }

    /// Windows 连接校园 Wi-Fi 后才读取配置；有线模式启动时已加载配置。
    pub fn connected_config(&mut self) -> Result<(CampusNetwork, &mut Config), AccessBlocker> {
        #[cfg(windows)]
        {
            let network = wifi::current_campus_network().ok_or(AccessBlocker::NotCampusWifi)?;
            let config = self
                .config
                .get_or_insert_with(crate::config::load_or_prompt_config);
            Ok((network, config))
        }
        #[cfg(not(windows))]
        {
            let network = wired::current_campus_network(&self.config)?;
            Ok((network, &mut self.config))
        }
    }
}

#[cfg(windows)]
use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation};
#[cfg(windows)]
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

#[derive(Clone, Copy)]
pub enum CampusWifi {
    Wlan,
    Dorm,
    Isp,
}

#[cfg(windows)]
pub fn initialize_windows_runtime() {
    // WinRT 网络接口在后续轮询中复用，因此只在进程启动时初始化一次。
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

#[cfg(not(windows))]
pub fn initialize_windows_runtime() {}

fn campus_wifi(ssid: &str) -> Option<CampusWifi> {
    match ssid {
        "WHUT-WLAN" => Some(CampusWifi::Wlan),
        "WHUT-DORM" => Some(CampusWifi::Dorm),
        "WHUT-ISP" => Some(CampusWifi::Isp),
        _ => None,
    }
}

#[cfg(windows)]
pub fn current_campus_wifi() -> Option<CampusWifi> {
    NetworkInformation::GetConnectionProfiles()
        .ok()
        .into_iter()
        .flatten()
        // 此列表也包含已保存但未连接的网络，必须排除无连通性的历史配置。
        .filter(|profile| {
            matches!(
                profile.GetNetworkConnectivityLevel(),
                Ok(level) if level != NetworkConnectivityLevel::None
            )
        })
        .filter_map(|profile| profile.WlanConnectionProfileDetails().ok())
        .filter_map(|details| details.GetConnectedSsid().ok())
        .find_map(|ssid| campus_wifi(&ssid.to_string()))
}

#[cfg(not(windows))]
pub fn current_campus_wifi() -> Option<CampusWifi> {
    None
}

pub fn balance_insufficient_tip(campus_wifi: CampusWifi) -> &'static str {
    match campus_wifi {
        CampusWifi::Dorm => {
            "WHUT-DORM 余额不足，请到 selfaaa.whut.edu.cn 或 cwsf.whut.edu.cn 充值。"
        }
        CampusWifi::Isp => "WHUT-ISP 余额不足，请通过对应运营商渠道充值。",
        CampusWifi::Wlan => "WHUT-WLAN 不收费，认证服务器返回了异常计费结果。",
    }
}

#[cfg(test)]
mod tests {
    use super::campus_wifi;

    #[test]
    fn recognizes_campus_wifi_ssids() {
        assert!(campus_wifi("WHUT-WLAN").is_some());
        assert!(campus_wifi("WHUT-DORM").is_some());
        assert!(campus_wifi("WHUT-ISP").is_some());
        assert!(campus_wifi("WHUT-WLAN-Guest").is_none());
        assert!(campus_wifi("OtherWiFi").is_none());
    }
}

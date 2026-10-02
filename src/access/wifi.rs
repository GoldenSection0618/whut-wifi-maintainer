use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation};

use super::CampusNetwork;

fn campus_wifi(ssid: &str) -> Option<CampusNetwork> {
    match ssid {
        "WHUT-WLAN" => Some(CampusNetwork::Wlan),
        "WHUT-DORM" => Some(CampusNetwork::Dorm),
        "WHUT-ISP" => Some(CampusNetwork::Isp),
        _ => None,
    }
}

pub(super) fn current_campus_network() -> Option<CampusNetwork> {
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

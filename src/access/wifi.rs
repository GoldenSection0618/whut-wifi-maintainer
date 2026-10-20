use windows::Networking::Connectivity::{ConnectionProfileFilter, NetworkInformation};

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
    let filter = ConnectionProfileFilter::new().ok()?;
    filter.SetIsConnected(true).ok()?;
    filter.SetIsWlanConnectionProfile(true).ok()?;
    // 查询当前关联的 Wi-Fi；无需已能访问互联网，也不遍历历史网络判断是否连接。
    NetworkInformation::FindConnectionProfilesAsync(&filter)
        .ok()?
        .get()
        .ok()?
        .into_iter()
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

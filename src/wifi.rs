#[cfg(windows)]
use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation};
#[cfg(windows)]
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

#[derive(Clone, Copy)]
#[allow(dead_code)] // 各平台仅构造部分变体：Windows 为 Wi-Fi SSID，Linux 为有线接入。
pub enum CampusWifi {
    Wlan,
    Dorm,
    Isp,
    /// 有线接入校园网（Linux/OpenWrt 路由器等场景）。
    #[cfg(not(windows))]
    Wired,
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

#[cfg(any(windows, test))]
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
    // Linux/OpenWrt：路由器等设备通过有线接入校园网，无法像 Windows 那样
    // 枚举 Wi-Fi SSID。先确认默认路由存在（WAN 已获取地址），再探测校园网
    // 认证门户，避免在家庭网络等非校园环境下向固定内网地址提交账号密码。
    if !has_default_route() {
        return None;
    }

    campus_portal_reachable().then_some(CampusWifi::Wired)
}

#[cfg(not(windows))]
pub(crate) fn has_default_route() -> bool {
    let Ok(content) = std::fs::read_to_string("/proc/net/route") else {
        return false;
    };

    has_default_route_in(&content)
}

// 解析 /proc/net/route 内容：Destination 为 00000000 且 Flags 含 RTF_UP(0x1)
// 的行即为默认路由。点对点链路（PPPoE 等）的 Gateway 可以为 0，不能据此排除。
#[cfg(any(not(windows), test))]
fn has_default_route_in(content: &str) -> bool {
    content.lines().skip(1).any(|line| {
        let mut fields = line.split_whitespace();
        let (_iface, Some(destination), Some(_gateway), Some(flags)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return false;
        };

        destination == "00000000"
            && u32::from_str_radix(flags, 16).is_ok_and(|flags| flags & 0x1 != 0)
    })
}

#[cfg(not(windows))]
fn campus_portal_reachable() -> bool {
    // 探测校园网认证门户：仅当返回合法的 CSRF token 才视为处于校园网，
    // 防止在其他网络环境下误向 172.30.21.100 提交认证请求。
    use reqwest::blocking::Client;
    use std::time::Duration;

    use crate::portal_auth::CSRF_TOKEN_URL;

    let Ok(client) = Client::builder().timeout(Duration::from_secs(3)).build() else {
        return false;
    };

    client
        .get(CSRF_TOKEN_URL)
        .send()
        .ok()
        .filter(|resp| resp.status().is_success())
        .and_then(|resp| resp.text().ok())
        .is_some_and(|body| portal_response_has_csrf_token(&body))
}

// 仅当门户返回非空 CSRF token 才视为处于校园网；
// 缺失字段、空字符串、非法 JSON 一律视为不在校园网。
#[cfg(any(not(windows), test))]
fn portal_response_has_csrf_token(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|json| {
            json.get("csrf_token")
                .and_then(serde_json::Value::as_str)
                .map(|token| !token.is_empty())
        })
        .unwrap_or(false)
}

pub fn balance_insufficient_tip(campus_wifi: CampusWifi) -> &'static str {
    match campus_wifi {
        CampusWifi::Dorm => {
            "WHUT-DORM 余额不足，请到 selfaaa.whut.edu.cn 或 cwsf.whut.edu.cn 充值。"
        }
        CampusWifi::Isp => "WHUT-ISP 余额不足，请通过对应运营商渠道充值。",
        CampusWifi::Wlan => "WHUT-WLAN 不收费，认证服务器返回了异常计费结果。",
        #[cfg(not(windows))]
        CampusWifi::Wired => {
            "有线校园网余额不足，请到 selfaaa.whut.edu.cn 或 cwsf.whut.edu.cn 充值。"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{campus_wifi, has_default_route_in, portal_response_has_csrf_token};

    #[test]
    fn recognizes_campus_wifi_ssids() {
        assert!(campus_wifi("WHUT-WLAN").is_some());
        assert!(campus_wifi("WHUT-DORM").is_some());
        assert!(campus_wifi("WHUT-ISP").is_some());
        assert!(campus_wifi("WHUT-WLAN-Guest").is_none());
        assert!(campus_wifi("OtherWiFi").is_none());
    }

    // /proc/net/route 的表头
    const ROUTE_HEADER: &str =
        "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n";

    #[test]
    fn detects_normal_default_route() {
        let table = format!(
            "{ROUTE_HEADER}eth0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
             eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n"
        );
        assert!(has_default_route_in(&table));
    }

    #[test]
    fn detects_zero_gateway_default_route() {
        // PPPoE 等点对点链路：Gateway 为 0 的默认路由同样有效。
        let table =
            format!("{ROUTE_HEADER}ppp0\t00000000\t00000000\t0003\t0\t0\t0\t00000000\t0\t0\t0\n");
        assert!(has_default_route_in(&table));
    }

    #[test]
    fn rejects_route_table_without_usable_default_route() {
        // 只有普通网段路由，没有默认路由
        let no_default =
            format!("{ROUTE_HEADER}eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n");
        assert!(!has_default_route_in(&no_default));

        // 有默认路由但接口未 UP（Flags 不含 RTF_UP）
        let down =
            format!("{ROUTE_HEADER}eth0\t00000000\t0101A8C0\t0000\t0\t0\t0\t00000000\t0\t0\t0\n");
        assert!(!has_default_route_in(&down));

        // 空表 / 只有表头
        assert!(!has_default_route_in(ROUTE_HEADER));
        assert!(!has_default_route_in(""));
    }

    #[test]
    fn accepts_valid_csrf_token_response() {
        assert!(portal_response_has_csrf_token(
            r#"{"csrf_token":"jw_8-3wDi2wj4Ayi7bYYmGQXbtk="}"#
        ));
    }

    #[test]
    fn rejects_missing_or_empty_csrf_token() {
        // 空 token 不可用，必须拒绝
        assert!(!portal_response_has_csrf_token(r#"{"csrf_token":""}"#));
        // 字段缺失
        assert!(!portal_response_has_csrf_token(r#"{"code":0}"#));
        // 非法 JSON
        assert!(!portal_response_has_csrf_token("not json"));
    }
}

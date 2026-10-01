use super::{AccessBlocker, CampusNetwork};
use crate::config::Config;

pub(super) fn current_campus_network(config: &Config) -> Result<CampusNetwork, AccessBlocker> {
    // 有线模式必须由用户显式开启并绑定指定接口：HTTP 门户返回 token
    // 并不能证明服务器身份，不能把"有默认路由 + 能访问某内网地址"
    // 当作已接入校园网的充分证据。
    let Some(iface) = config.wired_interface() else {
        return Err(AccessBlocker::NotEnabled);
    };

    if !has_default_route_on_iface(iface) {
        return Err(AccessBlocker::NoDefaultRoute);
    }

    if campus_portal_reachable(iface) {
        Ok(CampusNetwork::Wired)
    } else {
        Err(AccessBlocker::PortalUnreachable)
    }
}

fn has_default_route_on_iface(iface: &str) -> bool {
    let Ok(content) = std::fs::read_to_string("/proc/net/route") else {
        return false;
    };

    has_default_route_on(&content, iface)
}

// 默认路由的 Destination 和 Mask 均为 0，需含 RTF_UP(0x1) 且不含 RTF_REJECT(0x200)。
// 点对点链路（PPPoE 等）的 Gateway 可以为 0，不能据此排除。
fn has_default_route_on(content: &str, iface: &str) -> bool {
    content.lines().skip(1).any(|line| {
        let mut fields = line.split_whitespace();
        let (Some(line_iface), Some(destination), Some(_gateway), Some(flags)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        let Some(mask) = fields.nth(3) else {
            return false;
        };

        line_iface == iface
            && destination == "00000000"
            && mask == "00000000"
            && u32::from_str_radix(flags, 16)
                .is_ok_and(|flags| flags & 0x1 != 0 && flags & 0x200 == 0)
    })
}

fn campus_portal_reachable(iface: &str) -> bool {
    // 检查所选接口上的门户可达性；HTTP token 本身不能证明服务器身份。
    use std::time::Duration;

    use crate::network::client_builder;
    use crate::portal_auth::CSRF_TOKEN_URL;

    let Ok(client) = client_builder(Some(iface))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
    else {
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

// 门户响应必须包含非空 CSRF token；这只是可达性检查，不是身份认证。
fn portal_response_has_csrf_token(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|json| {
            json.get("csrf_token")
                .and_then(serde_json::Value::as_str)
                .map(|token| !token.trim().is_empty())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{has_default_route_on, portal_response_has_csrf_token};

    // /proc/net/route 的表头
    const ROUTE_HEADER: &str =
        "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n";

    #[test]
    fn detects_normal_default_route() {
        let table = format!(
            "{ROUTE_HEADER}eth0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
             eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n"
        );
        assert!(has_default_route_on(&table, "eth0"));
    }

    #[test]
    fn detects_zero_gateway_default_route() {
        // PPPoE 等点对点链路：Gateway 为 0 的默认路由同样有效。
        let table =
            format!("{ROUTE_HEADER}ppp0\t00000000\t00000000\t0003\t0\t0\t0\t00000000\t0\t0\t0\n");
        assert!(has_default_route_on(&table, "ppp0"));
    }

    #[test]
    fn default_route_must_be_on_bound_interface() {
        // 默认路由存在于其他接口时，绑定接口不算有默认路由。
        let table =
            format!("{ROUTE_HEADER}eth0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n");
        assert!(has_default_route_on(&table, "eth0"));
        assert!(!has_default_route_on(&table, "eth1"));
        assert!(!has_default_route_on(&table, "pppoe-wan"));
    }

    #[test]
    fn rejects_route_table_without_usable_default_route() {
        // 只有普通网段路由，没有默认路由
        let no_default =
            format!("{ROUTE_HEADER}eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n");
        assert!(!has_default_route_on(&no_default, "eth0"));

        // 有默认路由但接口未 UP（Flags 不含 RTF_UP）
        let down =
            format!("{ROUTE_HEADER}eth0\t00000000\t0101A8C0\t0000\t0\t0\t0\t00000000\t0\t0\t0\n");
        assert!(!has_default_route_on(&down, "eth0"));

        // 空表 / 只有表头
        assert!(!has_default_route_on(ROUTE_HEADER, "eth0"));
        assert!(!has_default_route_on("", "eth0"));
    }

    #[test]
    fn rejects_non_default_masks_reject_routes_and_malformed_rows() {
        for row in [
            "eth0 00000000 0101A8C0 0003 0 0 0 00000080 0 0 0",
            "eth0 00000000 0101A8C0 0201 0 0 0 00000000 0 0 0",
            "eth0 00000000 0101A8C0 invalid 0 0 0 00000000 0 0 0",
            "eth0 00000000 0101A8C0 0003",
            "eth0",
        ] {
            assert!(!has_default_route_on(
                &format!("{ROUTE_HEADER}{row}\n"),
                "eth0"
            ));
        }
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
        assert!(!portal_response_has_csrf_token(r#"{"csrf_token":"   "}"#));
        assert!(!portal_response_has_csrf_token(r#"{"csrf_token":null}"#));
        assert!(!portal_response_has_csrf_token(r#"{"csrf_token":42}"#));
        // 字段缺失
        assert!(!portal_response_has_csrf_token(r#"{"code":0}"#));
        // 非法 JSON
        assert!(!portal_response_has_csrf_token("not json"));
    }
}

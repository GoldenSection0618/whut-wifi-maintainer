use reqwest::blocking::Client;
use reqwest::header::{HeaderValue, REFERER};
use serde_json::Value;

use crate::network::RequestError;
use crate::protocol::{CSRF_TOKEN_URL, LOGIN_URL, REDIRECT_URL, credentials_rejected};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalLoginOutcome {
    Accepted,
    CredentialsRejected,
    BalanceInsufficient,
    Inconclusive,
}

struct PortalContext {
    nas_id: String,
    switch_ip: String,
    user_ipv4: String,
    user_mac: String,
}

fn query_value(url: &reqwest::Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(name, value)| name == key && !value.is_empty())
        .map(|(_, value)| value.into_owned())
}

fn valid_mac(value: &str) -> bool {
    let mut parts = value.split(':');
    (0..6).all(|_| {
        parts.next().is_some_and(|part| {
            part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }) && parts.next().is_none()
}

fn extract_portal_context(
    final_url: &str,
    login_url: &str,
    fallback_nas_id: &str,
) -> Option<PortalContext> {
    let url = reqwest::Url::parse(final_url).ok()?;
    let portal = reqwest::Url::parse(login_url).ok()?;
    if url.origin() != portal.origin() {
        return None;
    }
    let nas_id = query_value(&url, "nasId")
        .filter(|value| value.bytes().all(|byte| byte.is_ascii_digit()))
        .unwrap_or_else(|| fallback_nas_id.to_owned());
    let ipv4 = |key| {
        query_value(&url, key)
            .filter(|value| value.parse::<std::net::Ipv4Addr>().is_ok())
            .unwrap_or_default()
    };
    let user_mac = query_value(&url, "mac")
        .filter(|value| valid_mac(value))
        .unwrap_or_default();
    Some(PortalContext {
        nas_id,
        switch_ip: ipv4("switchip"),
        user_ipv4: ipv4("wlanuserip"),
        user_mac,
    })
}

pub fn csrf_token(json: &Value) -> Result<&str, RequestError> {
    json.get("csrf_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .ok_or(RequestError::Protocol)
}

fn classify_response(result: &Value) -> Result<PortalLoginOutcome, RequestError> {
    if result.get("code").is_some_and(|code| !code.is_i64()) {
        return Err(RequestError::Protocol);
    }
    let code = result.get("code").and_then(Value::as_i64);
    let message = result
        .get("authMsg")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| result.get("msg").and_then(Value::as_str))
        .unwrap_or("");
    if message.contains("余额不足") {
        return Ok(PortalLoginOutcome::BalanceInsufficient);
    }

    let success_message = result.get("msg").and_then(Value::as_str) == Some("success");
    let success = code == Some(0) || (code.is_none() && success_message);
    if success {
        if credentials_rejected(message) {
            return Err(RequestError::Protocol);
        }
        return Ok(PortalLoginOutcome::Accepted);
    }
    if success_message {
        return Err(RequestError::Protocol);
    }
    if credentials_rejected(message) {
        return Ok(PortalLoginOutcome::CredentialsRejected);
    }
    if code.is_some() || !message.is_empty() {
        return Ok(PortalLoginOutcome::Inconclusive);
    }
    Err(RequestError::Protocol)
}

pub fn login(
    client: &Client,
    discovery: &Client,
    username: &str,
    password: &str,
    fallback_nas_id: &str,
) -> Result<PortalLoginOutcome, RequestError> {
    login_at(
        client,
        discovery,
        username,
        password,
        fallback_nas_id,
        &Endpoints {
            redirect: REDIRECT_URL,
            csrf: CSRF_TOKEN_URL,
            login: LOGIN_URL,
        },
    )
}

struct Endpoints<'a> {
    redirect: &'a str,
    csrf: &'a str,
    login: &'a str,
}

fn login_at(
    client: &Client,
    discovery: &Client,
    username: &str,
    password: &str,
    fallback_nas_id: &str,
    endpoints: &Endpoints<'_>,
) -> Result<PortalLoginOutcome, RequestError> {
    // Only a redirect to the known portal may supply nasId or Referer.
    let redirect = discovery
        .get(endpoints.redirect)
        .send()
        .ok()
        .and_then(|response| {
            if !response.status().is_success() {
                return None;
            }
            extract_portal_context(response.url().as_str(), endpoints.login, fallback_nas_id)
                .map(|context| (response.url().clone(), context))
        });
    let fallback_context = PortalContext {
        nas_id: fallback_nas_id.to_owned(),
        switch_ip: String::new(),
        user_ipv4: String::new(),
        user_mac: String::new(),
    };
    let context = redirect
        .as_ref()
        .map(|(_, context)| context)
        .unwrap_or(&fallback_context);
    let json: Value = client
        .get(endpoints.csrf)
        .send()?
        .error_for_status()?
        .json()?;
    let token = HeaderValue::from_str(csrf_token(&json)?).map_err(|_| RequestError::Protocol)?;
    let mut request = client
        .post(endpoints.login)
        .header("X-CSRF-Token", token)
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[
            ("username", username),
            ("password", password),
            ("switchip", context.switch_ip.as_str()),
            ("nasId", context.nas_id.as_str()),
            ("userIpv4", context.user_ipv4.as_str()),
            ("userMac", context.user_mac.as_str()),
            ("captcha", ""),
            ("captchaId", ""),
        ]);
    if let Some((url, _)) = redirect {
        request = request.header(REFERER, url.as_str());
    }
    let response = request.send()?;
    // Never follow redirects from a credential-bearing request.
    if response.status().is_redirection() {
        return Err(RequestError::Protocol);
    }
    let result: Value = response.error_for_status()?.json()?;
    classify_response(&result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{response, serve};
    use serde_json::json;

    #[test]
    fn production_portal_client_never_forwards_credentials_on_307() {
        use crate::config::{Config, Credentials};
        use crate::network::HttpClients;
        use std::net::TcpListener;
        let destination = TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let redirect = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{}/capture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            destination.local_addr().unwrap()
        );
        let (url, server) = serve(vec![
            response("200 OK", "discovery"),
            response("200 OK", "{\"csrf_token\":\"token\"}"),
            redirect,
        ]);
        let config = Config::new(Credentials {
            username: "student".into(),
            password: "secret".into(),
        });
        let clients = HttpClients::new(&config).unwrap();
        let result = login_at(
            &clients.portal,
            &clients.discovery,
            "student",
            "secret",
            "52",
            &Endpoints {
                redirect: &url,
                csrf: &url,
                login: &url,
            },
        );
        assert!(matches!(result, Err(RequestError::Protocol)), "{result:?}");
        let requests = server.join().unwrap();
        assert!(requests[2].contains("password=secret"));
        assert_eq!(
            destination.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn redirected_discovery_session_is_reused_for_csrf_and_login() {
        use crate::config::{Config, Credentials};
        use crate::network::HttpClients;

        let discovery = "HTTP/1.1 200 OK\r\nSet-Cookie: portal_session=discovered; Path=/\r\nContent-Length: 9\r\nConnection: close\r\n\r\ndiscovery".to_owned();
        let (portal_url, portal) = serve(vec![
            discovery,
            response("200 OK", "{\"csrf_token\":\"token\"}"),
            response("200 OK", "{\"code\":0}"),
        ]);
        let discovery_url = format!(
            "{portal_url}?switchip=10.1.0.1&nasId=77&wlanuserip=10.1.2.3&mac=aa:bb:cc:dd:ee:ff"
        );
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: {discovery_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (entry_url, entry) = serve(vec![redirect]);
        let config = Config::new(Credentials {
            username: "student".into(),
            password: "secret".into(),
        });
        let clients = HttpClients::new(&config).unwrap();

        assert_eq!(
            login_at(
                &clients.portal,
                &clients.discovery,
                "student",
                "secret",
                "52",
                &Endpoints {
                    redirect: &entry_url,
                    csrf: &portal_url,
                    login: &portal_url,
                },
            )
            .unwrap(),
            PortalLoginOutcome::Accepted
        );

        assert_eq!(entry.join().unwrap().len(), 1);
        let requests = portal.join().unwrap();
        assert!(
            !requests[0]
                .to_ascii_lowercase()
                .contains("cookie: portal_session=discovered")
        );
        for request in &requests[1..] {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("cookie: portal_session=discovered")
            );
        }
        let login = &requests[2];
        let form = login
            .split_once("\r\n\r\n")
            .expect("login request must contain a form body")
            .1;
        for field in [
            "switchip=10.1.0.1",
            "nasId=77",
            "userIpv4=10.1.2.3",
            "userMac=aa%3Abb%3Acc%3Add%3Aee%3Aff",
            "captcha=",
            "captchaId=",
        ] {
            assert!(form.contains(field), "missing {field} in login form");
        }
    }

    #[test]
    fn unsuccessful_portal_landing_uses_fallback_context_without_referer() {
        use crate::config::{Config, Credentials};
        use crate::network::HttpClients;

        let (portal_url, portal) = serve(vec![
            response("500 Internal Server Error", "failed landing"),
            response("200 OK", "{\"csrf_token\":\"token\"}"),
            response("200 OK", "{\"code\":0}"),
        ]);
        let discovery_url = format!(
            "{portal_url}?switchip=10.1.0.1&nasId=77&wlanuserip=10.1.2.3&mac=aa:bb:cc:dd:ee:ff"
        );
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: {discovery_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (entry_url, entry) = serve(vec![redirect]);
        let config = Config::new(Credentials {
            username: "student".into(),
            password: "secret".into(),
        });
        let clients = HttpClients::new(&config).unwrap();

        assert_eq!(
            login_at(
                &clients.portal,
                &clients.discovery,
                "student",
                "secret",
                "52",
                &Endpoints {
                    redirect: &entry_url,
                    csrf: &portal_url,
                    login: &portal_url,
                },
            )
            .unwrap(),
            PortalLoginOutcome::Accepted
        );

        assert_eq!(entry.join().unwrap().len(), 1);
        let requests = portal.join().unwrap();
        let login = &requests[2];
        assert!(!login.to_ascii_lowercase().contains("\r\nreferer:"));
        let form = login.split_once("\r\n\r\n").unwrap().1;
        assert!(form.contains("nasId=52"));
        assert!(form.contains("switchip=&"));
        assert!(form.contains("userIpv4=&"));
        assert!(form.contains("userMac=&"));
        assert!(!form.contains("10.1.0.1"));
        assert!(!form.contains("10.1.2.3"));
    }

    #[test]
    fn malformed_csrf_never_submits_credentials() {
        let (url, server) = serve(vec![
            response("200 OK", "discovery"),
            response("200 OK", "{\"csrf_token\":\" \"}"),
        ]);
        let client = Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        let result = login_at(
            &client,
            &client,
            "student",
            "secret",
            "52",
            &Endpoints {
                redirect: &url,
                csrf: &url,
                login: &url,
            },
        );
        assert!(matches!(result, Err(RequestError::Protocol)));
        assert!(
            server
                .join()
                .unwrap()
                .iter()
                .all(|request| request.starts_with("GET "))
        );
    }

    #[test]
    fn real_http_exchange_returns_accepted_not_online() {
        let (url, server) = serve(vec![
            response("200 OK", "discovery"),
            response("200 OK", "{\"csrf_token\":\"token\"}"),
            response("200 OK", "{\"code\":0}"),
        ]);
        let client = Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        assert_eq!(
            login_at(
                &client,
                &client,
                "student",
                "secret",
                "77",
                &Endpoints {
                    redirect: &url,
                    csrf: &url,
                    login: &url
                }
            )
            .unwrap(),
            PortalLoginOutcome::Accepted
        );
        let requests = server.join().unwrap();
        assert!(requests[2].starts_with("POST "));
        assert!(requests[2].contains("nasId=77"));
        assert!(requests[2].contains("password=secret"));
    }

    #[test]
    fn accepts_only_nonempty_csrf() {
        assert_eq!(csrf_token(&json!({"csrf_token":"token"})).unwrap(), "token");
        for value in [
            json!({}),
            json!({"csrf_token":null}),
            json!({"csrf_token":"  "}),
            json!({"csrf_token":42}),
        ] {
            assert!(csrf_token(&value).is_err());
        }
    }

    #[test]
    fn classifies_auth_results_without_claiming_connectivity() {
        assert_eq!(
            classify_response(&json!({"code":0})).unwrap(),
            PortalLoginOutcome::Accepted
        );
        assert_eq!(
            classify_response(&json!({"authMsg":"密码错误"})).unwrap(),
            PortalLoginOutcome::CredentialsRejected
        );
        assert_eq!(
            classify_response(&json!({"authMsg":"余额不足"})).unwrap(),
            PortalLoginOutcome::BalanceInsufficient
        );
        assert_eq!(
            classify_response(&json!({"msg":"服务器繁忙"})).unwrap(),
            PortalLoginOutcome::Inconclusive
        );
        assert!(classify_response(&json!({})).is_err());
        for response in [
            json!({"code":0,"authMsg":"密码错误"}),
            json!({"code":1,"msg":"success"}),
            json!({"code":"0","msg":"success"}),
        ] {
            assert!(classify_response(&response).is_err());
        }
    }

    #[test]
    fn redirect_context_is_only_taken_from_known_portal() {
        let context = extract_portal_context(
            "http://172.30.21.100/?switchip=10.0.0.1&nasId=123&wlanuserip=10.0.0.2&mac=aa:bb:cc:dd:ee:ff",
            LOGIN_URL,
            "52",
        )
        .unwrap();
        assert_eq!(context.nas_id, "123");
        assert_eq!(context.switch_ip, "10.0.0.1");
        assert_eq!(context.user_ipv4, "10.0.0.2");
        assert_eq!(context.user_mac, "aa:bb:cc:dd:ee:ff");
        assert!(extract_portal_context("http://example.com/?nasId=123", LOGIN_URL, "52").is_none());
        let invalid = extract_portal_context(
            "http://172.30.21.100/?switchip=bad&nasId=bad&wlanuserip=bad&mac=bad",
            LOGIN_URL,
            "52",
        )
        .unwrap();
        assert_eq!(invalid.nas_id, "52");
        assert!(invalid.switch_ip.is_empty());
        assert!(invalid.user_ipv4.is_empty());
        assert!(invalid.user_mac.is_empty());
    }
}

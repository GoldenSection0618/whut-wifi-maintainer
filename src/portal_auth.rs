use reqwest::blocking::Client;
use reqwest::cookie::Jar;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use reqwest::redirect::Policy;
use serde_json::Value;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use crate::network::{USER_AGENT_VALUE, client_builder, is_network_ok};
use crate::unified_auth::is_credential_error;

const REDIRECT_URL: &str = "http://www.msftconnecttest.com/redirect";
const CSRF_TOKEN_URL: &str = "http://172.30.21.100/api/csrf-token";
const LOGIN_URL: &str = "http://172.30.21.100/api/account/login";

struct AuthContext {
    referer: Option<String>,
    nas_id: String,
    from_redirect: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PortalLoginOutcome {
    Verified { network_ok: bool },
    Rejected,
    Inconclusive { network_ok: bool },
    BalanceInsufficient,
}

struct AuthClients {
    discovery: Client,
    portal: Client,
}

fn build_clients(wired_interface: Option<&str>) -> Result<AuthClients, Box<dyn Error>> {
    // 两个客户端共享原有会话；只有门户发现请求允许自动跟随重定向。
    let cookies = Arc::new(Jar::default());
    let builder = || {
        client_builder(wired_interface)
            .cookie_provider(Arc::clone(&cookies))
            .user_agent(USER_AGENT_VALUE)
    };
    Ok(AuthClients {
        discovery: builder().build()?,
        portal: builder().redirect(Policy::none()).build()?,
    })
}

fn extract_nas_id(final_url: &str) -> String {
    reqwest::Url::parse(final_url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "nasId")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or_else(|| "52".to_string())
}

fn context_from_response(url: &reqwest::Url, status: reqwest::StatusCode) -> AuthContext {
    let portal = reqwest::Url::parse(LOGIN_URL).expect("valid portal URL");
    let from_redirect = status.is_success() && url.origin() == portal.origin();
    AuthContext {
        referer: from_redirect.then(|| url.to_string()),
        nas_id: if from_redirect {
            extract_nas_id(url.as_str())
        } else {
            "52".into()
        },
        from_redirect,
    }
}

fn auth_context(client: &Client, verbose: bool, redirect_url: &str) -> AuthContext {
    // 门户重定向携带当前接入点的 nasId，并可作为后续请求的 Referer。
    match client
        .get(redirect_url)
        .timeout(Duration::from_secs(10))
        .send()
    {
        Ok(resp) => {
            let context = context_from_response(resp.url(), resp.status());
            if verbose {
                if context.from_redirect {
                    println!("[*] 已进入校园网认证页面。");
                } else {
                    println!("[*] 未进入校园网认证页面，使用默认 nasId = 52 继续尝试。");
                }
            }
            context
        }
        Err(err) => {
            if verbose {
                println!("[!] 重定向检查失败: {err}");
                println!("[*] 使用默认 nasId = 52 继续尝试");
            }

            AuthContext {
                referer: None,
                nas_id: "52".to_string(),
                from_redirect: false,
            }
        }
    }
}

fn response_message(result: &Value) -> Option<&str> {
    result
        .get("authMsg")
        .and_then(Value::as_str)
        .filter(|msg| !msg.is_empty())
        .or_else(|| {
            result
                .get("msg")
                .and_then(Value::as_str)
                .filter(|msg| !msg.is_empty())
        })
}

fn csrf_token(response: &Value) -> Result<&str, Box<dyn Error>> {
    response
        .get("csrf_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| "认证门户未提供有效的 CSRF token".into())
}

fn classify_login_response(
    result: &Value,
    from_redirect: bool,
    network_check: impl FnOnce() -> bool,
) -> Result<PortalLoginOutcome, Box<dyn Error>> {
    if result.get("code").is_some_and(|code| !code.is_i64()) {
        return Err("认证门户返回的 code 类型不正确".into());
    }
    let code = result.get("code").and_then(Value::as_i64);
    let message = response_message(result);
    if message.is_some_and(|message| message.contains("余额不足")) {
        return Ok(PortalLoginOutcome::BalanceInsufficient);
    }

    let success_message = result.get("msg").and_then(Value::as_str) == Some("success");
    let success = code == Some(0) || (code.is_none() && success_message);
    if success {
        if message.is_some_and(is_credential_error) {
            return Err("认证门户返回了互相矛盾的认证结果".into());
        }
        let network_ok = network_check();
        return Ok(if from_redirect {
            PortalLoginOutcome::Verified { network_ok }
        } else {
            PortalLoginOutcome::Inconclusive { network_ok }
        });
    }
    if success_message {
        return Err("认证门户返回了互相矛盾的认证结果".into());
    }
    if message.is_some_and(is_credential_error) {
        return Ok(PortalLoginOutcome::Rejected);
    }
    Err(format!("认证门户返回了未识别的认证结果（code: {code:?}）").into())
}

pub fn login(
    username: &str,
    password: &str,
    verbose: bool,
    wired_interface: Option<&str>,
) -> Result<PortalLoginOutcome, Box<dyn Error>> {
    let clients = build_clients(wired_interface)?;
    login_at(
        &clients,
        username,
        password,
        verbose,
        &Endpoints {
            redirect: REDIRECT_URL,
            csrf: CSRF_TOKEN_URL,
            login: LOGIN_URL,
        },
        || is_network_ok(wired_interface),
        crate::access::ensure_campus_network,
    )
}

struct Endpoints<'a> {
    redirect: &'a str,
    csrf: &'a str,
    login: &'a str,
}

fn login_at(
    clients: &AuthClients,
    username: &str,
    password: &str,
    verbose: bool,
    endpoints: &Endpoints<'_>,
    network_check: impl FnOnce() -> bool,
    mut ensure_access: impl FnMut() -> Result<(), crate::access::CampusAccessChanged>,
) -> Result<PortalLoginOutcome, Box<dyn Error>> {
    ensure_access()?;
    let auth_context = auth_context(&clients.discovery, verbose, endpoints.redirect);

    ensure_access()?;
    let csrf_response = clients
        .portal
        .get(endpoints.csrf)
        .timeout(Duration::from_secs(5))
        .send()?
        .error_for_status()?;
    // 保留有线门户探测原有的成功状态要求，重定向响应不能提供有效 token。
    if !csrf_response.status().is_success() {
        return Err("认证门户 CSRF 接口未返回成功状态".into());
    }
    let csrf_json: Value = csrf_response.json()?;
    let csrf_token = csrf_token(&csrf_json)?;

    if verbose {
        println!("[*] 已获取 CSRF token");
    }

    let mut headers = HeaderMap::new();
    headers.insert("X-CSRF-Token", HeaderValue::from_str(csrf_token)?);
    headers.insert(
        "X-Requested-With",
        HeaderValue::from_static("XMLHttpRequest"),
    );
    if let Some(referer) = &auth_context.referer {
        headers.insert(REFERER, HeaderValue::from_str(referer)?);
    }
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded; charset=UTF-8"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));

    ensure_access()?;
    let login_resp = clients
        .portal
        .post(endpoints.login)
        .headers(headers)
        .form(&[
            ("username", username),
            ("password", password),
            ("nasId", auth_context.nas_id.as_str()),
        ])
        .timeout(Duration::from_secs(10))
        .send()?;
    // 307/308 会保留 POST 正文；禁止重发，并将所有登录重定向报告为协议异常。
    if login_resp.status().is_redirection() {
        return Err("认证门户登录接口返回了重定向，已停止请求".into());
    }
    let result: Value = login_resp.error_for_status()?.json()?;

    let outcome = classify_login_response(&result, auth_context.from_redirect, network_check)?;
    ensure_access()?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{response, serve};
    use serde_json::json;
    use std::net::TcpListener;

    #[test]
    fn login_redirects_never_forward_credentials_or_probe_connectivity() {
        for status in [301, 302, 303, 307, 308] {
            let destination = TcpListener::bind("127.0.0.1:0").unwrap();
            destination.set_nonblocking(true).unwrap();
            let redirect = format!(
                "HTTP/1.1 {status} Redirect\r\nLocation: http://{}/capture?ticket=private-fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                destination.local_addr().unwrap()
            );
            let (url, server) = serve(vec![
                response("discovery"),
                response(r#"{"csrf_token":"fixture-token"}"#),
                redirect,
            ]);
            let clients = build_clients(None).unwrap();
            let error = login_at(
                &clients,
                "student",
                "fixture-password",
                false,
                &Endpoints {
                    redirect: &url,
                    csrf: &url,
                    login: &url,
                },
                || panic!("redirected authentication must not check connectivity"),
                || Ok(()),
            )
            .unwrap_err();
            let requests = server.join().unwrap();
            assert_eq!(
                error.to_string(),
                "认证门户登录接口返回了重定向，已停止请求"
            );
            assert!(requests[2].starts_with("POST "));
            assert!(requests[2].contains("username=student"));
            assert!(requests[2].contains("password=fixture-password"));
            assert_eq!(
                destination.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "login redirect {status} must not reach its destination"
            );
        }
    }

    #[test]
    fn discovery_redirects_preserve_cookies_and_normal_login_responses() {
        let (url, server) = serve(vec![
            "HTTP/1.1 302 Found\r\nLocation: /portal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nSet-Cookie: campus_session=fixture; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            response(r#"{"csrf_token":"fixture-token"}"#),
            response(r#"{"code":0}"#),
        ]);
        let clients = build_clients(None).unwrap();
        let mut checked_connectivity = false;
        let outcome = login_at(
            &clients,
            "student",
            "fixture-password",
            false,
            &Endpoints {
                redirect: &url,
                csrf: &url,
                login: &url,
            },
            || {
                checked_connectivity = true;
                true
            },
            || Ok(()),
        )
        .unwrap();
        let requests = server.join().unwrap();
        assert!(requests[1].starts_with("GET /portal "));
        for request in &requests[2..] {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("cookie: campus_session=fixture")
            );
        }
        assert!(checked_connectivity);
        // A loopback discovery page is not a recognized campus portal.
        assert_eq!(
            outcome,
            PortalLoginOutcome::Inconclusive { network_ok: true }
        );
    }

    #[test]
    fn invalid_portal_responses_never_submit_credentials() {
        let mut responses = vec![
            response("not json"),
            response(r#"{"code":0}"#),
            response(r#"{"csrf_token":null}"#),
            response(r#"{"csrf_token":42}"#),
            response(r#"{"csrf_token":""}"#),
            response(r#"{"csrf_token":"   "}"#),
        ];
        for status in [302, 503] {
            responses.push(response(r#"{"csrf_token":"fixture-token"}"#).replacen(
                "200 OK",
                &format!("{status} Unavailable"),
                1,
            ));
        }
        for csrf_response in responses {
            let login_destination = TcpListener::bind("127.0.0.1:0").unwrap();
            login_destination.set_nonblocking(true).unwrap();
            let login_url = format!("http://{}/login", login_destination.local_addr().unwrap());
            let (url, server) = serve(vec![response("discovery"), csrf_response]);
            let clients = build_clients(None).unwrap();
            let outcome = login_at(
                &clients,
                "student",
                "fixture-password",
                false,
                &Endpoints {
                    redirect: &url,
                    csrf: &url,
                    login: &login_url,
                },
                || panic!("invalid portal responses must not check connectivity"),
                || Ok(()),
            );
            let requests = server.join().unwrap();
            assert!(outcome.is_err(), "invalid portal response was accepted");
            assert!(requests.iter().all(|request| request.starts_with("GET ")));
            assert_eq!(
                login_destination.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "invalid portal response must not trigger a credential POST"
            );
        }
    }

    #[test]
    fn leaving_campus_during_portal_setup_never_submits_credentials() {
        for cancelled_check in 0..3 {
            let responses = vec![
                response("discovery"),
                response(r#"{"csrf_token":"fixture-token"}"#),
            ];
            let (url, server) = serve(responses.into_iter().take(cancelled_check).collect());
            let destination = TcpListener::bind("127.0.0.1:0").unwrap();
            destination.set_nonblocking(true).unwrap();
            let login_url = format!("http://{}/login", destination.local_addr().unwrap());
            let mut checks = 0;
            let error = login_at(
                &build_clients(None).unwrap(),
                "student",
                "fixture-password",
                false,
                &Endpoints {
                    redirect: &url,
                    csrf: &url,
                    login: &login_url,
                },
                || panic!("cancelled authentication must not check external connectivity"),
                || {
                    if checks == cancelled_check {
                        return Err(crate::access::CampusAccessChanged);
                    }
                    checks += 1;
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(error.is::<crate::access::CampusAccessChanged>());
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), cancelled_check);
            assert!(requests.iter().all(|request| request.starts_with("GET ")));
            assert_eq!(
                destination.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }

    #[test]
    fn leaving_campus_during_recheck_discards_the_portal_result() {
        let (url, server) = serve(vec![
            response("discovery"),
            response(r#"{"csrf_token":"fixture-token"}"#),
            response(r#"{"code":0}"#),
        ]);
        let connected = std::cell::Cell::new(true);
        let error = login_at(
            &build_clients(None).unwrap(),
            "student",
            "fixture-password",
            false,
            &Endpoints {
                redirect: &url,
                csrf: &url,
                login: &url,
            },
            || {
                connected.set(false);
                true
            },
            || {
                if connected.get() {
                    Ok(())
                } else {
                    Err(crate::access::CampusAccessChanged)
                }
            },
        )
        .unwrap_err();
        assert!(error.is::<crate::access::CampusAccessChanged>());
        assert_eq!(server.join().unwrap().len(), 3);
    }

    #[test]
    fn token_errors_are_not_credential_rejections() {
        for response in [
            json!({}),
            json!({"csrf_token":null}),
            json!({"csrf_token":42}),
            json!({"csrf_token":""}),
            json!({"csrf_token":"   "}),
        ] {
            assert!(csrf_token(&response).is_err());
        }
        let response = json!({"csrf_token":"fixture-token"});
        assert_eq!(csrf_token(&response).unwrap(), "fixture-token");
    }

    #[test]
    fn a_completed_connectivity_request_is_not_a_portal_redirect() {
        for url in [REDIRECT_URL, "http://other.invalid/login"] {
            let context =
                context_from_response(&reqwest::Url::parse(url).unwrap(), reqwest::StatusCode::OK);
            assert!(!context.from_redirect);
            assert!(context.referer.is_none());
        }
        let portal_url = reqwest::Url::parse("http://172.30.21.100/?nasId=64").unwrap();
        let context = context_from_response(&portal_url, reqwest::StatusCode::OK);
        assert!(context.from_redirect);
        assert_eq!(context.nas_id, "64");
        assert!(
            !context_from_response(&portal_url, reqwest::StatusCode::INTERNAL_SERVER_ERROR)
                .from_redirect
        );
    }

    #[test]
    fn accepted_requests_without_a_portal_redirect_remain_inconclusive() {
        for network_ok in [false, true] {
            assert_eq!(
                classify_login_response(&json!({"code":0}), false, || network_ok).unwrap(),
                PortalLoginOutcome::Inconclusive { network_ok }
            );
            assert_eq!(
                classify_login_response(&json!({"code":0}), true, || network_ok).unwrap(),
                PortalLoginOutcome::Verified { network_ok }
            );
        }
    }

    #[test]
    fn unrelated_failures_and_conflicting_responses_do_not_reject_credentials() {
        for response in [
            json!({"code":1,"msg":"服务器忙"}),
            json!({}),
            json!({"code":1,"msg":"success"}),
            json!({"code":0,"authMsg":"密码错误"}),
            json!({"code":"1","msg":"success"}),
            json!({"private_field":"fixture-secret"}),
        ] {
            let error =
                classify_login_response(&response, true, || panic!("must not check connectivity"))
                    .unwrap_err();
            assert!(!error.to_string().contains("fixture-secret"));
        }
        assert_eq!(
            classify_login_response(&json!({"code":1,"authMsg":"密码错误"}), true, || panic!(
                "must not check connectivity"
            ))
            .unwrap(),
            PortalLoginOutcome::Rejected
        );
        assert_eq!(
            classify_login_response(&json!({"code":0,"authMsg":"余额不足"}), true, || panic!(
                "must not check connectivity"
            ))
            .unwrap(),
            PortalLoginOutcome::BalanceInsufficient
        );
    }
}

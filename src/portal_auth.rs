use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use serde_json::Value;
use std::error::Error;
use std::time::Duration;

use crate::network::{USER_AGENT_VALUE, client_builder, is_network_ok};
use crate::unified_auth::is_credential_error;

const REDIRECT_URL: &str = "http://www.msftconnecttest.com/redirect";
pub(crate) const CSRF_TOKEN_URL: &str = "http://172.30.21.100/api/csrf-token";
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

fn build_client(wired_interface: Option<&str>) -> Result<Client, Box<dyn Error>> {
    Ok(client_builder(wired_interface)
        .cookie_store(true)
        .user_agent(USER_AGENT_VALUE)
        .build()?)
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

fn auth_context(client: &Client, verbose: bool) -> AuthContext {
    // 门户重定向携带当前接入点的 nasId，并可作为后续请求的 Referer。
    match client
        .get(REDIRECT_URL)
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
    let client = build_client(wired_interface)?;
    let auth_context = auth_context(&client, verbose);

    let csrf_json: Value = client
        .get(CSRF_TOKEN_URL)
        .timeout(Duration::from_secs(5))
        .send()?
        .error_for_status()?
        .json()?;
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

    let login_resp = client
        .post(LOGIN_URL)
        .headers(headers)
        .form(&[
            ("username", username),
            ("password", password),
            ("nasId", auth_context.nas_id.as_str()),
        ])
        .timeout(Duration::from_secs(10))
        .send()?
        .error_for_status()?;
    let result: Value = login_resp.json()?;

    classify_login_response(&result, auth_context.from_redirect, || {
        is_network_ok(wired_interface)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

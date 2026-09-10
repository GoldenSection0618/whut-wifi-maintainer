use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use serde_json::Value;
use std::error::Error;
use std::time::Duration;

use crate::USER_AGENT_VALUE;
use crate::network::is_network_ok;

const REDIRECT_URL: &str = "http://www.msftconnecttest.com/redirect";
const CSRF_TOKEN_URL: &str = "http://172.30.21.100/api/csrf-token";
const LOGIN_URL: &str = "http://172.30.21.100/api/account/login";

struct AuthContext {
    referer: Option<String>,
    nas_id: String,
    from_redirect: bool,
}

pub enum PortalLoginOutcome {
    Verified,
    Rejected,
    Inconclusive,
    BalanceInsufficient,
}

fn build_client() -> Result<Client, Box<dyn Error>> {
    Ok(Client::builder()
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

fn auth_context(client: &Client, verbose: bool) -> AuthContext {
    // 门户重定向携带当前接入点的 nasId，并可作为后续请求的 Referer。
    match client
        .get(REDIRECT_URL)
        .timeout(Duration::from_secs(10))
        .send()
    {
        Ok(resp) => {
            let final_url = resp.url().to_string();
            let nas_id = extract_nas_id(&final_url);

            if verbose {
                println!("[*] 已重定向到认证页面: {final_url}");
                println!("[*] nasId = {nas_id}");
            }

            AuthContext {
                referer: Some(final_url),
                nas_id,
                from_redirect: true,
            }
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

pub fn login(
    username: &str,
    password: &str,
    verbose: bool,
) -> Result<PortalLoginOutcome, Box<dyn Error>> {
    let client = build_client()?;
    let auth_context = auth_context(&client, verbose);

    let csrf_json: Value = client
        .get(CSRF_TOKEN_URL)
        .timeout(Duration::from_secs(5))
        .send()?
        .error_for_status()?
        .json()?;
    let Some(csrf_token) = csrf_json.get("csrf_token").and_then(Value::as_str) else {
        println!("[!] 获取 CSRF token 失败");
        return Ok(PortalLoginOutcome::Rejected);
    };

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

    if verbose {
        println!("[*] 登录响应: {result}");
    }

    if result.get("code").and_then(Value::as_i64) == Some(0)
        || result.get("msg").and_then(Value::as_str) == Some("success")
    {
        if !auth_context.from_redirect && is_network_ok() {
            Ok(PortalLoginOutcome::Inconclusive)
        } else {
            if verbose {
                println!("[+] 登录成功");
            }
            Ok(PortalLoginOutcome::Verified)
        }
    } else {
        if let Some(message) = response_message(&result) {
            // 计费失败不代表凭据错误，不能触发重新输入账号密码。
            if message.contains("余额不足") {
                return Ok(PortalLoginOutcome::BalanceInsufficient);
            }
            println!("[-] 登录失败: {message}");
        } else {
            println!("[-] 登录失败: {result}");
        }
        Ok(PortalLoginOutcome::Rejected)
    }
}

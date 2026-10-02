use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rand::rngs::OsRng;
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::LOCATION;
use reqwest::redirect::Policy;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use serde::Deserialize;
use std::error::Error;
use std::time::Duration;

use crate::network::{USER_AGENT_VALUE, client_builder};

const UNIFIED_LOGIN_URL: &str =
    "https://zhlgd.whut.edu.cn/tpass/login?service=https%3A%2F%2Fzhlgd.whut.edu.cn%2Ftp_up%2F";
const UNIFIED_RSA_URL: &str = "https://zhlgd.whut.edu.cn/tpass/rsa?skipWechat=true";
const UNIFIED_SERVICE_URL: &str = "https://zhlgd.whut.edu.cn/tp_up/";

#[derive(Deserialize)]
struct RsaKeyResponse {
    #[serde(rename = "publicKey")]
    public_key: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CredentialVerification {
    Valid,
    Invalid,
    Inconclusive,
}

fn build_client(wired_interface: Option<&str>) -> Result<Client, Box<dyn Error>> {
    Ok(client_builder(wired_interface)
        .cookie_store(true)
        // 成功与否由登录接口的重定向目标判定，不能自动跟随。
        .redirect(Policy::none())
        .user_agent(USER_AGENT_VALUE)
        .build()?)
}

fn hidden_input_value(html: &str, id: &str) -> Result<Option<String>, Box<dyn Error>> {
    let input_re = Regex::new(r#"(?is)<input\b[^>]*>"#)?;
    let id_re = Regex::new(&format!(
        r#"(?i)(?:^|\s)id\s*=\s*[\"']{}[\"']"#,
        regex::escape(id)
    ))?;
    let value_re = Regex::new(r#"(?i)(?:^|\s)value\s*=\s*[\"']([^\"']*)[\"']"#)?;

    Ok(input_re.find_iter(html).find_map(|input| {
        let input = input.as_str();
        id_re
            .is_match(input)
            .then(|| value_re.captures(input))
            .flatten()
            .and_then(|captures| captures.get(1))
            .map(|value| value.as_str().to_string())
    }))
}

fn unified_error_message(html: &str) -> Result<Option<String>, Box<dyn Error>> {
    let error_re =
        Regex::new(r#"(?is)<[^>]*\sid\s*=\s*[\"']errormsghide[\"'][^>]*>(.*?)</[^>]+>"#)?;
    let tag_re = Regex::new(r#"(?is)<[^>]+>"#)?;

    Ok(error_re
        .captures(html)
        .and_then(|captures| captures.get(1))
        .map(|message| tag_re.replace_all(message.as_str(), "").trim().to_string())
        .filter(|message| !message.is_empty()))
}

fn encrypt(public_key: &RsaPublicKey, value: &str) -> Result<String, Box<dyn Error>> {
    let encrypted = public_key.encrypt(&mut OsRng, Pkcs1v15Encrypt, value.as_bytes())?;
    Ok(BASE64_STANDARD.encode(encrypted))
}

pub(crate) fn is_credential_error(message: &str) -> bool {
    if ["验证码", "锁定", "过期", "维护"]
        .iter()
        .any(|reason| message.contains(reason))
    {
        return false;
    }
    [
        "密码错误",
        "密码不正确",
        "账号不存在",
        "帐号不存在",
        "用户名不存在",
        "用户不存在",
    ]
    .iter()
    .any(|reason| message.contains(reason))
}

fn is_service_redirect(response_url: &reqwest::Url, location: &str) -> bool {
    let service = reqwest::Url::parse(UNIFIED_SERVICE_URL).expect("valid unified service URL");
    response_url.join(location).is_ok_and(|destination| {
        destination.origin() == service.origin() && destination.path().starts_with(service.path())
    })
}

fn classify_login_page(html: &str) -> Result<CredentialVerification, Box<dyn Error>> {
    match unified_error_message(html)? {
        Some(message) if is_credential_error(&message) => Ok(CredentialVerification::Invalid),
        Some(_) => Err("统一认证返回了其他错误，无法判断账号密码是否有效".into()),
        None => Ok(CredentialVerification::Inconclusive),
    }
}

pub fn verify_credentials(
    username: &str,
    password: &str,
    wired_interface: Option<&str>,
) -> Result<CredentialVerification, Box<dyn Error>> {
    let client = build_client(wired_interface)?;

    // 登录页会建立会话 Cookie，并提供本次提交所需的 lt。
    let login_page = client
        .get(UNIFIED_LOGIN_URL)
        .timeout(Duration::from_secs(10))
        .send()?
        .error_for_status()?;
    let login_html = login_page.text()?;
    let lt = hidden_input_value(&login_html, "lt")?
        .filter(|value| !value.is_empty())
        .ok_or("统一认证登录页未提供 lt 字段")?;

    let key_response: RsaKeyResponse = client
        .post(UNIFIED_RSA_URL)
        .timeout(Duration::from_secs(10))
        .send()?
        .error_for_status()?
        .json()?;
    let public_key_der = BASE64_STANDARD.decode(key_response.public_key)?;
    let public_key = RsaPublicKey::from_public_key_der(&public_key_der)?;

    // 与浏览器一致：使用服务端当前公钥加密账号和密码后再提交。
    let encrypted_username = encrypt(&public_key, username)?;
    let encrypted_password = encrypt(&public_key, password)?;
    let login_response = client
        .post(UNIFIED_LOGIN_URL)
        .form(&[
            ("ua", ""),
            ("visitorId", ""),
            ("rsa", ""),
            ("ul", encrypted_username.as_str()),
            ("pl", encrypted_password.as_str()),
            ("lt", lt.as_str()),
            ("execution", "e1s1"),
            ("_eventId", "submit"),
        ])
        .timeout(Duration::from_secs(10))
        .send()?;

    if login_response.status().is_redirection() {
        let location = login_response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or("统一认证成功重定向缺少 Location 响应头")?;

        if is_service_redirect(login_response.url(), location) {
            return Ok(CredentialVerification::Valid);
        }

        return Err("统一认证重定向到了未预期地址".into());
    }

    if !login_response.status().is_success() {
        return Err(format!("统一认证返回 HTTP {}", login_response.status()).into());
    }

    let response_html = login_response.text()?;
    let verification = classify_login_page(&response_html)?;
    if verification == CredentialVerification::Invalid {
        println!("[-] 统一认证返回账号或密码错误。");
    }
    Ok(verification)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_lt_from_login_page() {
        let html = r#"<input type="hidden" id="lt" name="lt" value="LT-123-tpass">"#;

        assert_eq!(
            hidden_input_value(html, "lt").unwrap(),
            Some("LT-123-tpass".to_string())
        );
    }

    #[test]
    fn extracts_unified_login_error() {
        let html = r#"<span id="errormsghide">密码错误</span>"#;

        assert_eq!(
            unified_error_message(html).unwrap(),
            Some("密码错误".to_string())
        );
    }

    #[test]
    fn data_attributes_are_not_login_fields() {
        let html = r#"<input data-id="lt" value="wrong">
                      <input id="lt" data-value="wrong" value="LT-correct">"#;
        assert_eq!(
            hidden_input_value(html, "lt").unwrap().as_deref(),
            Some("LT-correct")
        );
        assert!(
            unified_error_message(r#"<span data-id="errormsghide">密码错误</span>"#)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn other_unified_errors_do_not_reject_credentials() {
        for message in [
            "验证码错误",
            "会话已过期",
            "系统维护中",
            "账号已锁定",
            "密码错误次数过多，账号已锁定",
        ] {
            let html = format!(r#"<span id="errormsghide">{message}</span>"#);
            assert!(classify_login_page(&html).is_err());
        }
        assert_eq!(
            classify_login_page(r#"<span id="errormsghide">用户名或密码错误</span>"#).unwrap(),
            CredentialVerification::Invalid
        );
        assert_eq!(
            classify_login_page("<html>unknown response</html>").unwrap(),
            CredentialVerification::Inconclusive
        );
    }

    #[test]
    fn service_redirects_use_resolved_urls() {
        let response_url = reqwest::Url::parse(UNIFIED_LOGIN_URL).unwrap();
        assert!(is_service_redirect(
            &response_url,
            "https://zhlgd.whut.edu.cn/tp_up/?ticket=fixture"
        ));
        assert!(is_service_redirect(&response_url, "/tp_up/?ticket=fixture"));
        for location in [
            "https://zhlgd.whut.edu.cn/tp_up/../tpass/login",
            "https://zhlgd.whut.edu.cn/tp_up/%2e%2e/tpass/login",
            "https://zhlgd.whut.edu.cn.evil.invalid/tp_up/",
            "http://zhlgd.whut.edu.cn/tp_up/",
        ] {
            assert!(!is_service_redirect(&response_url, location));
        }
    }
}

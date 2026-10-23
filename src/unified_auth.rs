use crate::network::RequestError;
use crate::protocol::{
    UNIFIED_LOGIN_URL, UNIFIED_RSA_URL, UNIFIED_SERVICE_URL, credentials_rejected,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rand::rngs::OsRng;
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::LOCATION;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use serde::Deserialize;
use std::error::Error;

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

fn is_service_redirect(response_url: &reqwest::Url, location: &str) -> bool {
    let service = reqwest::Url::parse(UNIFIED_SERVICE_URL).expect("constant URL");
    response_url.join(location).is_ok_and(|destination| {
        destination.origin() == service.origin() && destination.path().starts_with(service.path())
    })
}

fn classify_login_page(html: &str) -> Result<CredentialVerification, RequestError> {
    match unified_error_message(html).map_err(|_| RequestError::Protocol)? {
        Some(message) if credentials_rejected(&message) => Ok(CredentialVerification::Invalid),
        Some(_) => Err(RequestError::Protocol),
        None => Ok(CredentialVerification::Inconclusive),
    }
}

pub fn verify_credentials(
    client: &Client,
    username: &str,
    password: &str,
) -> Result<CredentialVerification, RequestError> {
    // 登录页会建立会话 Cookie，并提供本次提交所需的 lt。
    let login_page = client.get(UNIFIED_LOGIN_URL).send()?.error_for_status()?;
    let login_html = login_page.text()?;
    let lt = hidden_input_value(&login_html, "lt")
        .map_err(|_| RequestError::Protocol)?
        .filter(|value| !value.is_empty())
        .ok_or(RequestError::Protocol)?;

    let key_response: RsaKeyResponse = client
        .post(UNIFIED_RSA_URL)
        .send()?
        .error_for_status()?
        .json()?;
    let public_key_der = BASE64_STANDARD
        .decode(key_response.public_key)
        .map_err(|_| RequestError::Protocol)?;
    let public_key =
        RsaPublicKey::from_public_key_der(&public_key_der).map_err(|_| RequestError::Protocol)?;

    // 与浏览器一致：使用服务端当前公钥加密账号和密码后再提交。
    let encrypted_username = encrypt(&public_key, username).map_err(|_| RequestError::Protocol)?;
    let encrypted_password = encrypt(&public_key, password).map_err(|_| RequestError::Protocol)?;
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
        .send()?;

    if login_response.status().is_redirection() {
        let location = login_response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestError::Protocol)?;

        if is_service_redirect(login_response.url(), location) {
            return Ok(CredentialVerification::Valid);
        }

        return Err(RequestError::Protocol);
    }

    if !login_response.status().is_success() {
        return Err(RequestError::Status(login_response.status().as_u16()));
    }

    classify_login_page(&login_response.text()?)
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
        assert_eq!(
            hidden_input_value(r#"<input data-id="lt" value="wrong">"#, "lt").unwrap(),
            None
        );
    }

    #[test]
    fn other_unified_errors_are_not_credential_rejections() {
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

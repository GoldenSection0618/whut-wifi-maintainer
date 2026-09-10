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

use crate::USER_AGENT_VALUE;

const UNIFIED_LOGIN_URL: &str =
    "https://zhlgd.whut.edu.cn/tpass/login?service=https%3A%2F%2Fzhlgd.whut.edu.cn%2Ftp_up%2F";
const UNIFIED_RSA_URL: &str = "https://zhlgd.whut.edu.cn/tpass/rsa?skipWechat=true";
const UNIFIED_SERVICE_URL: &str = "https://zhlgd.whut.edu.cn/tp_up/";

#[derive(Deserialize)]
struct RsaKeyResponse {
    #[serde(rename = "publicKey")]
    public_key: String,
}

pub enum CredentialVerification {
    Valid,
    Invalid,
    Inconclusive,
}

fn build_client() -> Result<Client, Box<dyn Error>> {
    Ok(Client::builder()
        .cookie_store(true)
        // 成功与否由登录接口的重定向目标判定，不能自动跟随。
        .redirect(Policy::none())
        .user_agent(USER_AGENT_VALUE)
        .build()?)
}

fn hidden_input_value(html: &str, id: &str) -> Result<Option<String>, Box<dyn Error>> {
    let input_re = Regex::new(r#"(?is)<input\b[^>]*>"#)?;
    let id_re = Regex::new(&format!(
        r#"(?i)\bid\s*=\s*[\"']{}[\"']"#,
        regex::escape(id)
    ))?;
    let value_re = Regex::new(r#"(?i)\bvalue\s*=\s*[\"']([^\"']*)[\"']"#)?;

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
        Regex::new(r#"(?is)<[^>]*\bid\s*=\s*[\"']errormsghide[\"'][^>]*>(.*?)</[^>]+>"#)?;
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

pub fn verify_credentials(
    username: &str,
    password: &str,
) -> Result<CredentialVerification, Box<dyn Error>> {
    let client = build_client()?;

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

        if location.starts_with(UNIFIED_SERVICE_URL) {
            return Ok(CredentialVerification::Valid);
        }

        return Err(format!("统一认证重定向到了未预期地址: {location}").into());
    }

    if !login_response.status().is_success() {
        return Err(format!("统一认证返回 HTTP {}", login_response.status()).into());
    }

    let response_html = login_response.text()?;
    if let Some(message) = unified_error_message(&response_html)? {
        println!("[-] 统一认证失败: {message}");
        return Ok(CredentialVerification::Invalid);
    }

    Ok(CredentialVerification::Inconclusive)
}

#[cfg(test)]
mod tests {
    use super::{hidden_input_value, unified_error_message};

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
}

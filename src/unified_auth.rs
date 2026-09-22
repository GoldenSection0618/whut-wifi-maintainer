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

pub enum CredentialVerification {
    Valid,
    Invalid,
    Inconclusive,
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

        if reqwest::Url::parse(location).is_ok_and(|url| {
            let expected = reqwest::Url::parse(UNIFIED_SERVICE_URL).expect("constant URL");
            url.origin() == expected.origin() && url.path() == expected.path()
        }) {
            return Ok(CredentialVerification::Valid);
        }

        return Err(RequestError::Protocol);
    }

    if !login_response.status().is_success() {
        return Err(RequestError::Status(login_response.status().as_u16()));
    }

    let response_html = login_response.text()?;
    if unified_error_message(&response_html)
        .map_err(|_| RequestError::Protocol)?
        .is_some_and(|message| credentials_rejected(&message))
    {
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

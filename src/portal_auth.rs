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

fn extract_nas_id(final_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(final_url).ok()?;
    let portal = reqwest::Url::parse(LOGIN_URL).ok()?;
    if url.origin() != portal.origin() {
        return None;
    }
    url.query_pairs()
        .find(|(key, value)| {
            key == "nasId" && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
        })
        .map(|(_, value)| value.into_owned())
}

pub fn csrf_token(json: &Value) -> Result<&str, RequestError> {
    json.get("csrf_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .ok_or(RequestError::Protocol)
}

pub fn reachable(client: &Client) -> Result<(), RequestError> {
    let json: Value = client
        .get(CSRF_TOKEN_URL)
        .send()?
        .error_for_status()?
        .json()?;
    csrf_token(&json).map(|_| ())
}

fn classify_response(result: &Value) -> PortalLoginOutcome {
    if result.get("code").and_then(Value::as_i64) == Some(0)
        || result.get("msg").and_then(Value::as_str) == Some("success")
    {
        return PortalLoginOutcome::Accepted;
    }
    let message = result
        .get("authMsg")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| result.get("msg").and_then(Value::as_str))
        .unwrap_or("");
    if message.contains("余额不足") {
        PortalLoginOutcome::BalanceInsufficient
    } else if credentials_rejected(message) {
        PortalLoginOutcome::CredentialsRejected
    } else {
        PortalLoginOutcome::Inconclusive
    }
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
            extract_nas_id(response.url().as_str()).map(|nas| (response.url().clone(), nas))
        });
    let nas_id = redirect
        .as_ref()
        .map(|(_, nas)| nas.as_str())
        .unwrap_or(fallback_nas_id);
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
            ("nasId", nas_id),
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
    Ok(classify_response(&result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{response, serve};
    use serde_json::json;

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
            classify_response(&json!({"code":0})),
            PortalLoginOutcome::Accepted
        );
        assert_eq!(
            classify_response(&json!({"authMsg":"密码错误"})),
            PortalLoginOutcome::CredentialsRejected
        );
        assert_eq!(
            classify_response(&json!({"authMsg":"余额不足"})),
            PortalLoginOutcome::BalanceInsufficient
        );
        assert_eq!(
            classify_response(&json!({"msg":"服务器繁忙"})),
            PortalLoginOutcome::Inconclusive
        );
        assert_eq!(
            classify_response(&json!({})),
            PortalLoginOutcome::Inconclusive
        );
    }

    #[test]
    fn nas_id_is_only_taken_from_known_portal() {
        assert_eq!(
            extract_nas_id("http://172.30.21.100/?nasId=123"),
            Some("123".into())
        );
        assert_eq!(extract_nas_id("http://example.com/?nasId=123"), None);
        assert_eq!(extract_nas_id("http://172.30.21.100/?nasId=bad"), None);
    }
}

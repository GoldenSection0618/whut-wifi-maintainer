use reqwest::blocking::{Client, ClientBuilder};

use crate::config::{Config, MonitorSettings};
use crate::protocol::USER_AGENT;
use std::io::Read;
use std::time::Duration;

#[derive(thiserror::Error, Debug)]
pub enum RequestError {
    #[error("请求超时")]
    Timeout,
    #[error("连接或传输失败")]
    Transport,
    #[error("服务器返回 HTTP {0}")]
    Status(u16),
    #[error("服务器响应格式异常")]
    Protocol,
}

impl From<reqwest::Error> for RequestError {
    fn from(error: reqwest::Error) -> Self {
        // reqwest errors may contain URLs with authentication tokens.
        if error.is_timeout() {
            Self::Timeout
        } else if let Some(status) = error.status() {
            Self::Status(status.as_u16())
        } else if error.is_decode() {
            Self::Protocol
        } else {
            Self::Transport
        }
    }
}

pub struct HttpClients {
    pub probe: Client,
    pub portal: Client,
    pub discovery: Client,
}

impl HttpClients {
    pub fn new(config: &Config) -> Result<Self, RequestError> {
        let make = || configured_builder(config);
        Ok(Self {
            probe: make().redirect(reqwest::redirect::Policy::none()).build()?,
            portal: make()
                .cookie_store(true)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            discovery: make()
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()?,
        })
    }
}

fn configured_builder(config: &Config) -> ClientBuilder {
    client_builder(config.wired_interface())
        .timeout(Duration::from_secs(config.monitor.timeout_secs))
        .user_agent(USER_AGENT)
}

pub fn credential_session(config: &Config) -> Result<Client, RequestError> {
    // Each credential check must prove the supplied password, not reuse a prior SSO login.
    Ok(configured_builder(config)
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reachability {
    Online,
    Partial,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeFailure {
    Timeout,
    Transport,
    Status,
    UnexpectedBody,
}

pub struct ProbeReport {
    pub http: Result<(), ProbeFailure>,
    pub https: Result<(), ProbeFailure>,
}

impl ProbeReport {
    pub fn reachability(&self) -> Reachability {
        match (self.http.is_ok(), self.https.is_ok()) {
            (true, true) => Reachability::Online,
            (false, false) => Reachability::Offline,
            _ => Reachability::Partial,
        }
    }
}

pub fn client_builder(wired_interface: Option<&str>) -> ClientBuilder {
    let builder = Client::builder().no_proxy();
    #[cfg(target_os = "linux")]
    if let Some(iface) = wired_interface {
        // 路由表检查不能约束实际出口；将所有有线模式 HTTP 连接绑定到 WAN，
        // 并禁用环境代理，避免探测或凭据经由另一个出口发出。
        return builder.interface(iface);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = wired_interface;
    builder
}

pub fn probe(client: &Client, settings: &MonitorSettings) -> ProbeReport {
    ProbeReport {
        http: probe_one(
            client,
            &settings.http_url,
            Some(&settings.http_expected_body),
        ),
        https: probe_one(client, &settings.https_url, None),
    }
}

fn probe_one(client: &Client, url: &str, expected: Option<&str>) -> Result<(), ProbeFailure> {
    let response = client.get(url).send().map_err(|error| {
        if error.is_timeout() {
            ProbeFailure::Timeout
        } else {
            ProbeFailure::Transport
        }
    })?;
    if !response.status().is_success() {
        return Err(ProbeFailure::Status);
    }
    // Bound reads even when a captive portal returns a large response.
    let limit = if expected.is_some() { 65537 } else { 4096 };
    let mut body = Vec::new();
    response
        .take(limit)
        .read_to_end(&mut body)
        .map_err(|error| {
            let timeout = error.kind() == std::io::ErrorKind::TimedOut
                || error
                    .get_ref()
                    .and_then(|source| source.downcast_ref::<reqwest::Error>())
                    .is_some_and(|error| error.is_timeout());
            if timeout {
                ProbeFailure::Timeout
            } else {
                ProbeFailure::Transport
            }
        })?;
    if expected.is_some() && body.len() > 65536 {
        return Err(ProbeFailure::UnexpectedBody);
    }
    let valid = match expected {
        Some(value) => std::str::from_utf8(&body).is_ok_and(|body| body.trim() == value.trim()),
        None => body.iter().any(|byte| !byte.is_ascii_whitespace()),
    };
    if valid {
        Ok(())
    } else {
        Err(ProbeFailure::UnexpectedBody)
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use crate::test_support::{response, serve};

    #[test]
    fn independent_credential_checks_do_not_reuse_sso_cookies() {
        let cookie_response = "HTTP/1.1 200 OK\r\nSet-Cookie: TGC=old-session; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
        let (url, server) = serve(vec![
            cookie_response,
            response("200 OK", ""),
            response("200 OK", ""),
        ]);
        let config: Config =
            toml::from_str("username='test'\npassword='secret'\nwired=true\nwired_interface='lo'")
                .unwrap();
        let first = credential_session(&config).unwrap();
        first.get(&url).send().unwrap();
        first.get(&url).send().unwrap();
        credential_session(&config)
            .unwrap()
            .get(&url)
            .send()
            .unwrap();
        let requests = server.join().unwrap();
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("cookie: tgc=old-session")
        );
        assert!(!requests[2].to_ascii_lowercase().contains("cookie:"));
    }

    #[test]
    fn rejects_http_response_truncated_after_expected_text() {
        let mut body = String::from("expected");
        body.extend(std::iter::repeat_n(' ', 65537));
        body.push_str("unexpected suffix");
        let (url, server) = serve(vec![response("200 OK", &body)]);
        assert_eq!(
            probe_one(&client(), &url, Some("expected")),
            Err(ProbeFailure::UnexpectedBody)
        );
        server.join().unwrap();
    }

    #[test]
    fn response_body_timeout_is_reported_as_timeout() {
        let (url, server) = crate::test_support::serve_with_body_delay(
            vec![response("200 OK", "expected")],
            Duration::from_secs(1),
        );
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let result = probe_one(&client, &url, Some("expected"));
        server.join().unwrap();
        assert_eq!(result, Err(ProbeFailure::Timeout));
    }

    fn client() -> Client {
        Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
    }

    #[test]
    fn single_probe_failure_is_partial_not_offline() {
        let (url, server) = serve(vec![
            response("503 Service Unavailable", ""),
            response("200 OK", "public page"),
        ]);
        // Loopback HTTP tests status/body handling; configuration enforces HTTPS in production.
        let settings = MonitorSettings {
            http_url: url.clone(),
            https_url: url,
            ..Default::default()
        };
        let report = probe(&client(), &settings);
        assert_eq!(report.reachability(), Reachability::Partial);
        server.join().unwrap();
    }

    #[test]
    fn captive_portal_and_redirect_do_not_pass_probes() {
        let (url, server) = serve(vec![
            response("200 OK", "<html>login</html>"),
            response("302 Found", "redirect"),
        ]);
        let settings = MonitorSettings {
            http_url: url.clone(),
            https_url: url,
            ..Default::default()
        };
        assert_eq!(
            probe(&client(), &settings).reachability(),
            Reachability::Offline
        );
        server.join().unwrap();
    }

    #[test]
    fn both_expected_responses_are_required_for_online() {
        let (url, server) = serve(vec![
            response("200 OK", "Microsoft Connect Test"),
            response("200 OK", "public page"),
        ]);
        let settings = MonitorSettings {
            http_url: url.clone(),
            https_url: url,
            ..Default::default()
        };
        assert_eq!(
            probe(&client(), &settings).reachability(),
            Reachability::Online
        );
        server.join().unwrap();
    }

    #[test]
    fn empty_https_response_does_not_pass() {
        let (url, server) = serve(vec![response("200 OK", "  \r\n")]);
        assert_eq!(
            probe_one(&client(), &url, None),
            Err(ProbeFailure::UnexpectedBody)
        );
        server.join().unwrap();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::client_builder;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn wired_client_uses_selected_interface() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut reader = BufReader::new(&stream);
                        let mut request = String::new();
                        while !request.ends_with("\r\n\r\n") {
                            assert!(reader.read_line(&mut request).unwrap() > 0);
                            assert!(request.len() < 8192);
                        }
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                            .unwrap();
                        return;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "no request reached the bound interface"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("mock server failed: {err}"),
                }
            }
        });
        let response = client_builder(Some("lo"))
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send();
        let server_result = server.join();
        assert!(response.is_ok(), "bound request failed: {response:?}");
        server_result.unwrap();
        assert_eq!(response.unwrap().text().unwrap(), "ok");
    }

    #[test]
    fn missing_wired_interface_never_falls_back_to_default_route() {
        let iface = "whut-no-such";
        assert!(!std::path::Path::new("/sys/class/net").join(iface).exists());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let response = client_builder(Some(iface))
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap()
            .get(format!("http://{}", listener.local_addr().unwrap()))
            .send();
        assert!(response.is_err());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

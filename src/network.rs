use reqwest::blocking::{Client, ClientBuilder};

pub(crate) const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const CONNECT_TEST_URL: &str = "http://www.msftconnecttest.com/connecttest.txt";
const HTTPS_CONNECT_TEST_URL: &str = "https://www.baidu.com/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeFailure {
    Client,
    Timeout,
    Connection,
    Request,
    HttpStatus(u16),
    UnexpectedBody,
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Client => formatter.write_str("无法创建检测客户端"),
            Self::Timeout => formatter.write_str("请求超时"),
            Self::Connection => formatter.write_str("连接失败"),
            Self::Request => formatter.write_str("请求或响应读取失败"),
            Self::HttpStatus(status) => write!(formatter, "HTTP {status}"),
            Self::UnexpectedBody => formatter.write_str("响应内容与预期不符"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ConnectivityFailure {
    pub http: ProbeFailure,
    pub https: ProbeFailure,
}

impl std::fmt::Display for ConnectivityFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "两项联网检测均未通过（HTTP：{}；HTTPS：{}）",
            self.http, self.https
        )
    }
}

pub fn client_builder(wired_interface: Option<&str>) -> ClientBuilder {
    // 校园网检测和认证不使用代理环境变量；TUN 和透明代理由系统网络配置决定。
    let builder = Client::builder().no_proxy();
    #[cfg(target_os = "linux")]
    if let Some(iface) = wired_interface {
        // 路由表检查不能约束实际出口；将所有有线模式 HTTP 连接绑定到 WAN。
        return builder.interface(iface);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = wired_interface;
    builder
}

pub fn is_network_ok(wired_interface: Option<&str>) -> bool {
    check_connectivity(wired_interface).is_ok()
}

pub(crate) fn check_connectivity(wired_interface: Option<&str>) -> Result<(), ConnectivityFailure> {
    let client = match probe_client(wired_interface) {
        Ok(client) => client,
        Err(_) => {
            return Err(ConnectivityFailure {
                http: ProbeFailure::Client,
                https: ProbeFailure::Client,
            });
        }
    };

    check_connectivity_with(
        || probe(&client, CONNECT_TEST_URL, Some("Microsoft Connect Test")),
        // 独立站点的 HTTPS 连接仍校验证书且不跟随重定向；单个 HTTP 站点故障不触发认证。
        || probe(&client, HTTPS_CONNECT_TEST_URL, None),
    )
}

fn probe_client(wired_interface: Option<&str>) -> Result<Client, reqwest::Error> {
    client_builder(wired_interface)
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT_VALUE)
        .build()
}

fn check_connectivity_with(
    primary: impl FnOnce() -> Result<(), ProbeFailure>,
    fallback: impl FnOnce() -> Result<(), ProbeFailure>,
) -> Result<(), ConnectivityFailure> {
    match primary() {
        Ok(()) => Ok(()),
        Err(http) => fallback().map_err(|https| ConnectivityFailure { http, https }),
    }
}

fn probe(client: &Client, url: &str, expected_body: Option<&str>) -> Result<(), ProbeFailure> {
    let response = client.get(url).send().map_err(probe_error)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(ProbeFailure::HttpStatus(response.status().as_u16()));
    }
    if let Some(expected_body) = expected_body {
        let body = response.text().map_err(probe_error)?;
        if body.trim() != expected_body {
            return Err(ProbeFailure::UnexpectedBody);
        }
    }
    Ok(())
}

fn probe_error(error: reqwest::Error) -> ProbeFailure {
    if error.is_timeout() {
        ProbeFailure::Timeout
    } else if error.is_connect() {
        ProbeFailure::Connection
    } else {
        ProbeFailure::Request
    }
}

#[cfg(test)]
mod connectivity_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn a_single_failed_probe_does_not_mean_the_network_is_unavailable() {
        check_connectivity_with(
            || Ok(()),
            || panic!("a successful primary probe must not trigger a second request"),
        )
        .unwrap();
        for failure in [
            ProbeFailure::Timeout,
            ProbeFailure::Connection,
            ProbeFailure::HttpStatus(302),
            ProbeFailure::UnexpectedBody,
        ] {
            check_connectivity_with(|| Err(failure), || Ok(())).unwrap();
        }
    }

    #[test]
    fn both_probes_must_fail_and_the_reasons_are_preserved() {
        let failure = check_connectivity_with(
            || Err(ProbeFailure::Timeout),
            || Err(ProbeFailure::HttpStatus(503)),
        )
        .unwrap_err();
        assert_eq!(failure.http, ProbeFailure::Timeout);
        assert_eq!(failure.https, ProbeFailure::HttpStatus(503));
        assert_eq!(
            failure.to_string(),
            "两项联网检测均未通过（HTTP：请求超时；HTTPS：HTTP 503）"
        );
    }

    #[test]
    fn probe_rejects_redirects_errors_and_captive_portal_bodies() {
        for (status, body, expected_body, expected) in [
            (
                200,
                "Microsoft Connect Test\r\n",
                Some("Microsoft Connect Test"),
                Ok(()),
            ),
            (
                200,
                "<html>campus login</html>",
                Some("Microsoft Connect Test"),
                Err(ProbeFailure::UnexpectedBody),
            ),
            (
                302,
                "Microsoft Connect Test",
                Some("Microsoft Connect Test"),
                Err(ProbeFailure::HttpStatus(302)),
            ),
            (302, "", None, Err(ProbeFailure::HttpStatus(302))),
            (503, "unavailable", None, Err(ProbeFailure::HttpStatus(503))),
            (200, "", None, Ok(())),
        ] {
            let redirect = TcpListener::bind("127.0.0.1:0").unwrap();
            redirect.set_nonblocking(true).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let response = format!(
                "HTTP/1.1 {status} Response\r\nLocation: http://{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                redirect.local_addr().unwrap(),
                body.len()
            );
            let server = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "probe did not reach the target");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("mock accept failed: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut headers = String::new();
                while !headers.ends_with("\r\n\r\n") {
                    assert!(reader.read_line(&mut headers).unwrap() > 0);
                    assert!(headers.len() < 8192);
                }
                stream.write_all(response.as_bytes()).unwrap();
            });
            let result = probe(&probe_client(None).unwrap(), &url, expected_body);
            server.join().unwrap();
            assert_eq!(result, expected);
            assert_eq!(
                redirect.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
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

#[cfg(all(test, windows))]
mod windows_tests {
    use super::client_builder;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn proxy_environment_does_not_redirect_requests() {
        const CHILD_SCHEME: &str = "WHUT_TEST_DIRECT_REQUEST_SCHEME";
        if let Some(scheme) = std::env::var_os(CHILD_SCHEME) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let https = scheme == "https";
            let server = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "target received no connection");
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("target accept failed: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                if https {
                    // Observe a direct TLS handshake without needing a certificate fixture.
                    let mut first_byte = [0];
                    stream.read_exact(&mut first_byte).unwrap();
                    assert_eq!(first_byte[0], 0x16, "expected a TLS handshake record");
                } else {
                    let mut reader = BufReader::new(&stream);
                    let mut request = String::new();
                    while !request.ends_with("\r\n\r\n") {
                        assert!(reader.read_line(&mut request).unwrap() > 0);
                        assert!(request.len() < 8192);
                    }
                    assert!(request.starts_with("GET /direct "));
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .unwrap();
                }
            });
            let response = client_builder(None)
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap()
                .get(format!("{}://{address}/direct", scheme.to_str().unwrap()))
                .send();
            server.join().unwrap();
            if https {
                // The target deliberately closes after observing the handshake.
                assert!(response.is_err());
            } else {
                assert_eq!(response.unwrap().text().unwrap(), "ok");
            }
            return;
        }

        for scheme in ["http", "https"] {
            let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
            proxy.set_nonblocking(true).unwrap();
            let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
            // Child environments isolate proxy settings from concurrently running tests.
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "network::windows_tests::proxy_environment_does_not_redirect_requests",
                    "--nocapture",
                ])
                .env(CHILD_SCHEME, scheme)
                .env("HTTP_PROXY", &proxy_url)
                .env("HTTPS_PROXY", &proxy_url)
                .env("ALL_PROXY", &proxy_url)
                .env("NO_PROXY", "")
                .env_remove("REQUEST_METHOD")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(6);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("direct {scheme} request did not finish promptly");
                }
                thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{scheme} child failed: {output:?}");
            assert_eq!(
                proxy.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "{scheme} request reached the proxy"
            );
        }
    }
}

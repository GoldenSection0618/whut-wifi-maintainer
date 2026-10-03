use reqwest::blocking::{Client, ClientBuilder};

pub(crate) const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const CONNECT_TEST_URL: &str = "http://www.msftconnecttest.com/connecttest.txt";

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
    let client = match client_builder(wired_interface)
        .timeout(std::time::Duration::from_secs(5))
        .user_agent(USER_AGENT_VALUE)
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    // 仅 HTTP 成功还不够；被认证页劫持时，响应内容会不同。
    match client.get(CONNECT_TEST_URL).send() {
        Ok(resp) if resp.status().is_success() => match resp.text() {
            Ok(text) => text.trim() == "Microsoft Connect Test",
            Err(_) => false,
        },
        _ => false,
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

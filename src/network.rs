use reqwest::blocking::{Client, ClientBuilder};

use crate::USER_AGENT_VALUE;

const CONNECT_TEST_URL: &str = "http://www.msftconnecttest.com/connecttest.txt";

pub fn client_builder(wired_interface: Option<&str>) -> ClientBuilder {
    let builder = Client::builder();
    #[cfg(target_os = "linux")]
    if let Some(iface) = wired_interface {
        // 路由表检查不能约束实际出口；将所有有线模式 HTTP 连接绑定到 WAN，
        // 并禁用环境代理，避免探测或凭据经由另一个出口发出。
        return builder.interface(iface).no_proxy();
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

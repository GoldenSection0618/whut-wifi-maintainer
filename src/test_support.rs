//! Bounded loopback HTTP fixtures. Tests never contact a campus or public endpoint.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub fn serve(responses: Vec<String>) -> (String, JoinHandle<Vec<String>>) {
    serve_with_body_delay(responses, Duration::ZERO)
}

pub fn serve_with_body_delay(
    responses: Vec<String>,
    delay: Duration,
) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "expected mock request did not arrive"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("mock accept failed: {error}"),
                }
            };
            // Accepted sockets inherit nonblocking mode on Windows.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(&stream);
            let mut headers = String::new();
            while !headers.ends_with("\r\n\r\n") {
                assert!(reader.read_line(&mut headers).unwrap() > 0);
                assert!(headers.len() < 16384);
            }
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(length < 16384);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            requests.push(headers + &String::from_utf8(body).unwrap());
            let (headers, body) = response.split_once("\r\n\r\n").unwrap();
            write!(stream, "{headers}\r\n\r\n").unwrap();
            thread::sleep(delay);
            // A timeout test intentionally lets the client close before the body arrives.
            if let Err(error) = stream.write_all(body.as_bytes()) {
                assert!(!delay.is_zero(), "mock response failed: {error}");
            }
        }
        requests
    });
    (url, handle)
}

pub fn response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

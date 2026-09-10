use reqwest::blocking::Client;

use crate::USER_AGENT_VALUE;

const CONNECT_TEST_URL: &str = "http://www.msftconnecttest.com/connecttest.txt";

pub fn is_network_ok() -> bool {
    let client = match Client::builder()
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

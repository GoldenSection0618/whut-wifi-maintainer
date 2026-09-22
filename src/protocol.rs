//! WHUT protocol defaults. Device-specific values belong in configuration.
pub const USER_AGENT: &str = "Mozilla/5.0 (compatible; WHUT-WiFi-Maintainer)";
pub const REDIRECT_URL: &str = "http://www.msftconnecttest.com/redirect";
pub const CSRF_TOKEN_URL: &str = "http://172.30.21.100/api/csrf-token";
pub const LOGIN_URL: &str = "http://172.30.21.100/api/account/login";
pub const UNIFIED_LOGIN_URL: &str =
    "https://zhlgd.whut.edu.cn/tpass/login?service=https%3A%2F%2Fzhlgd.whut.edu.cn%2Ftp_up%2F";
pub const UNIFIED_RSA_URL: &str = "https://zhlgd.whut.edu.cn/tpass/rsa?skipWechat=true";
pub const UNIFIED_SERVICE_URL: &str = "https://zhlgd.whut.edu.cn/tp_up/";

pub fn credentials_rejected(message: &str) -> bool {
    [
        "密码错误",
        "密码不正确",
        "用户名或密码错误",
        "账号或密码错误",
        "账号不存在",
        "用户不存在",
    ]
    .iter()
    .any(|reason| message.contains(reason))
}

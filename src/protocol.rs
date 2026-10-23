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
    if ["验证码", "锁定", "过期", "维护"]
        .iter()
        .any(|reason| message.contains(reason))
    {
        return false;
    }
    [
        "密码错误",
        "密码不正确",
        "用户名或密码错误",
        "账号或密码错误",
        "账号不存在",
        "帐号不存在",
        "用户名不存在",
        "用户不存在",
    ]
    .iter()
    .any(|reason| message.contains(reason))
}

#[cfg(test)]
mod tests {
    use super::credentials_rejected;

    #[test]
    fn distinguishes_credentials_from_other_account_errors() {
        for message in ["用户名或密码错误", "密码不正确", "帐号不存在"] {
            assert!(credentials_rejected(message));
        }
        for message in [
            "验证码错误",
            "会话已过期",
            "系统维护中",
            "密码错误次数过多，账号已锁定",
        ] {
            assert!(!credentials_rejected(message));
        }
    }
}

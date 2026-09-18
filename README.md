# 武汉理工大学校园网保持器

本校校园网有 3 个，分别是`WHUT-WLAN`，`WHUT-DORM`，`WHUT-ISP`，这 3 个 wifi 背后是同一个数据库。

比较恼人的是，该数据库只允许**同一个账号下最多 2 个设备同时连接校园网**。事实上，当前人均电子设备至少 3 个并不是什么罕见的事情，本校的这一规定已经落后。
当第三个设备接入 Wi-Fi 时，校园网会基于某种不可知的机制，在一定时间后从前两个设备中踢出一个（表现为需要重新认证）。
该机制不可知且不可控，即经过一段不确定的时间后，随机踢掉其中一个设备。

这种不可控给学生带来日常的麻烦，很多同学不得不频繁地重新输入账号密码以连接校园网，这只能依赖校方放宽设备数限制来解决。

这种不可控还会带来不必要的损失，比如当你正在游戏中激情对战时，突然游戏掉线，居然仅仅只是因为校园网需要重新认证；再比如电脑上正在下载重要文件，
或者你正在等待电脑上一个联网程序的最终结果，你估算出门上课或吃饭回来以后任务就能完成，
可你回到寝室之后，却发现电脑意外断网，任务已经中止甚至失败，一切又得重来……这是本仓库希望能解决的问题。

本仓库提供一个程序，能够检测电脑上是否已经断开网络，并在断网的第一时间重新认证，快速重连。

## 使用方法

### Windows

1. 从 Releases 下载或在本地执行 `cargo build --release` 编译；
2. 运行 `whut-wifi-maintainer.exe`（或双击 `run.cmd`），首次运行会提示输入校园网账号密码；
3. 账号密码会保存到程序同目录的 `config.toml`，之后自动读取。

### Linux / OpenWrt（路由器常驻保活）

Linux 下程序无法枚举 Wi-Fi SSID，改为通过 **WAN 口默认路由 + 校园网认证门户探测**判断是否已接入校园网：先确认存在默认路由（WAN 已获取地址），再请求认证门户的 CSRF 接口验证确实处于校园网，两者都通过后才开始保活，避免在家庭网络等非校园环境下误发认证请求。适合将路由器有线接入校园网后 7×24 小时保持在线。

交叉编译静态二进制（以 aarch64 路由器为例）：

```sh
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl
# 产物：target/aarch64-unknown-linux-musl/release/whut-wifi-maintainer
```

在路由器上运行：

```sh
mkdir -p /root/whut-wifi-maintainer
# 将编译产物和 config.toml 放入该目录
cd /root/whut-wifi-maintainer && ./whut-wifi-maintainer
```

配置文件（`config.toml`，与程序同目录，不存在时首次运行会提示输入）：

```toml
username = "你的学号"
password = "你的密码"
```

程序每 30 秒检测一次 `http://www.msftconnecttest.com/connecttest.txt` 是否返回预期内容，一旦发现断网立即重新认证，通常在 30~60 秒内恢复。

## 附录：认证门户 API（逆向整理）

以下接口通过分析认证门户（`http://172.30.21.100/tpl/whut/login.html`）前端代码并实测整理，可用于实现登出、在线状态查询等扩展功能。门户 API 在校园网认证前即可访问（处于免认证白名单内），任何接入校园网的设备均可调用。

| 端点（基址 `http://172.30.21.100/api`） | 方法 | 作用 | 认证要求 |
| --- | --- | --- | --- |
| `/csrf-token` | GET | 获取 CSRF token | 无 |
| `/account/login` | POST | 账号密码认证 | 请求头 `X-CSRF-Token` |
| `/account/status?token=` | GET | 查询在线状态、流量、会话信息 | 见下方说明 |
| `/account/logout?token=` | GET | **登出（注销当前会话）** | URL 中的 token |
| `/config` | GET | 门户配置（含自服务平台地址） | 无 |
| `/notice/some?id=login` | GET | 登录页公告 | 无 |

### 登出机制要点

- 登出只需一次 GET：`/api/account/logout?token=<token>`，返回 `{"code":0,"msg":"登出成功"}` 即成功，`code:1` 为失败。
- token 来源于登录响应（前端存于 `localStorage`）。**实测发现 `/account/status` 并不校验 token**：在已认证的连接后调用（甚至不传 token），响应中会直接附带一枚新鲜可用的 `token` 字段。因此程序无需持久化保存 token——任何时候都可以通过 `status` 取 token、再调 `logout` 完成注销。
- `status` 响应还包含会话建立时间（`AddTime`）、上下行流量（`BytesIn4/BytesOut4`）、当前 IP/MAC、`macOnlineCount`（单 MAC 在线会话数）等字段，可直接用于健康检查。
- 登录页 HTML 中存在被注释掉的 `do_force_logout()`（"远程注销"），多设备会话管理/强制下线功能在自服务平台 `http://selfaaa.whut.edu.cn`（地址来自 `/api/config` 的 `selfservice_url`），需浏览器登录账号后使用。

### 实测验证（2026-09-18，小米 AX3600 / OpenWrt 23.05.5）

通过 `status` 取 token 后调用 `logout`，返回 `登出成功`，旧会话立即销毁；本程序在 **15 秒内**检测到断网并自动完成重新认证，全程无需人工干预。同 IP、同 MAC 直接重认证成功，未触发任何风控。

### 应用场景与安全提示

- **优雅下线**：更换 MAC 地址或撤出设备前，先调用 `logout` 注销旧会话，避免账号在 BRAS 上残留僵死会话，降低被风控（账号临时禁用）的概率。
- **CSRF 风险**：`logout` 为 GET 请求且 token 位于 URL，理论上恶意网页可通过 `<img>` 标签等方式触发跨站请求将在线用户踢下线。使用本 API 的衍生工具时请注意不要在不可信页面环境中暴露 token。

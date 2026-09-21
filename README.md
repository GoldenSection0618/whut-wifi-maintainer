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

注意：后台常驻 / 开机自启等无交互终端场景下，程序无法提示输入账号密码。请先在终端交互运行一次完成配置，或预先手工创建好 `config.toml`，再以后台方式启动。

程序每 30 秒检测一次 `http://www.msftconnecttest.com/connecttest.txt` 是否返回预期内容，一旦发现断网立即重新认证，通常在 30~60 秒内恢复。

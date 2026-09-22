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

### Linux / OpenWrt（有线接入支持）

面向路由器等通过网线接入校园网的设备，适合 7×24 小时常驻保活。

有线模式需要**显式开启并绑定校园网 WAN 接口**。程序检查该接口的 IPv4 默认路由，然后检测外网；只有确认需要认证时才检查门户并提交凭据。门户探测、连通性检查和认证请求都绑定该接口，并禁用 HTTP/HTTPS 环境代理；绑定失败不会改走其他接口。Linux 的接口绑定可能需要 root 或相应网络权限，OpenWrt 可使用 root 运行。

正常安装的 OpenWrt 推荐使用软件包与 procd，详见 [安装、升级与迁移说明](docs/openwrt.md)。软件包将程序与配置分开存放，升级应用无需修改内核镜像。恢复环境的 tmpfs 不具有普通文件持久性，必须先迁移到正常安装环境。

HTTP 门户及 CSRF token **不能证明服务器身份**。请仅在确定所选接口接入可信校园网时开启有线模式，不要为家庭网络或备用出口启用此设置。

交叉编译静态二进制（以 aarch64 路由器为例，需要另外准备目标平台的 musl C 编译器 / 链接器，安装 Rust target 并不会安装它们）：

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

配置文件（`config.toml`，与程序同目录）：

```toml
username = "你的学号"
password = "你的密码"
wired = true
wired_interface = "eth0"  # 绑定 WAN 口接口名，按实际情况填写（如 eth0.2、pppoe-wan）
```

配置文件包含明文账号密码。程序通过同目录临时文件、同步写入与原子替换保存，Unix 新文件和替换后的文件均为 `0600`。替换前失败会保留旧配置；若替换成功后目录同步失败，会明确报告“已替换，但断电持久性未确认”。手工创建配置也请执行 `chmod 600 config.toml`。

Linux/OpenWrt 首次运行前必须手工准备上述完整的 `config.toml`；缺少账号密码或配置格式错误时，程序会报告错误并退出。配置在启动时读取，修改配置后请重启程序。终端中重新输入账号密码会保留有线模式和接口设置。后台常驻 / 开机自启时无法交互输入，请先修正配置再重启。

### 配置路径与后台运行

```sh
whut-wifi-maintainer --config /etc/whut-wifi-maintainer/config.toml --check-config
whut-wifi-maintainer --config /etc/whut-wifi-maintainer/config.toml --non-interactive
```

`--config` 指定的文件不存在或无效时直接失败，不会回退到其他文件。未指定时仍依次检查程序同目录、当前目录；保存始终使用实际读取的路径。`--check-config` 只校验格式和设置，不联网、不改写正文；Unix 加载配置时始终先将权限收紧为 `0600`，权限调整失败立即退出，也不验证密码或接口实际可用性。

`--non-interactive` 或标准输入不是终端时，明确的凭据拒绝会让程序退出并提示修改配置，不会反复索要密码。退出码 `0` 表示校验成功，`2` 表示配置、启动或凭据错误；常驻运行期间不会因单次网络请求失败退出。交互更改的凭据必须通过独立统一认证会话验证后才保存。

### 网络状态与可选设置

默认每轮检查后等待 30 秒，每个请求超时 5 秒。Microsoft HTTP 探测必须返回预期正文，百度 HTTPS 探测必须通过 TLS 校验、返回成功状态及非空内容；探测不跟随重定向。两项都通过才报告网络正常；部分通过报告“部分可达”，不会重新登录；全部失败连续两轮后才尝试认证，认证尝试之间至少等待 30 秒。

门户返回成功只表示请求被接受，只有随后两项探测均通过才报告恢复。统一认证暂不可用时继续检测外网。探测站本身也可能故障，这些结果不代表所有网站均可访问；实际恢复时间取决于探测、门户和账号状态。

旧配置无需修改；以下字段可按需添加：

```toml
[monitor]
interval_secs = 30
timeout_secs = 5
http_url = "http://www.msftconnecttest.com/connecttest.txt"
http_expected_body = "Microsoft Connect Test"
https_url = "https://www.baidu.com/"

[portal]
fallback_nas_id = "52"
```

检查间隔允许 1–86400 秒，超时允许 1–120 秒。备用 nasId 只在无法从已知校园门户取得合法值时使用；探测地址与备用值不改变凭据提交的固定校园门户地址。

# OpenWrt 软件包

支持正常安装、具有持久化 overlay 的 OpenWrt；initramfs / tmpfs 恢复环境不能把普通文件安装当作持久化部署。本包不刷固件、不直接读写 MTD/UBI。

## 使用官方 SDK 构建

先从 OpenWrt 官方下载与目标固件一致的 SDK，核对官方 SHA-256。在 Linux / WSL 的 Linux 文件系统中解压（SDK 路径不能含空格），然后执行：

```sh
cd "$SDK"
./scripts/feeds update base packages
cd "$SOURCE"
python3 scripts/build_openwrt.py --sdk "$SDK" --output "$OUTPUT" --system-rust
```

`SDK` 是已解压 SDK 的绝对路径，`SOURCE` 是源码仓库，`OUTPUT` 是产物目录。源码必须已提交且工作区干净，构建只使用该提交的跟踪文件。不会打包本地账号配置。

`--system-rust` 复用已安装 Rust 与目标标准库；aarch64 需先执行 `rustup target add aarch64-unknown-linux-musl`。链接器和目标 C 库仍由 SDK 提供，构建复用 OpenWrt 的 `rust-package.mk`。省略该参数时使用官方 `rust/host` 依赖，其首次构建耗时较长。

输出 APK、可执行文件、`manifest.json`、`SHA256SUMS`。清单自动记录版本、源码提交、架构、SDK 版本、长度和哈希。OpenWrt 25.12 使用 APK；不要将其交给旧版本的 opkg。

## 安装与配置

先核对产物清单中的架构、版本与 SHA-256。对自己本机构建并已验证的安装包，可以执行：

```sh
apk add --allow-untrusted /tmp/whut-wifi-maintainer-*.apk
chmod 600 /etc/whut-wifi-maintainer/config.toml
vi /etc/whut-wifi-maintainer/config.toml
whut-wifi-maintainer --config /etc/whut-wifi-maintainer/config.toml --check-config
/etc/init.d/whut-wifi-maintainer enable
/etc/init.d/whut-wifi-maintainer start
```

`--allow-untrusted` 仅适用于上述本地构建、人工验证哈希的包；它不是下载后跳过来源核验的通用安装方法。批量分发应使用自己的签名仓库。

默认配置没有凭据、有线模式关闭，因此安装后不会自动向任何接口提交密码。必须填写账号密码，并明确设置 `wired = true` 和真实校园网出口 `wired_interface`。

程序位于 `/usr/bin/whut-wifi-maintainer`，配置位于 `/etc/whut-wifi-maintainer/config.toml`。procd 启动前做离线配置校验，运行时使用非交互模式。短时间连续失败会在有限次重启后停止；修正配置后手动 restart。配置只在启动时读取，reload 等价于重启。

## 升级、回退与重启验证

应用升级只安装新的 APK；无需重新制作内核镜像。保留上一版本 APK 和配置备份，记录实际执行程序的哈希。配置已声明为 conffile，并加入 sysupgrade 保留清单；正常保留配置的升级应保留它，`sysupgrade -n` 会明确清除配置。

安装后运行离线校验并重启服务，确认 HTTP 与 HTTPS 两项均通过，再安排一次设备重启验证自启动及配置保留。回退时安装保存的上一版本 APK（apk 降级可使用 `apk add --allow-untrusted --force-old-apk /tmp/previous.apk`），重新验证。配置 schema 保持向后兼容；仍应保留相应版本备份。

固件升级与应用升级是不同操作：固件升级可能需要重新安装应用包，应提前保存同目标版本可用的离线包。不要把包含旧内核模块、旧基础系统脚本的整个 /etc 或根目录直接覆盖到新版本。

## 迁移现有恢复环境

1. 在电脑保存并核对所有 UBI 卷、启动信息、配置、设备密钥和自定义应用，而不仅是本程序。
2. 下载对应设备的官方正常安装镜像，核对校验值和设备兼容标识，准备离线应用包及恢复材料。
3. 电脑通过 LAN 连接、操作人在现场后，按该设备的官方安装流程切换。
4. 在正常持久文件系统中按功能迁移网络、无线、SSH 和应用配置；采用新固件的驱动与基础脚本。
5. 验证外网、无线、原有应用、程序升级及回退、配置更改后的重启保留。

设备私密迁移材料不应提交到源码仓库。旧固件拼接工具仅作为历史恢复资料保存，不参与标准安装与后续应用更新。

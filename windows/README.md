# Windows 桌面版

界面使用 WinUI 3，托盘沿用 [H.NotifyIcon 的 WinUI 示例](https://github.com/HavenDV/H.NotifyIcon/tree/master/src/apps/H.NotifyIcon.Apps.WinUI)。保活和配置由同目录的 Rust 工作进程负责。

在 Windows x64 上安装 .NET 10 SDK 和 Rust Windows 构建工具后，从仓库根目录执行：

```powershell
cargo build --release --locked
./windows/package.ps1 -OutputDirectory target/winui
```

发布目录必须尚未创建，以免打包旧文件。保留发布目录中的文件，运行 `whut-wifi-maintainer.exe`。已有的 `config.toml` 放在同目录即可复用。Cargo 的默认产物目录是 `target/cargo`，Linux Release 二进制位于 `target/cargo/release/whut-wifi-maintainer`。

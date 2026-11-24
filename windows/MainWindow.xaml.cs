using System.ComponentModel;
using System.Diagnostics;
using System.Text;
using System.Text.Json;
using System.Windows.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;

namespace WhutWifi;

public sealed partial class MainWindow : Window
{
    private readonly Queue<string> logs = new();
    private Process? worker;
    private Task credentialWrite = Task.CompletedTask;
    private bool exiting;

    public ICommand OpenCommand { get; }
    public ICommand ExitCommand { get; }

    public MainWindow()
    {
        OpenCommand = new TrayCommand(OpenWindow);
        ExitCommand = new TrayCommand(Exit);
        InitializeComponent();
        var iconPath = Path.Combine(AppContext.BaseDirectory, "Assets", "wifi.ico");
        CaptionIcon.Source = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri(iconPath));
        var trayImage = new System.Drawing.Icon(iconPath, 32, 32);
        TrayIcon.Icon = trayImage;
        AppWindow.SetIcon(iconPath);
        Closed += (_, _) =>
        {
            TrayIcon.Dispose();
            trayImage.Dispose();
        };
        CaptionTitle.Text = Title;
        if (AppWindowTitleBar.IsCustomizationSupported())
        {
            ExtendsContentIntoTitleBar = true;
            SetTitleBar(Caption);
            var bar = AppWindow.TitleBar;
            bar.PreferredHeightOption = TitleBarHeightOption.Tall;
            var background = Windows.UI.Color.FromArgb(255, 32, 32, 32);
            bar.ButtonBackgroundColor = bar.ButtonInactiveBackgroundColor = background;
            bar.ButtonForegroundColor = Microsoft.UI.Colors.White;
            bar.ButtonInactiveForegroundColor = Microsoft.UI.Colors.Gainsboro;
            bar.ButtonHoverForegroundColor = bar.ButtonPressedForegroundColor = Microsoft.UI.Colors.White;
            bar.ButtonHoverBackgroundColor = Windows.UI.Color.FromArgb(255, 56, 56, 56);
            bar.ButtonPressedBackgroundColor = Windows.UI.Color.FromArgb(255, 72, 72, 72);
        }
        else
        {
            Caption.Visibility = Visibility.Collapsed;
        }
        ContentGrid.Loaded += (_, _) =>
        {
            double scale = ContentGrid.XamlRoot.RasterizationScale;
            AppWindow.ResizeClient(new Windows.Graphics.SizeInt32(
                (int)(540 * scale), (int)((340 + (ExtendsContentIntoTitleBar ? Caption.Height : 0)) * scale)));
        };
        if (AppWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.IsResizable = false;
            presenter.IsMaximizable = false;
        }
        AppWindow.Closing += (_, args) =>
        {
            if (exiting)
            {
                args.Cancel = worker is not null;
                return;
            }
            if (!TrayIcon.IsCreated)
                return;
            args.Cancel = true;
            AppWindow.Hide();
        };
    }

    internal void Start()
    {
        TrayIcon.ForceCreate(enablesEfficiencyMode: false);
        if (!TrayIcon.IsCreated)
            Report("[!] 系统托盘图标不可用。");
        _ = RunWorkerAsync();
    }

    private async Task RunWorkerAsync()
    {
        try
        {
            var start = new ProcessStartInfo(Path.Combine(AppContext.BaseDirectory, "whut-wifi-worker.exe"))
            {
                WorkingDirectory = AppContext.BaseDirectory,
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                StandardInputEncoding = new UTF8Encoding(false),
                StandardOutputEncoding = Encoding.UTF8
            };
            start.ArgumentList.Add("--winui-worker");
            using var process = Process.Start(start) ?? throw new InvalidOperationException("无法启动保活进程");
            worker = process;
            while (await process.StandardOutput.ReadLineAsync() is { } line)
                HandleWorkerMessage(line);
            await process.WaitForExitAsync();
            if (!exiting)
                Report("[!] 保活已停止。");
        }
        catch (Exception error) when (error is Win32Exception or IOException or InvalidOperationException)
        {
            if (!exiting)
                Report($"[!] 保活进程无法运行：{error.Message}");
        }
        finally
        {
            worker = null;
            CredentialsForm.Visibility = Visibility.Collapsed;
            if (exiting)
                Close();
        }
    }

    private void HandleWorkerMessage(string line)
    {
        try
        {
            using var update = JsonDocument.Parse(line);
            var payload = update.RootElement;
            if (payload.ValueKind == JsonValueKind.Object &&
                payload.TryGetProperty("kind", out var kind) && kind.ValueKind == JsonValueKind.String)
            {
                switch (kind.GetString())
                {
                    case "credentials_needed":
                        if (!exiting)
                        {
                            CredentialsForm.Visibility = Visibility.Visible;
                            Username.Focus(FocusState.Programmatic);
                        }
                        return;
                    case "status" when payload.TryGetProperty("message", out var message) &&
                        message.ValueKind == JsonValueKind.String && message.GetString() is { } text:
                        Report(text);
                        return;
                }
            }
        }
        catch (JsonException) { }
        Report("[!] 保活进程消息格式错误。");
    }

    private void Report(string line)
    {
        if (logs.Count == 200)
            logs.Dequeue();
        logs.Enqueue(line);
        LogText.Text = string.Join('\n', logs);
        LogText.UpdateLayout();
        LogScroll.ChangeView(null, LogScroll.ScrollableHeight, null, disableAnimation: true);
    }

    private void OpenWindow()
    {
        if (AppWindow.Presenter is OverlappedPresenter presenter &&
            presenter.State == OverlappedPresenterState.Minimized)
            presenter.Restore();
        AppWindow.Show();
    }

    private async void Exit()
    {
        if (exiting)
            return;
        exiting = true;
        if (worker is not { } process)
        {
            Close();
            return;
        }
        CredentialsForm.Visibility = Visibility.Collapsed;
        Report("[*] 正在停止保活。");
        try
        {
            await credentialWrite;
        }
        catch (Exception error) when (error is IOException or InvalidOperationException) { }
        if (worker == process)
        {
            try
            {
                process.StandardInput.Close();
            }
            catch (IOException) { }
        }
    }

    private void CredentialsKeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (args.Key == Windows.System.VirtualKey.Enter)
        {
            args.Handled = true;
            SubmitCredentials(sender, args);
        }
    }

    private async void SubmitCredentials(object sender, RoutedEventArgs args)
    {
        if (exiting || !credentialWrite.IsCompleted ||
            CredentialsForm.Visibility != Visibility.Visible || worker is not { } process)
            return;
        string username = Username.Text.Trim();
        string password = Password.Password;
        if (username.Length == 0 || password.Length == 0)
        {
            Report("[!] 账号或密码为空。");
            if (username.Length == 0)
                Username.Focus(FocusState.Programmatic);
            else
                Password.Focus(FocusState.Programmatic);
            return;
        }
        Password.Password = "";
        CredentialsForm.Visibility = Visibility.Collapsed;
        try
        {
            credentialWrite = process.StandardInput.WriteLineAsync(
                JsonSerializer.Serialize(new { username, password }));
            await credentialWrite;
        }
        catch (Exception error) when (error is IOException or InvalidOperationException)
        {
            if (!exiting)
                Report("[!] 无法提交凭据，保活进程的输入已关闭。");
        }
    }

    private sealed class TrayCommand(Action execute) : ICommand
    {
        public event EventHandler? CanExecuteChanged { add { } remove { } }
        public bool CanExecute(object? parameter) => true;
        public void Execute(object? parameter) => execute();
    }
}

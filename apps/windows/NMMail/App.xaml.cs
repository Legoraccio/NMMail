using System.Diagnostics;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using NMMail.Services;
using NMMail.Services.Demo;
using NMMail.ViewModels;

namespace NMMail;

public partial class App : Application
{
    private DispatcherQueue? _dispatcher;
    private bool _shuttingDown;

    public App(StartupOptions options)
    {
        Options = options;
        InitializeComponent();
        UnhandledException += OnUnhandledException;
        TaskScheduler.UnobservedTaskException += (_, e) =>
        {
            AppInfo.Log("unobserved task exception: " + e.Exception);
            e.SetObserved();
        };
    }

    public static new App Current => (App)Application.Current;

    public StartupOptions Options { get; }
    public IDaemonClient? Client { get; private set; }
    public MainViewModel? ViewModel { get; private set; }
    public MainWindow? MainWindow { get; private set; }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        StartupTrace.Mark("OnLaunched");
        _dispatcher = DispatcherQueue.GetForCurrentThread();
        ThemeHelper.Apply(UiStateStore.Current.Theme);

        // Show the window shell immediately; data arrives asynchronously from the daemon.
        Client = Options.Demo ? new FakeDaemonClient(_dispatcher) : new DaemonClient(_dispatcher);
        ViewModel = new MainViewModel(Client, _dispatcher);
        MainWindow = new MainWindow(ViewModel);
        MainWindow.Activate();
        MainWindow.ApplyInitialState();
        StartupTrace.HookFirstFrame();

        Client.Start();
        Handle(Options);
#if DEBUG
        DebugSnapshot.Start(_dispatcher);
#endif
    }

    /// <summary>A second NMMail.exe was started: it redirected its activation here (called on a background thread).</summary>
    public void OnRedirectedActivation(StartupOptions options) =>
        _dispatcher?.TryEnqueue(() =>
        {
            MainWindow?.BringToFront();
            Handle(options);
        });

    private void Handle(StartupOptions options)
    {
        if (ViewModel is null) return;
        if (options.OpenMessageId is long id) ViewModel.OpenMessage(id);
        if (options.Compose is { } mailto) ViewModel.ComposeMailto(mailto);
    }

    public async void Shutdown()
    {
        if (_shuttingDown) return;
        _shuttingDown = true;
        UiStateStore.Save();
        try
        {
            if (Client is not null) await Client.DisposeAsync().AsTask().WaitAsync(TimeSpan.FromMilliseconds(500));
        }
        catch (Exception ex)
        {
            Debug.WriteLine("client dispose: " + ex.Message);
        }
        Exit();
    }

    private static void OnUnhandledException(object sender, Microsoft.UI.Xaml.UnhandledExceptionEventArgs e)
    {
        AppInfo.Log("unhandled exception: " + e.Exception);
        // Keep the UI alive: the daemon owns all state, a failed UI action must not kill the window.
        e.Handled = true;
    }
}

/// <summary>Opt-in startup timing (NMMAIL_STARTUP_TRACE=1) written to %LOCALAPPDATA%\NMMail\ui.log.</summary>
public static class StartupTrace
{
    private static readonly bool s_enabled = Environment.GetEnvironmentVariable("NMMAIL_STARTUP_TRACE") == "1";

    public static void Mark(string what)
    {
        if (!s_enabled) return;
        using var process = Process.GetCurrentProcess();
        AppInfo.Log($"startup: {what} at {(DateTime.Now - process.StartTime).TotalMilliseconds:F0} ms, working set {process.WorkingSet64 / (1024 * 1024)} MB");
    }

    public static void HookFirstFrame()
    {
        if (!s_enabled) return;
        EventHandler<object>? handler = null;
        handler = (_, _) =>
        {
            CompositionTarget.Rendering -= handler;
            Mark("first frame");
        };
        CompositionTarget.Rendering += handler;
    }
}

using System.Globalization;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;
using NMMail.Services;

namespace NMMail;

/// <summary>
/// Custom entry point: single instance (AppInstance.FindOrRegisterForKey + activation redirection),
/// language override before any resource is loaded, then the WinUI application.
/// </summary>
public static class Program
{
    private static App? s_app;

    [STAThread]
    private static int Main(string[] args)
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();
        var options = StartupOptions.Parse(args);

        if (RedirectToPrimaryInstance(options)) return 0;

        UiStateStore.PersistPreferences = !options.Demo;
        UiStateStore.Load();
        ApplyLanguage(UiStateStore.Current.Language);

        Application.Start(_ =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            s_app = new App(options);
        });
        return 0;
    }

    /// <summary>Returns true when another NMMail.exe already runs: activation (args) is forwarded to it.</summary>
    private static bool RedirectToPrimaryInstance(StartupOptions options)
    {
        AppInstance keyInstance;
        try
        {
            // The demo is a separate instance so it never steals the real UI's activations.
            keyInstance = AppInstance.FindOrRegisterForKey(options.Demo ? "NMMail.Demo" : "NMMail.Main");
        }
        catch (Exception ex)
        {
            AppInfo.Log("single instance check failed: " + ex.Message);
            return false;
        }

        if (keyInstance.IsCurrent)
        {
            keyInstance.Activated += OnActivated;
            return false;
        }

        var activation = AppInstance.GetCurrent().GetActivatedEventArgs();
        // We are the foreground process (the user just launched us): let the primary instance take the focus.
        NativeMethods.AllowSetForegroundWindow(keyInstance.ProcessId);

        var done = NativeMethods.CreateEvent(0, true, false, null);
        Task.Run(() =>
        {
            try
            {
                keyInstance.RedirectActivationToAsync(activation).AsTask().Wait();
            }
            catch (Exception ex)
            {
                AppInfo.Log("redirect failed: " + ex.Message);
            }
            NativeMethods.SetEvent(done);
        });
        // Pump COM while waiting (STA thread): a plain Wait() could deadlock the redirection.
        _ = NativeMethods.CoWaitForMultipleObjects(NativeMethods.CWMO_DEFAULT, NativeMethods.INFINITE, 1, [done], out _);
        NativeMethods.CloseHandle(done);
        return true;
    }

    private static void OnActivated(object? sender, AppActivationArguments args)
    {
        var options = new StartupOptions();
        if (args.Kind == ExtendedActivationKind.Launch &&
            args.Data is Windows.ApplicationModel.Activation.ILaunchActivatedEventArgs launch)
        {
            options = StartupOptions.ParseCommandLine(launch.Arguments);
        }
        s_app?.OnRedirectedActivation(options);
    }

    /// <summary>Language setting (system/it/en) cached locally because it must be applied before XAML loads.</summary>
    private static void ApplyLanguage(string language)
    {
        var tag = language switch
        {
            Models.LanguageNames.Italian => "it-IT",
            Models.LanguageNames.English => "en-US",
            _ => "",
        };
        // "system": nothing to override (MRT follows the Windows display languages; it-IT is the default).
        if (tag.Length == 0) return;
        try
        {
            Microsoft.Windows.Globalization.ApplicationLanguages.PrimaryLanguageOverride = tag;
        }
        catch (Exception ex)
        {
            AppInfo.Log("language override failed: " + ex.Message);
        }
        var culture = CultureInfo.GetCultureInfo(tag);
        CultureInfo.DefaultThreadCurrentUICulture = culture;
        CultureInfo.CurrentUICulture = culture;
    }
}

using System.Collections.ObjectModel;
using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Xaml.Media;
using NMMail.Helpers;
using NMMail.Models;
using NMMail.Services;

namespace NMMail.ViewModels;

/// <summary>An account row in Settings → Account.</summary>
public sealed partial class AccountSettingsItem : ObservableObject
{
    private readonly SettingsViewModel _owner;
    private bool _applying;

    public AccountSettingsItem(SettingsViewModel owner, Account account)
    {
        _owner = owner;
        Account = account;
        Title = "";
        Email = "";
        StateText = "";
        Update(account);
    }

    public Account Account { get; private set; }
    public long Id => Account.Id;
    public SolidColorBrush AccountBrush => Palette.AccountBrush(Account.Id);

    [ObservableProperty]
    public partial string Title { get; set; }

    [ObservableProperty]
    public partial string Email { get; set; }

    [ObservableProperty]
    public partial string StateText { get; set; }

    [ObservableProperty]
    public partial bool Notify { get; set; }

    [ObservableProperty]
    public partial bool PrimaryOnly { get; set; }

    [ObservableProperty]
    public partial bool IsGmail { get; set; }

    [ObservableProperty]
    public partial bool NeedsReauth { get; set; }

    partial void OnNotifyChanged(bool value)
    {
        if (!_applying) _ = _owner.UpdateAccountAsync(new UpdateAccountParams { Id = Id, Notify = value });
    }

    partial void OnPrimaryOnlyChanged(bool value)
    {
        if (!_applying)
            _ = _owner.UpdateAccountAsync(new UpdateAccountParams { Id = Id, NotifyFilter = value ? NotifyFilterNames.Primary : NotifyFilterNames.All });
    }

    public void Update(Account account)
    {
        _applying = true;
        try
        {
            Account = account;
            Title = string.IsNullOrWhiteSpace(account.DisplayName) ? account.Email : account.DisplayName;
            var provider = account.Provider switch
            {
                Providers.Gmail => "Gmail",
                Providers.Outlook => "Outlook",
                _ => "IMAP",
            };
            Email = provider + " · " + account.Email;
            StateText = account.State switch
            {
                AccountStates.Online => Loc.Get("AccountState_Online"),
                AccountStates.Connecting => Loc.Get("AccountState_Connecting"),
                AccountStates.Offline => Loc.Get("AccountState_Offline"),
                AccountStates.AuthRequired => Loc.Get("AccountState_AuthRequired"),
                AccountStates.Error => Loc.Get("AccountState_Error") + (string.IsNullOrWhiteSpace(account.Error) ? "" : ": " + account.Error),
                _ => account.State,
            };
            Notify = account.Notify;
            PrimaryOnly = account.NotifyFilter == NotifyFilterNames.Primary;
            IsGmail = account.Provider == Providers.Gmail;
            NeedsReauth = account.State == AccountStates.AuthRequired;
        }
        finally
        {
            _applying = false;
        }
    }
}

/// <summary>Settings page state. Every change is pushed to the daemon with settings.set (Partial&lt;Settings&gt;).</summary>
public sealed partial class SettingsViewModel : ObservableObject
{
    private static readonly int[] s_cacheDays = [30, 90, 180, 365];
    private static readonly string[] s_themes = [ThemeNames.System, ThemeNames.Light, ThemeNames.Dark];
    private static readonly string[] s_languages = [LanguageNames.System, LanguageNames.Italian, LanguageNames.English];

    private readonly MainViewModel _main;
    private readonly string _startupLanguage;
    private bool _applying;

    public SettingsViewModel(MainViewModel main)
    {
        _main = main;
        _startupLanguage = UiStateStore.Current.Language;
        GoogleClientId = "";
        GoogleClientSecret = "";
        MicrosoftClientId = "";
        QuietText = "";
        NotificationTestStatus = "";
        DaemonVersion = "–";
        DaemonPid = "–";
        DaemonUptime = "–";
        DaemonMemory = "–";
        UiMemory = "–";
        AppVersion = AppInfo.Version + (main.Client.IsDemo ? " (demo)" : "");
        _main.AccountsUpdated += (_, _) => SyncAccounts();
        _main.SettingsApplied += (_, s) => Apply(s);
        SyncAccounts();
        if (_main.CurrentSettings is { } settings) Apply(settings);
    }

    public IDaemonClient Client => _main.Client;
    public MainViewModel Main => _main;
    public ObservableCollection<AccountSettingsItem> Accounts { get; } = [];

    [ObservableProperty]
    public partial int SectionIndex { get; set; }

    [ObservableProperty]
    public partial bool Autostart { get; set; }

    [ObservableProperty]
    public partial bool NotificationsEnabled { get; set; }

    [ObservableProperty]
    public partial bool NotificationSound { get; set; }

    [ObservableProperty]
    public partial bool ShowPreview { get; set; }

    [ObservableProperty]
    public partial int CacheDaysIndex { get; set; }

    [ObservableProperty]
    public partial int ThemeIndex { get; set; }

    [ObservableProperty]
    public partial int LanguageIndex { get; set; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowGoogleOAuthFields), nameof(ShowOAuthSave))]
    public partial string GoogleClientId { get; set; }

    [ObservableProperty]
    public partial string GoogleClientSecret { get; set; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowMicrosoftOAuthFields), nameof(ShowOAuthSave))]
    public partial string MicrosoftClientId { get; set; }

    /// <summary>This build already contains the Google OAuth client (publisher registration).</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowGoogleOAuthFields), nameof(ShowOAuthSave), nameof(AnyOAuthBuiltIn),
        nameof(NoOAuthBuiltIn), nameof(OAuthBuiltInText))]
    public partial bool OAuthGoogleBuiltIn { get; set; }

    /// <summary>This build already contains the Microsoft OAuth client.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowMicrosoftOAuthFields), nameof(ShowOAuthSave), nameof(AnyOAuthBuiltIn),
        nameof(NoOAuthBuiltIn), nameof(OAuthBuiltInText))]
    public partial bool OAuthMicrosoftBuiltIn { get; set; }

    /// <summary>"Use my own credentials" (advanced): shows the override fields of built-in clients.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowGoogleOAuthFields), nameof(ShowMicrosoftOAuthFields), nameof(ShowOAuthSave))]
    public partial bool ShowOAuthAdvanced { get; set; }

    public bool AnyOAuthBuiltIn => OAuthGoogleBuiltIn || OAuthMicrosoftBuiltIn;
    public bool NoOAuthBuiltIn => !AnyOAuthBuiltIn;
    public bool ShowGoogleOAuthFields => !OAuthGoogleBuiltIn || ShowOAuthAdvanced || !string.IsNullOrWhiteSpace(GoogleClientId);
    public bool ShowMicrosoftOAuthFields => !OAuthMicrosoftBuiltIn || ShowOAuthAdvanced || !string.IsNullOrWhiteSpace(MicrosoftClientId);
    public bool ShowOAuthSave => ShowGoogleOAuthFields || ShowMicrosoftOAuthFields;

    public string OAuthBuiltInText => (OAuthGoogleBuiltIn, OAuthMicrosoftBuiltIn) switch
    {
        (true, true) => Loc.Get("Settings_OAuthBuiltInBoth"),
        (true, false) => Loc.Get("Settings_OAuthBuiltInGoogle"),
        (false, true) => Loc.Get("Settings_OAuthBuiltInMicrosoft"),
        _ => "",
    };

    [ObservableProperty]
    public partial string QuietText { get; set; }

    [ObservableProperty]
    public partial bool IsPaused { get; set; }

    [ObservableProperty]
    public partial bool ShowLanguageRestartHint { get; set; }

    [ObservableProperty]
    public partial string NotificationTestStatus { get; set; }

    [ObservableProperty]
    public partial bool ShowNotificationsDisabledHint { get; set; }

    [ObservableProperty]
    public partial bool IsTestingNotification { get; set; }

    [ObservableProperty]
    public partial string DaemonVersion { get; set; }

    [ObservableProperty]
    public partial string DaemonPid { get; set; }

    [ObservableProperty]
    public partial string DaemonUptime { get; set; }

    [ObservableProperty]
    public partial string DaemonMemory { get; set; }

    [ObservableProperty]
    public partial string UiMemory { get; set; }

    [ObservableProperty]
    public partial string AppVersion { get; set; }

    [ObservableProperty]
    public partial bool HasAccounts { get; set; }

    // ------------------------------------------------------------------ load / apply

    public async Task LoadAsync()
    {
        try
        {
            Apply(await Client.GetSettingsAsync());
        }
        catch (Exception ex)
        {
            _main.ShowError(Loc.Get("Error_Settings"), ex);
        }
        await RefreshStatusAsync();
    }

    private void Apply(Settings s)
    {
        _applying = true;
        try
        {
            Autostart = s.Autostart;
            NotificationsEnabled = s.NotificationsEnabled;
            NotificationSound = s.NotificationSound;
            ShowPreview = s.ShowPreview;
            var cacheIndex = Array.IndexOf(s_cacheDays, s.CacheDays);
            CacheDaysIndex = cacheIndex >= 0 ? cacheIndex : 1;
            ThemeIndex = Math.Max(0, Array.IndexOf(s_themes, s.Theme));
            LanguageIndex = Math.Max(0, Array.IndexOf(s_languages, s.Language));
            GoogleClientId = s.GoogleClientId ?? "";
            GoogleClientSecret = s.GoogleClientSecret ?? "";
            MicrosoftClientId = s.MicrosoftClientId ?? "";
            UpdateQuiet(s.QuietUntil);
        }
        finally
        {
            _applying = false;
        }
    }

    private void UpdateQuiet(long? quietUntil)
    {
        if (quietUntil is long until && until > DateTimeOffset.UtcNow.ToUnixTimeMilliseconds())
        {
            var local = Fmt.ToLocal(until);
            var when = local.Date == DateTime.Today
                ? local.ToString("HH:mm", CultureInfo.CurrentCulture)
                : local.ToString("ddd d MMM HH:mm", CultureInfo.CurrentCulture);
            QuietText = Loc.Format("Settings_PausedUntil", when);
            IsPaused = true;
        }
        else
        {
            QuietText = Loc.Get("Settings_NotPaused");
            IsPaused = false;
        }
    }

    private void SyncAccounts()
    {
        var accounts = _main.Accounts;
        var existing = Accounts.ToDictionary(a => a.Id);
        var desired = new List<AccountSettingsItem>(accounts.Count);
        foreach (var account in accounts)
        {
            if (existing.TryGetValue(account.Id, out var item)) item.Update(account);
            else item = new AccountSettingsItem(this, account);
            desired.Add(item);
        }
        CollectionSync.Sync(Accounts, desired);
        HasAccounts = Accounts.Count > 0;
    }

    // ------------------------------------------------------------------ pushes

    partial void OnAutostartChanged(bool value) => Push(new SettingsPatch { Autostart = value });

    partial void OnNotificationsEnabledChanged(bool value) => Push(new SettingsPatch { NotificationsEnabled = value });

    partial void OnNotificationSoundChanged(bool value) => Push(new SettingsPatch { NotificationSound = value });

    partial void OnShowPreviewChanged(bool value) => Push(new SettingsPatch { ShowPreview = value });

    partial void OnCacheDaysIndexChanged(int value)
    {
        if (value >= 0 && value < s_cacheDays.Length) Push(new SettingsPatch { CacheDays = s_cacheDays[value] });
    }

    partial void OnThemeIndexChanged(int value)
    {
        if (_applying || value < 0 || value >= s_themes.Length) return;
        var theme = s_themes[value];
        // Apply immediately (live), then persist.
        if (_main.CurrentSettings is { } current)
        {
            var preview = current.Clone();
            preview.Theme = theme;
            _main.ApplySettingsLocally(preview);
        }
        else
        {
            ThemeHelper.Apply(theme);
        }
        Push(new SettingsPatch { Theme = theme });
    }

    partial void OnLanguageIndexChanged(int value)
    {
        if (_applying || value < 0 || value >= s_languages.Length) return;
        var language = s_languages[value];
        ShowLanguageRestartHint = language != _startupLanguage;
        if (UiStateStore.PersistPreferences)
        {
            UiStateStore.Current.Language = language;
            UiStateStore.Save();
        }
        Push(new SettingsPatch { Language = language });
    }

    private async void Push(SettingsPatch patch)
    {
        if (_applying) return;
        try
        {
            var settings = await Client.SetSettingsAsync(patch);
            _main.ApplySettingsLocally(settings);
        }
        catch (Exception ex)
        {
            _main.ShowError(Loc.Get("Error_Settings"), ex);
        }
    }

    public void SaveOAuth() => Push(new SettingsPatch
    {
        GoogleClientId = GoogleClientId.Trim(),
        GoogleClientSecret = GoogleClientSecret.Trim(),
        MicrosoftClientId = MicrosoftClientId.Trim(),
    });

    public async Task UpdateAccountAsync(UpdateAccountParams request)
    {
        try
        {
            await Client.UpdateAccountAsync(request);
        }
        catch (Exception ex)
        {
            _main.ShowError(Loc.Get("Error_Settings"), ex);
        }
    }

    public async Task RemoveAccountAsync(AccountSettingsItem item)
    {
        try
        {
            await Client.RemoveAccountAsync(item.Id);
        }
        catch (Exception ex)
        {
            _main.ShowError(Loc.Get("Error_RemoveAccount"), ex);
        }
    }

    // ------------------------------------------------------------------ notifications

    public async Task PauseAsync(TimeSpan? duration, bool untilTomorrow)
    {
        long? until = null;
        if (untilTomorrow)
            until = new DateTimeOffset(DateTime.Today.AddDays(1).AddHours(8)).ToUnixTimeMilliseconds();
        else if (duration is TimeSpan d)
            until = DateTimeOffset.UtcNow.Add(d).ToUnixTimeMilliseconds();
        try
        {
            await Client.PauseNotificationsAsync(until);
            UpdateQuiet(until);
        }
        catch (Exception ex)
        {
            _main.ShowError(Loc.Get("Error_Settings"), ex);
        }
    }

    public async Task TestNotificationAsync()
    {
        IsTestingNotification = true;
        try
        {
            var status = await Client.TestNotificationAsync();
            ShowNotificationsDisabledHint = status.StartsWith("disabled", StringComparison.OrdinalIgnoreCase);
            NotificationTestStatus = ShowNotificationsDisabledHint
                ? Loc.Get("Settings_TestDisabled") + " (" + status + ")"
                : Loc.Get("Settings_TestSent") + " (" + status + ")";
        }
        catch (Exception ex)
        {
            NotificationTestStatus = ex.Message;
            ShowNotificationsDisabledHint = false;
        }
        finally
        {
            IsTestingNotification = false;
        }
    }

    // ------------------------------------------------------------------ about

    public async Task RefreshStatusAsync()
    {
        UiMemory = Fmt.Megabytes(Environment.WorkingSet);
        try
        {
            var status = await Client.GetStatusAsync();
            OAuthGoogleBuiltIn = status.OAuthBuiltIn?.Google ?? false;
            OAuthMicrosoftBuiltIn = status.OAuthBuiltIn?.Microsoft ?? false;
            DaemonVersion = status.Version;
            DaemonPid = status.Pid.ToString(CultureInfo.CurrentCulture);
            DaemonUptime = Fmt.Duration(status.UptimeSec);
            DaemonMemory = Fmt.Megabytes(status.MemoryBytes);
        }
        catch (Exception)
        {
            DaemonVersion = Loc.Get("Settings_DaemonUnavailable");
            DaemonPid = "–";
            DaemonUptime = "–";
            DaemonMemory = "–";
        }
    }

    public async Task ShutdownDaemonAsync()
    {
        try
        {
            await Client.ShutdownAsync();
        }
        catch (Exception ex)
        {
            _main.ShowError(Loc.Get("Error_Settings"), ex);
        }
    }
}

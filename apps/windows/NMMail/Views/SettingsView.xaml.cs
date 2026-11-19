using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NMMail.Models;
using NMMail.Services;
using NMMail.ViewModels;
using Windows.System;

namespace NMMail.Views;

/// <summary>Settings shown inside the main window (replaces the three panes while open).</summary>
public sealed partial class SettingsView : UserControl
{
    public SettingsView()
    {
        InitializeComponent();
        VersionLabel.Text = Loc.Get("Settings_VersionLabel") + " ";
    }

    public SettingsViewModel ViewModel { get; set; } = null!;

    public event EventHandler? CloseRequested;

    public void ShowSection(int index)
    {
        if (index >= 0 && index < Sections.Items.Count) Sections.SelectedItem = Sections.Items[index];
    }

    private void Sections_SelectionChanged(SelectorBar sender, SelectorBarSelectionChangedEventArgs args)
    {
        var index = sender.Items.IndexOf(sender.SelectedItem);
        if (index < 0 || ViewModel is null) return;
        ViewModel.SectionIndex = index;
        if (index == 5) _ = ViewModel.RefreshStatusAsync();
    }

    private void Back_Click(object sender, RoutedEventArgs e) => CloseRequested?.Invoke(this, EventArgs.Empty);

    private void AddGmail_Click(object sender, RoutedEventArgs e) => _ = AccountFlows.AddOAuthAsync(XamlRoot, ViewModel.Main, Providers.Gmail);

    private void AddOutlook_Click(object sender, RoutedEventArgs e) => _ = AccountFlows.AddOAuthAsync(XamlRoot, ViewModel.Main, Providers.Outlook);

    private void AddImap_Click(object sender, RoutedEventArgs e) => _ = AccountFlows.AddImapAsync(XamlRoot, ViewModel.Main);

    private void Reauth_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.Tag is AccountSettingsItem item)
            _ = AccountFlows.ReauthenticateAsync(XamlRoot, ViewModel.Main, item.Account);
    }

    private async void RemoveAccount_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.Tag is not AccountSettingsItem item) return;
        var confirmed = await AccountFlows.ConfirmAsync(XamlRoot,
            Loc.Get("Settings_RemoveTitle"),
            Loc.Format("Settings_RemoveMessage", item.Account.Email),
            Loc.Get("Settings_RemoveConfirm"));
        if (confirmed) await ViewModel.RemoveAccountAsync(item);
    }

    private void PauseHour_Click(object sender, RoutedEventArgs e) => _ = ViewModel.PauseAsync(TimeSpan.FromHours(1), untilTomorrow: false);

    private void PauseTomorrow_Click(object sender, RoutedEventArgs e) => _ = ViewModel.PauseAsync(null, untilTomorrow: true);

    private void Resume_Click(object sender, RoutedEventArgs e) => _ = ViewModel.PauseAsync(null, untilTomorrow: false);

    private void TestNotification_Click(object sender, RoutedEventArgs e) => _ = ViewModel.TestNotificationAsync();

    private async void OpenNotificationSettings_Click(object sender, RoutedEventArgs e) =>
        await Launcher.LaunchUriAsync(new Uri("ms-settings:notifications"));

    private void SaveOAuth_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.SaveOAuth();
        ViewModel.Main.ShowNotice(Loc.Get("Settings_OAuthSaved"), "", InfoBarSeverity.Success);
    }

    private void RefreshStatus_Click(object sender, RoutedEventArgs e) => _ = ViewModel.RefreshStatusAsync();

    private async void Shutdown_Click(object sender, RoutedEventArgs e)
    {
        var confirmed = await AccountFlows.ConfirmAsync(XamlRoot,
            Loc.Get("Confirm_ShutdownTitle"), Loc.Get("Confirm_ShutdownMessage"), Loc.Get("Confirm_ShutdownButton"));
        if (confirmed) await ViewModel.ShutdownDaemonAsync();
    }
}

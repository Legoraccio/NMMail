using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NMMail.Models;
using NMMail.Services;
using NMMail.ViewModels;
using Windows.System;

namespace NMMail.Views;

/// <summary>Dialog flows shared by Settings and the sidebar ("Accedi di nuovo").</summary>
public static class AccountFlows
{
    private static bool s_dialogOpen;

    /// <summary>accounts.addOAuth: "Completa l'accesso nel browser…" with auth.progress and a Cancel button (accounts.cancelAuth).</summary>
    public static async Task AddOAuthAsync(XamlRoot root, MainViewModel main, string provider)
    {
        if (s_dialogOpen) return;
        s_dialogOpen = true;
        var client = main.Client;
        var providerName = provider == Providers.Gmail ? "Gmail" : "Outlook";
        string? url = null;

        var status = new TextBlock { Text = Loc.Get("Auth_Starting"), TextWrapping = TextWrapping.Wrap, VerticalAlignment = VerticalAlignment.Center };
        var ring = new ProgressRing { IsActive = true, Width = 28, Height = 28 };
        var reopen = new HyperlinkButton { Content = Loc.Get("Auth_OpenAgain"), Visibility = Visibility.Collapsed, Padding = new Thickness(0) };
        reopen.Click += async (_, _) =>
        {
            if (url is not null && Uri.TryCreate(url, UriKind.Absolute, out var uri)) await Launcher.LaunchUriAsync(uri);
        };
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
        row.Children.Add(ring);
        row.Children.Add(status);
        var panel = new StackPanel { Spacing = 12, MinWidth = 360 };
        panel.Children.Add(row);
        panel.Children.Add(reopen);

        var dialog = new ContentDialog
        {
            XamlRoot = root,
            RequestedTheme = (root.Content as FrameworkElement)?.RequestedTheme ?? ElementTheme.Default,
            Title = Loc.Format("Auth_Title", providerName),
            Content = panel,
            CloseButtonText = Loc.Get("Common_Cancel"),
            DefaultButton = ContentDialogButton.Close,
        };

        void OnProgress(object? sender, AuthProgressEvent e)
        {
            status.Text = e.Stage switch
            {
                AuthStages.Browser => Loc.Get("Auth_Browser"),
                AuthStages.Exchanging => Loc.Get("Auth_Exchanging"),
                AuthStages.Connecting => Loc.Get("Auth_Connecting"),
                AuthStages.Done => Loc.Get("Auth_Done"),
                _ => e.Stage,
            };
            if (!string.IsNullOrEmpty(e.Url))
            {
                url = e.Url;
                reopen.Visibility = Visibility.Visible;
            }
        }

        client.AuthProgress += OnProgress;
        try
        {
            var add = client.AddOAuthAccountAsync(provider);
            var show = dialog.ShowAsync().AsTask();
            var first = await Task.WhenAny(add, show);
            if (first == show && !add.IsCompleted)
            {
                // Closed by the user: stop the loopback listener in the daemon.
                try { await client.CancelAuthAsync(); } catch (Exception ex) { AppInfo.Log("cancelAuth: " + ex.Message); }
                try { await add; } catch { /* cancelled */ }
                return;
            }
            dialog.Hide();
            var account = await add;
            main.ShowNotice(Loc.Format("Auth_Added", account.Email), "", InfoBarSeverity.Success);
        }
        catch (DaemonException ex) when (ex.Code == DaemonErrorCodes.Cancelled)
        {
            dialog.Hide();
        }
        catch (Exception ex)
        {
            dialog.Hide();
            main.ShowError(Loc.Get("Error_AddAccount"), ex);
        }
        finally
        {
            client.AuthProgress -= OnProgress;
            s_dialogOpen = false;
        }
    }

    public static async Task AddImapAsync(XamlRoot root, MainViewModel main, Account? existing = null)
    {
        if (s_dialogOpen) return;
        s_dialogOpen = true;
        try
        {
            var dialog = new ImapAccountDialog(main.Client, existing)
            {
                XamlRoot = root,
                RequestedTheme = (root.Content as FrameworkElement)?.RequestedTheme ?? ElementTheme.Default,
            };
            await dialog.ShowAsync();
            if (dialog.Result is { } account)
                main.ShowNotice(Loc.Format("Auth_Added", account.Email), "", InfoBarSeverity.Success);
        }
        finally
        {
            s_dialogOpen = false;
        }
    }

    /// <summary>"Accedi di nuovo": OAuth providers repeat the browser flow, IMAP accounts re-enter the password.</summary>
    public static Task ReauthenticateAsync(XamlRoot root, MainViewModel main, Account account) =>
        account.Provider is Providers.Gmail or Providers.Outlook
            ? AddOAuthAsync(root, main, account.Provider)
            : AddImapAsync(root, main, account);

    public static async Task<bool> ConfirmAsync(XamlRoot root, string title, string message, string primary)
    {
        if (s_dialogOpen) return false;
        s_dialogOpen = true;
        try
        {
            var dialog = new ContentDialog
            {
                XamlRoot = root,
                RequestedTheme = (root.Content as FrameworkElement)?.RequestedTheme ?? ElementTheme.Default,
                Title = title,
                Content = new TextBlock { Text = message, TextWrapping = TextWrapping.Wrap },
                PrimaryButtonText = primary,
                CloseButtonText = Loc.Get("Common_Cancel"),
                DefaultButton = ContentDialogButton.Close,
            };
            return await dialog.ShowAsync() == ContentDialogResult.Primary;
        }
        finally
        {
            s_dialogOpen = false;
        }
    }
}

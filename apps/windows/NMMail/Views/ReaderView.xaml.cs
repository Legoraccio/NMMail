using System.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Web.WebView2.Core;
using NMMail.Helpers;
using NMMail.Models;
using NMMail.Services;
using NMMail.ViewModels;
using Windows.System;

namespace NMMail.Views;

/// <summary>
/// Reading pane. One WebView2 instance for the whole session, locked down: no scripts, no dev tools,
/// no host objects/web messages, every navigation cancelled (links open in the default browser) and every
/// http(s) request blocked unless the user allowed remote content for the current message.
/// </summary>
public sealed partial class ReaderView : UserControl
{
    private const string LargeDocumentOrigin = "https://message.nmmail.invalid/";
    private const int MaxNavigateToStringBytes = 1_900_000; // NavigateToString limit is 2 MB

    private static readonly HashSet<string> s_contextMenuWhitelist =
        ["copy", "selectAll", "copyLinkLocation", "copyImage", "copyImageLocation", "print"];

    private WebView2 _web;
    private Task? _initTask;
    private CoreWebView2Environment? _environment;
    private MessageDetail? _pending;
    private byte[]? _largeDocument;
    private bool _remoteAllowed;
    private bool _allowNextNavigation;

    public ReaderView()
    {
        InitializeComponent();
        _web = BodyView;
    }

    public MainViewModel Main { get; private set; } = null!;
    public ReaderViewModel ViewModel { get; private set; } = null!;

    /// <summary>The (single) WebView2 currently used for message bodies.</summary>
    internal WebView2 BodyWebView => _web;

    /// <summary>Owner window id (for pickers).</summary>
    public Func<Microsoft.UI.WindowId>? OwnerWindowId { get; set; }

    public void Initialize(MainViewModel main)
    {
        Main = main;
        ViewModel = main.Reader;
        ViewModel.BodyReady += OnBodyReady;
        ToLabel.Text = Loc.Get("Reader_To") + " ";
        CcLabel.Text = Loc.Get("Reader_Cc") + " ";
    }

    /// <summary>Creates the WebView2 ahead of time (called at idle after startup) so the first message opens instantly.</summary>
    public void Prewarm() => _ = EnsureWebViewAsync();

    // ------------------------------------------------------------------ WebView2 setup

    private Task EnsureWebViewAsync() => _initTask ??= InitWebViewAsync();

    private async Task InitWebViewAsync()
    {
        _environment = await WebViewEnvironment.GetAsync();
        await _web.EnsureCoreWebView2Async(_environment);
        var core = _web.CoreWebView2;

        var settings = core.Settings;
        settings.IsScriptEnabled = false;
        settings.AreDevToolsEnabled = false;
        settings.AreHostObjectsAllowed = false;
        settings.IsWebMessageEnabled = false;
        settings.IsStatusBarEnabled = false;
        settings.AreDefaultScriptDialogsEnabled = false;
        settings.IsBuiltInErrorPageEnabled = false;
        settings.AreBrowserAcceleratorKeysEnabled = false;
        settings.IsPasswordAutosaveEnabled = false;
        settings.IsGeneralAutofillEnabled = false;
        settings.IsSwipeNavigationEnabled = false;
        settings.IsZoomControlEnabled = true;
        core.Profile.PreferredColorScheme = CoreWebView2PreferredColorScheme.Light;

        core.AddWebResourceRequestedFilter("*", CoreWebView2WebResourceContext.All);
        core.WebResourceRequested += OnWebResourceRequested;
        core.NavigationStarting += OnNavigationStarting;
        core.FrameNavigationStarting += OnFrameNavigationStarting;
        core.NewWindowRequested += OnNewWindowRequested;
        core.LaunchingExternalUriScheme += (_, e) => e.Cancel = true;
        core.DownloadStarting += (_, e) => e.Cancel = true;
        core.PermissionRequested += (_, e) => e.State = CoreWebView2PermissionState.Deny;
        core.ContextMenuRequested += OnContextMenuRequested;
        core.StatusBarTextChanged += OnStatusBarTextChanged;
        _web.CoreProcessFailed += OnCoreProcessFailed;
    }

    private void OnCoreProcessFailed(WebView2 sender, CoreWebView2ProcessFailedEventArgs args)
    {
        AppInfo.Log($"WebView2 process failed: {args.ProcessFailedKind}");
        if (args.ProcessFailedKind != CoreWebView2ProcessFailedKind.BrowserProcessExited)
        {
            // A renderer crashed: re-render the current message.
            if (ViewModel.Message is { } current) Render(current);
            return;
        }
        // The browser process is gone: this control is unusable, replace it.
        var host = (Grid)_web.Parent;
        var index = host.Children.IndexOf(_web);
        _web.Close();
        _web = new WebView2 { DefaultBackgroundColor = Microsoft.UI.Colors.White };
        host.Children.Insert(index, _web);
        host.Children.Remove(sender);
        _initTask = null;
        if (ViewModel.Message is { } message) OnBodyReady(this, message);
    }

    // ------------------------------------------------------------------ rendering

    private async void OnBodyReady(object? sender, MessageDetail detail)
    {
        _pending = detail;
        try
        {
            await EnsureWebViewAsync();
        }
        catch (Exception ex)
        {
            AppInfo.Log("WebView2 init failed: " + ex);
            _initTask = null;
            Main.ShowNotice(Loc.Get("Error_WebView"), ex.Message, InfoBarSeverity.Error, autoHide: false);
            return;
        }
        if (!ReferenceEquals(_pending, detail)) return; // a newer message is on its way
        _pending = null;
        Render(detail);
    }

    private void Render(MessageDetail detail)
    {
        _remoteAllowed = detail.RemoteAllowed;
        LinkPreview.Visibility = Visibility.Collapsed;
        var html = MessageHtml.Prepare(detail);
        _allowNextNavigation = true;
        if (Encoding.UTF8.GetByteCount(html) <= MaxNavigateToStringBytes)
        {
            _largeDocument = null;
            _web.NavigateToString(html);
        }
        else
        {
            // Too big for NavigateToString: serve it from WebResourceRequested on a reserved, unresolvable origin.
            _largeDocument = Encoding.UTF8.GetBytes(html);
            _web.CoreWebView2.Navigate(LargeDocumentOrigin + detail.Id);
        }
    }

    // ------------------------------------------------------------------ lockdown handlers

    private void OnNavigationStarting(CoreWebView2 sender, CoreWebView2NavigationStartingEventArgs args)
    {
        if (_allowNextNavigation && !args.IsUserInitiated)
        {
            _allowNextNavigation = false; // our own NavigateToString / large document
            return;
        }
        args.Cancel = true;
        if (args.IsUserInitiated) LaunchExternal(args.Uri);
    }

    private void OnFrameNavigationStarting(CoreWebView2 sender, CoreWebView2NavigationStartingEventArgs args)
    {
        if (!args.Uri.StartsWith("about:", StringComparison.OrdinalIgnoreCase)) args.Cancel = true;
    }

    private void OnNewWindowRequested(CoreWebView2 sender, CoreWebView2NewWindowRequestedEventArgs args)
    {
        args.Handled = true;
        if (args.IsUserInitiated) LaunchExternal(args.Uri);
    }

    private void OnWebResourceRequested(CoreWebView2 sender, CoreWebView2WebResourceRequestedEventArgs args)
    {
        var uri = args.Request.Uri;
        if (_largeDocument is { } document && uri.StartsWith(LargeDocumentOrigin, StringComparison.OrdinalIgnoreCase))
        {
            var stream = new MemoryStream(document, writable: false).AsRandomAccessStream();
            args.Response = _environment!.CreateWebResourceResponse(stream, 200, "OK", "Content-Type: text/html; charset=utf-8");
            return;
        }
        if (uri.StartsWith("http:", StringComparison.OrdinalIgnoreCase) || uri.StartsWith("https:", StringComparison.OrdinalIgnoreCase))
        {
            if (!_remoteAllowed || uri.StartsWith(LargeDocumentOrigin, StringComparison.OrdinalIgnoreCase))
                args.Response = _environment!.CreateWebResourceResponse(null, 403, "Blocked", "");
        }
    }

    private static void OnContextMenuRequested(CoreWebView2 sender, CoreWebView2ContextMenuRequestedEventArgs args)
    {
        var items = args.MenuItems;
        for (var i = items.Count - 1; i >= 0; i--)
        {
            if (!s_contextMenuWhitelist.Contains(items[i].Name)) items.RemoveAt(i);
        }
    }

    /// <summary>Custom "status bar": shows the real target of the hovered link (anti-phishing).</summary>
    private void OnStatusBarTextChanged(CoreWebView2 sender, object args)
    {
        var text = sender.StatusBarText;
        LinkPreviewText.Text = text;
        LinkPreview.Visibility = string.IsNullOrEmpty(text) ? Visibility.Collapsed : Visibility.Visible;
    }

    private static async void LaunchExternal(string uri)
    {
        if (!Uri.TryCreate(uri, UriKind.Absolute, out var parsed)) return;
        if (parsed.Scheme is not ("http" or "https" or "mailto")) return;
        try
        {
            await Launcher.LaunchUriAsync(parsed);
        }
        catch (Exception ex)
        {
            AppInfo.Log("LaunchUriAsync failed: " + ex.Message);
        }
    }

    // ------------------------------------------------------------------ commands

    private void Reply_Click(object sender, RoutedEventArgs e) => Main.Compose(ComposeModes.Reply);

    private void ReplyAll_Click(object sender, RoutedEventArgs e) => Main.Compose(ComposeModes.ReplyAll);

    private void Forward_Click(object sender, RoutedEventArgs e) => Main.Compose(ComposeModes.Forward);

    private void Archive_Click(object sender, RoutedEventArgs e) => _ = Main.ArchiveAsync(Main.ActionTargets());

    private void Delete_Click(object sender, RoutedEventArgs e) => _ = Main.DeleteAsync(Main.ActionTargets());

    private void ToggleRead_Click(object sender, RoutedEventArgs e) =>
        _ = Main.SetSeenAsync(Main.ActionTargets(), ViewModel.ItemIsUnread);

    private void Flag_Click(object sender, RoutedEventArgs e) => _ = Main.ToggleFlagAsync(Main.ActionTargets());

    private void MultiArchive_Click(object sender, RoutedEventArgs e) => _ = Main.ArchiveAsync(Main.SelectedItems);

    private void MultiDelete_Click(object sender, RoutedEventArgs e) => _ = Main.DeleteAsync(Main.SelectedItems);

    private void MultiRead_Click(object sender, RoutedEventArgs e) => _ = Main.SetSeenAsync(Main.SelectedItems, true);

    private void MultiFlag_Click(object sender, RoutedEventArgs e) => _ = Main.ToggleFlagAsync(Main.SelectedItems);

    private async void ShowImages_Click(object sender, RoutedEventArgs e) => await ViewModel.ShowRemoteContentAsync();

    private void MoveFlyout_Opening(object? sender, object e)
    {
        MoveFlyout.Items.Clear();
        var targets = Main.ActionTargets();
        var accounts = targets.Select(t => t.AccountId).Distinct().ToList();
        if (accounts.Count != 1) return;
        foreach (var folder in Main.MoveTargets(accounts[0]))
        {
            var item = new MenuFlyoutItem
            {
                Text = new string(' ', folder.Depth * 3) + folder.Title,
                Icon = new FontIcon { Glyph = folder.Glyph },
                Tag = folder.Folder!.Id,
            };
            item.Click += (s, e) =>
            {
                if ((s as FrameworkElement)?.Tag is long id) _ = Main.MoveAsync(Main.ActionTargets(), id);
            };
            MoveFlyout.Items.Add(item);
        }
    }

    // ------------------------------------------------------------------ attachments

    private async void Attachment_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.Tag is not AttachmentItem attachment) return;
        try
        {
            await AttachmentService.OpenAsync(Main.Client, attachment);
        }
        catch (BlockedAttachmentException)
        {
            Main.ShowNotice(Loc.Get("Attachment_BlockedTitle"), Loc.Get("Attachment_BlockedMessage"), InfoBarSeverity.Warning, autoHide: false);
        }
        catch (Exception ex)
        {
            Main.ShowError(Loc.Get("Error_Attachment"), ex);
        }
    }

    private async void AttachmentSaveAs_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.Tag is not AttachmentItem attachment || OwnerWindowId is null) return;
        try
        {
            var saved = await AttachmentService.SaveAsAsync(Main.Client, attachment, OwnerWindowId());
            if (saved is not null) Main.ShowNotice(Loc.Get("Attachment_Saved"), saved, InfoBarSeverity.Success);
        }
        catch (Exception ex)
        {
            Main.ShowError(Loc.Get("Error_Attachment"), ex);
        }
    }
}

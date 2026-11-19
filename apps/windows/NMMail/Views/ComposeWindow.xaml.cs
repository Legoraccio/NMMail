using System.Net;
using System.Reflection;
using System.Security.Cryptography;
using System.Text.Json;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.Web.WebView2.Core;
using Microsoft.Windows.Storage.Pickers;
using NMMail.Helpers;
using NMMail.Models;
using NMMail.Services;
using NMMail.ViewModels;
using Windows.Graphics;

namespace NMMail.Views;

/// <summary>
/// Compose window (Mica, own title bar). The body editor is a second WebView2 hosting our own contenteditable page:
/// scripts are allowed only there, under a nonce-based CSP, with every http(s) load and navigation blocked.
/// </summary>
public sealed partial class ComposeWindow : Window
{
    private static readonly List<ComposeWindow> s_open = [];
    private static string? s_editorTemplate;

    private readonly MainViewModel _main;
    private readonly string _nonce = Convert.ToHexString(RandomNumberGenerator.GetBytes(16));
    private readonly DispatcherQueueTimer _suggestTimer;
    private AutoSuggestBox? _suggestTarget;
    private CancellationTokenSource? _suggestCts;
    private bool _closeConfirmed;
    private bool _editorReady;
    private bool _allowNavigation;

    public ComposeWindow(MainViewModel main, ComposeRequest request)
    {
        _main = main;
        ViewModel = new ComposeViewModel(main.Client, request, main.Accounts);
        InitializeComponent();

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(ComposeTitleBar);
        AppWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;
        if (File.Exists(AppInfo.IconPath)) AppWindow.SetIcon(AppInfo.IconPath);
        PlaceWindow();
        ThemeHelper.Register(this);

        _suggestTimer = DispatcherQueue.CreateTimer();
        _suggestTimer.Interval = TimeSpan.FromMilliseconds(150);
        _suggestTimer.IsRepeating = false;
        _suggestTimer.Tick += OnSuggestTimer;

        AppWindow.Closing += OnClosing;
        Closed += (_, _) =>
        {
            s_open.Remove(this);
            _suggestCts?.Cancel();
            Editor.Close();
        };
        s_open.Add(this);
        _ = InitializeAsync();
    }

    public static IReadOnlyList<ComposeWindow> OpenWindows => s_open;

    public ComposeViewModel ViewModel { get; }

#if DEBUG
    internal FrameworkElement SnapshotRoot => RootGrid;
    internal WebView2[] SnapshotWebViews => [Editor];
#endif

    private void PlaceWindow()
    {
        var scale = NativeMethods.GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this)) / 96.0;
        if (scale <= 0) scale = 1;
        var width = (int)(880 * scale);
        var height = (int)(740 * scale);
        var owner = App.Current.MainWindow?.AppWindow.Id ?? AppWindow.Id;
        var area = DisplayArea.GetFromWindowId(owner, DisplayAreaFallback.Nearest).WorkArea;
        width = Math.Min(width, area.Width);
        height = Math.Min(height, area.Height);
        // Cascade new windows slightly so they don't stack exactly.
        var offset = (int)(28 * scale * (s_open.Count % 5));
        AppWindow.MoveAndResize(new RectInt32(
            area.X + (area.Width - width) / 2 + offset,
            area.Y + (area.Height - height) / 2 + offset,
            width, height));
    }

    private async Task InitializeAsync()
    {
        try
        {
            await ViewModel.PrepareAsync();
        }
        catch (Exception ex)
        {
            ViewModel.ShowError(Loc.Get("Compose_ErrorPrepare") + " " + ex.Message);
        }

        try
        {
            await InitEditorAsync();
        }
        catch (Exception ex)
        {
            AppInfo.Log("editor init failed: " + ex);
            ViewModel.ShowError(Loc.Get("Error_WebView") + " " + ex.Message);
            return;
        }

        if (string.IsNullOrWhiteSpace(ViewModel.To)) ToBox.Focus(FocusState.Programmatic);
        else
        {
            Editor.Focus(FocusState.Programmatic);
            _ = ExecAsync("nm.focus()");
        }
    }

    // ------------------------------------------------------------------ editor (WebView2)

    private async Task InitEditorAsync()
    {
        var environment = await WebViewEnvironment.GetAsync();
        await Editor.EnsureCoreWebView2Async(environment);
        var core = Editor.CoreWebView2;
        var settings = core.Settings;
        settings.IsScriptEnabled = true; // only our own page, nonce-protected
        settings.AreDevToolsEnabled = false;
        settings.AreHostObjectsAllowed = false;
        settings.IsWebMessageEnabled = true; // used only for Ctrl+Enter → "send"
        settings.IsStatusBarEnabled = false;
        settings.AreDefaultScriptDialogsEnabled = false;
        settings.IsBuiltInErrorPageEnabled = false;
        settings.AreBrowserAcceleratorKeysEnabled = false;
        settings.IsPasswordAutosaveEnabled = false;
        settings.IsGeneralAutofillEnabled = false;
        settings.IsSwipeNavigationEnabled = false;
        core.Profile.PreferredColorScheme = CoreWebView2PreferredColorScheme.Light;

        core.AddWebResourceRequestedFilter("*", CoreWebView2WebResourceContext.All);
        core.WebResourceRequested += (s, e) =>
        {
            var uri = e.Request.Uri;
            if (uri.StartsWith("http:", StringComparison.OrdinalIgnoreCase) || uri.StartsWith("https:", StringComparison.OrdinalIgnoreCase))
                e.Response = environment.CreateWebResourceResponse(null, 403, "Blocked", "");
        };
        core.NavigationStarting += (s, e) =>
        {
            if (_allowNavigation && !e.IsUserInitiated)
            {
                _allowNavigation = false;
                return;
            }
            e.Cancel = true;
        };
        core.FrameNavigationStarting += (s, e) => e.Cancel = !e.Uri.StartsWith("about:", StringComparison.OrdinalIgnoreCase);
        core.NewWindowRequested += (s, e) => e.Handled = true;
        core.LaunchingExternalUriScheme += (s, e) => e.Cancel = true;
        core.DownloadStarting += (s, e) => e.Cancel = true;
        core.PermissionRequested += (s, e) => e.State = CoreWebView2PermissionState.Deny;
        core.WebMessageReceived += (s, e) =>
        {
            if (e.TryGetWebMessageAsString() == "send") _ = SendAsync();
        };

        var loaded = new TaskCompletionSource();
        void OnCompleted(CoreWebView2 sender, CoreWebView2NavigationCompletedEventArgs args)
        {
            core.NavigationCompleted -= OnCompleted;
            loaded.TrySetResult();
        }
        core.NavigationCompleted += OnCompleted;
        _allowNavigation = true;
        Editor.NavigateToString(BuildEditorPage());
        await loaded.Task;

        await ExecAsync("nm.setContent(" + JsonSerializer.Serialize(ViewModel.InitialHtml, IpcJson.Default.String) + ")");
        _editorReady = true;
    }

    private string BuildEditorPage()
    {
        if (s_editorTemplate is null)
        {
            using var stream = typeof(ComposeWindow).Assembly.GetManifestResourceStream("NMMail.Assets.editor.html")
                               ?? throw new InvalidOperationException("editor.html resource missing");
            using var reader = new StreamReader(stream);
            s_editorTemplate = reader.ReadToEnd();
        }
        return s_editorTemplate
            .Replace("{{NONCE}}", _nonce)
            .Replace("{{LANG}}", Loc.LanguageCode)
            .Replace("{{PLACEHOLDER}}", WebUtility.HtmlEncode(Loc.Get("Compose_Placeholder")));
    }

    private async Task<string> ExecAsync(string script) =>
        Editor.CoreWebView2 is null ? "" : await Editor.CoreWebView2.ExecuteScriptAsync(script);

    private async Task<string> GetEditorStringAsync(string script)
    {
        var json = await ExecAsync(script);
        return string.IsNullOrEmpty(json) ? "" : JsonSerializer.Deserialize(json, IpcJson.Default.String) ?? "";
    }

    private async Task<bool> EditorChangedAsync()
    {
        if (!_editorReady) return false;
        try
        {
            return await ExecAsync("nm.isDirty()") == "true";
        }
        catch
        {
            return false;
        }
    }

    private void Format_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.Tag is string command && _editorReady)
            _ = ExecAsync("nm.exec(" + JsonSerializer.Serialize(command, IpcJson.Default.String) + ")");
    }

    private void ClearFormatting_Click(object sender, RoutedEventArgs e)
    {
        if (_editorReady) _ = ExecAsync("nm.clearFormatting()");
    }

    private async void Link_Click(object sender, RoutedEventArgs e)
    {
        if (!_editorReady) return;
        var input = new TextBox { PlaceholderText = "https://", MinWidth = 360, IsSpellCheckEnabled = false };
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            RequestedTheme = RootGrid.RequestedTheme,
            Title = Loc.Get("Compose_LinkTitle"),
            Content = input,
            PrimaryButtonText = Loc.Get("Compose_LinkInsert"),
            CloseButtonText = Loc.Get("Common_Cancel"),
            DefaultButton = ContentDialogButton.Primary,
        };
        if (await dialog.ShowAsync() != ContentDialogResult.Primary || string.IsNullOrWhiteSpace(input.Text)) return;
        await ExecAsync("nm.link(" + JsonSerializer.Serialize(input.Text.Trim(), IpcJson.Default.String) + ")");
    }

    // ------------------------------------------------------------------ recipients

    private void Recipient_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        if (args.Reason != AutoSuggestionBoxTextChangeReason.UserInput) return;
        _suggestTarget = sender;
        _suggestTimer.Stop();
        _suggestTimer.Start();
    }

    private async void OnSuggestTimer(DispatcherQueueTimer timer, object args)
    {
        var box = _suggestTarget;
        if (box is null) return;
        var token = LastToken(box.Text);
        _suggestCts?.Cancel();
        if (token.Length < 1)
        {
            ViewModel.Suggestions.Clear();
            box.IsSuggestionListOpen = false;
            return;
        }
        var cts = _suggestCts = new CancellationTokenSource();
        try
        {
            var contacts = await ViewModel.Client.SuggestContactsAsync(token, 8, cts.Token);
            if (cts.IsCancellationRequested || !ReferenceEquals(box, _suggestTarget)) return;
            ViewModel.SetSuggestions(contacts);
            box.IsSuggestionListOpen = contacts.Count > 0;
        }
        catch (Exception ex) when (ex is OperationCanceledException or DaemonException)
        {
        }
    }

    private void Recipient_QuerySubmitted(AutoSuggestBox sender, AutoSuggestBoxQuerySubmittedEventArgs args)
    {
        if (args.ChosenSuggestion is not Addr addr) return;
        var text = sender.Text;
        var cut = Math.Max(text.LastIndexOf(','), text.LastIndexOf(';'));
        var prefix = cut >= 0 ? text[..(cut + 1)] + " " : "";
        sender.Text = prefix + addr + ", ";
        ViewModel.Suggestions.Clear();
        sender.IsSuggestionListOpen = false;
        if (VisualTree.FindDescendant<TextBox>(sender) is { } inner) inner.SelectionStart = inner.Text.Length;
    }

    private static string LastToken(string text)
    {
        var cut = Math.Max(text.LastIndexOf(','), text.LastIndexOf(';'));
        return (cut >= 0 ? text[(cut + 1)..] : text).Trim();
    }

    private void ShowCcBcc_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.ShowCcBcc = true;
        CcBox.Focus(FocusState.Programmatic);
    }

    // ------------------------------------------------------------------ attachments

    private async void Attach_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var picker = new FileOpenPicker(AppWindow.Id) { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
            picker.FileTypeFilter.Add("*");
            var files = await picker.PickMultipleFilesAsync();
            if (files is null) return;
            ViewModel.AddFiles(files.Select(f => f.Path));
        }
        catch (Exception ex)
        {
            ViewModel.ShowError(ex.Message);
        }
    }

    private void RemoveAttachment_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.Tag is ComposeAttachmentItem item) ViewModel.RemoveAttachment(item);
    }

    // ------------------------------------------------------------------ send / close

    private void Send_Click(object sender, RoutedEventArgs e) => _ = SendAsync();

    private void SendAccelerator_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        _ = SendAsync();
    }

    private async Task SendAsync()
    {
        if (ViewModel.IsBusy || !_editorReady) return;
        ViewModel.IsErrorOpen = false;
        try
        {
            var html = await GetEditorStringAsync("nm.getHtml()");
            var text = await GetEditorStringAsync("nm.getText()");
            var request = ViewModel.BuildSendRequest(html, text);
            ViewModel.IsBusy = true;
            var result = await ViewModel.Client.SendAsync(request);
            if (result.Queued)
                _main.ShowNotice(Loc.Get("Notice_Queued"), "", InfoBarSeverity.Informational);
            _closeConfirmed = true;
            Close();
        }
        catch (Exception ex)
        {
            // Validation and permanent send errors (e.g. rejected recipient): keep the draft open.
            ViewModel.ShowError(ex.Message);
        }
        finally
        {
            ViewModel.IsBusy = false;
        }
    }

    public async Task<bool> HasUnsavedChangesAsync() => ViewModel.IsDirty(await EditorChangedAsync());

    /// <summary>Closes without asking (used after the main window confirmed exiting).</summary>
    public void ForceClose()
    {
        _closeConfirmed = true;
        Close();
    }

    private async void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (_closeConfirmed) return;
        args.Cancel = true;
        if (await HasUnsavedChangesAsync())
        {
            var dialog = new ContentDialog
            {
                XamlRoot = Content.XamlRoot,
                RequestedTheme = RootGrid.RequestedTheme,
                Title = Loc.Get("Compose_DiscardTitle"),
                Content = Loc.Get("Compose_DiscardMessage"),
                PrimaryButtonText = Loc.Get("Compose_DiscardConfirm"),
                CloseButtonText = Loc.Get("Compose_KeepEditing"),
                DefaultButton = ContentDialogButton.Close,
            };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
        }
        _closeConfirmed = true;
        Close();
    }
}

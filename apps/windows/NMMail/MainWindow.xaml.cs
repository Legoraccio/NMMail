using System.ComponentModel;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using NMMail.Models;
using NMMail.Services;
using NMMail.ViewModels;
using NMMail.Views;
using Windows.Graphics;
using WinRT.Interop;
using VirtualKey = Windows.System.VirtualKey;
using VirtualKeyModifiers = Windows.System.VirtualKeyModifiers;

namespace NMMail;

/// <summary>Main window: custom title bar with search, sidebar | message list | reader, settings overlay.</summary>
public sealed partial class MainWindow : Window
{
    private const double DefaultListWidth = 400;

    private readonly DispatcherQueueTimer _searchTimer;
    private readonly nint _hwnd;
    private SettingsViewModel? _settingsViewModel;
    private RectInt32? _normalBounds;
    private bool _exitConfirmed;
    private bool _sidebarCollapsed;
    private bool _prewarmScheduled;
    private bool _firstDataLogged;
    private DispatcherQueueTimer? _prewarmTimer;

    public MainWindow(MainViewModel viewModel)
    {
        ViewModel = viewModel;
        InitializeComponent();
        _hwnd = WindowNative.GetWindowHandle(this);

        Title = "NM Mail";
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        AppWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;
        if (File.Exists(AppInfo.IconPath)) AppWindow.SetIcon(AppInfo.IconPath);
        if (AppWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.PreferredMinimumWidth = 760;
            presenter.PreferredMinimumHeight = 480;
        }
        RestorePlacement();
        ThemeHelper.Register(this);

        Sidebar.ViewModel = viewModel;
        Sidebar.SettingsRequested += (_, _) => ShowSettings(0);
        MessageListPane.Initialize(viewModel);
        MessageListPane.AddAccountRequested += (_, _) => ShowSettings(0);
        Reader.Initialize(viewModel);
        Reader.OwnerWindowId = () => AppWindow.Id;
        SetupPanes();

        _searchTimer = DispatcherQueue.CreateTimer();
        _searchTimer.Interval = TimeSpan.FromMilliseconds(250);
        _searchTimer.IsRepeating = false;
        _searchTimer.Tick += (_, _) => _ = ViewModel.SearchAsync(SearchBox.Text);

        viewModel.ComposeRequested += OnComposeRequested;
        viewModel.BringToFrontRequested += (_, _) => BringToFront();
        viewModel.ReauthRequested += (_, account) => _ = AccountFlows.ReauthenticateAsync(Content.XamlRoot, ViewModel, account);
        viewModel.SettingsApplied += OnSettingsApplied;
        viewModel.ListReloaded += OnListReloaded;
        viewModel.PropertyChanged += OnViewModelPropertyChanged;

        AppWindow.Changed += OnAppWindowChanged;
        AppWindow.Closing += OnClosing;
        Activated += (_, e) =>
        {
            if (e.WindowActivationState != WindowActivationState.Deactivated) ViewModel.OnActivated();
        };
        Closed += OnClosed;
    }

    public MainViewModel ViewModel { get; }

#if DEBUG
    internal FrameworkElement SnapshotRoot => RootGrid;
    internal WebView2[] SnapshotWebViews => [Reader.BodyWebView];
#endif

    private bool IsSettingsOpen => SettingsHost.Visibility == Visibility.Visible;

    // ------------------------------------------------------------------ window placement

    private double Scale => Math.Max(1, NativeMethods.GetDpiForWindow(_hwnd)) / 96.0;

    private void RestorePlacement()
    {
        var state = UiStateStore.Current;
        if (state.WindowX is int x && state.WindowY is int y && state.WindowWidth is int w && state.WindowHeight is int h && w > 300 && h > 200)
        {
            var rect = new RectInt32(x, y, w, h);
            if (DisplayArea.GetFromRect(rect, DisplayAreaFallback.None) is not null)
            {
                AppWindow.MoveAndResize(rect);
                _normalBounds = rect;
                return;
            }
        }
        var area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        var width = Math.Min((int)(1320 * Scale), area.Width);
        var height = Math.Min((int)(840 * Scale), area.Height);
        var bounds = new RectInt32(area.X + (area.Width - width) / 2, area.Y + (area.Height - height) / 2, width, height);
        AppWindow.MoveAndResize(bounds);
        _normalBounds = bounds;
    }

    /// <summary>Called right after the first Activate().</summary>
    public void ApplyInitialState()
    {
        if (UiStateStore.Current.Maximized && AppWindow.Presenter is OverlappedPresenter presenter) presenter.Maximize();
    }

    private void OnAppWindowChanged(AppWindow sender, AppWindowChangedEventArgs args)
    {
        if (!args.DidPositionChange && !args.DidSizeChange) return;
        if (sender.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Restored })
            _normalBounds = new RectInt32(sender.Position.X, sender.Position.Y, sender.Size.Width, sender.Size.Height);
    }

    private void SavePlacement()
    {
        var state = UiStateStore.Current;
        state.Maximized = AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Maximized };
        if (_normalBounds is { } bounds)
        {
            state.WindowX = bounds.X;
            state.WindowY = bounds.Y;
            state.WindowWidth = bounds.Width;
            state.WindowHeight = bounds.Height;
        }
        state.SidebarCollapsed = _sidebarCollapsed;
        if (!_sidebarCollapsed && SidebarColumn.ActualWidth > 0) state.SidebarWidth = SidebarColumn.ActualWidth;
        if (ListColumn.ActualWidth > 0) state.ListWidth = ListColumn.ActualWidth;
        UiStateStore.Save();
    }

    public void BringToFront()
    {
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized } presenter) presenter.Restore();
        AppWindow.Show();
        Activate();
        if (!NativeMethods.SetForegroundWindow(_hwnd))
        {
            var info = new NativeMethods.FLASHWINFO
            {
                cbSize = (uint)System.Runtime.InteropServices.Marshal.SizeOf<NativeMethods.FLASHWINFO>(),
                hwnd = _hwnd,
                dwFlags = NativeMethods.FLASHW_ALL | NativeMethods.FLASHW_TIMERNOFG,
                uCount = 3,
            };
            NativeMethods.FlashWindowEx(ref info);
        }
    }

    // ------------------------------------------------------------------ panes

    private void SetupPanes()
    {
        var state = UiStateStore.Current;
        SidebarColumn.Width = new GridLength(Math.Clamp(state.SidebarWidth, 200, 420));
        ListColumn.Width = new GridLength(Math.Clamp(state.ListWidth, 280, 720));

        SidebarSplitter.Target = SidebarColumn;
        SidebarSplitter.MinTargetWidth = 200;
        SidebarSplitter.MaxTargetWidth = 420;
        SidebarSplitter.ReservedWidth = 280 + 320;
        SidebarSplitter.ToggleRequested += (_, _) => SetSidebarCollapsed(true);
        SidebarSplitter.Resized += (_, _) => UiStateStore.Current.SidebarWidth = SidebarColumn.ActualWidth;
        AutomationProperties.SetName(SidebarSplitter, Loc.Get("Splitter_Sidebar"));

        ListSplitter.Target = ListColumn;
        ListSplitter.MinTargetWidth = 280;
        ListSplitter.MaxTargetWidth = 720;
        ListSplitter.ReservedWidth = 320;
        ListSplitter.ToggleRequested += (_, _) => ListColumn.Width = new GridLength(DefaultListWidth);
        ListSplitter.Resized += (_, _) => UiStateStore.Current.ListWidth = ListColumn.ActualWidth;
        AutomationProperties.SetName(ListSplitter, Loc.Get("Splitter_List"));

        if (state.SidebarCollapsed) SetSidebarCollapsed(true);
    }

    private void SetSidebarCollapsed(bool collapsed)
    {
        if (collapsed == _sidebarCollapsed) return;
        if (collapsed && SidebarColumn.ActualWidth > 0) UiStateStore.Current.SidebarWidth = SidebarColumn.ActualWidth;
        _sidebarCollapsed = collapsed;
        Sidebar.Visibility = collapsed ? Visibility.Collapsed : Visibility.Visible;
        SidebarSplitter.Visibility = Sidebar.Visibility;
        SidebarColumn.Width = collapsed
            ? new GridLength(0)
            : new GridLength(Math.Clamp(UiStateStore.Current.SidebarWidth, 200, 420));
    }

    private void AppTitleBar_PaneToggleRequested(TitleBar sender, object args) => SetSidebarCollapsed(!_sidebarCollapsed);

    // ------------------------------------------------------------------ settings overlay

    private void ShowSettings(int section)
    {
        if (_settingsView is null || _settingsViewModel is null)
        {
            _settingsViewModel = new SettingsViewModel(ViewModel);
            _settingsView = new SettingsView { ViewModel = _settingsViewModel };
            _settingsView.CloseRequested += (_, _) => CloseSettings();
            SettingsHost.Children.Add(_settingsView);
        }
        MailGrid.Visibility = Visibility.Collapsed;
        SettingsHost.Visibility = Visibility.Visible;
        _settingsView.ShowSection(section);
        _ = _settingsViewModel.LoadAsync();
    }

    private SettingsView? _settingsView;

    private void CloseSettings()
    {
        if (!IsSettingsOpen) return;
        SettingsHost.Visibility = Visibility.Collapsed;
        MailGrid.Visibility = Visibility.Visible;
        MessageListPane.FocusList();
    }

    // ------------------------------------------------------------------ search

    private void SearchBox_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        if (args.Reason != AutoSuggestionBoxTextChangeReason.UserInput) return;
        _searchTimer.Stop();
        if (string.IsNullOrWhiteSpace(sender.Text)) ViewModel.ExitSearch();
        else _searchTimer.Start();
    }

    private void SearchBox_QuerySubmitted(AutoSuggestBox sender, AutoSuggestBoxQuerySubmittedEventArgs args)
    {
        _searchTimer.Stop();
        if (IsSettingsOpen) CloseSettings();
        _ = ViewModel.SearchAsync(sender.Text);
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(MainViewModel.IsSearchMode) && !ViewModel.IsSearchMode && SearchBox.Text.Length > 0)
        {
            _searchTimer.Stop();
            SearchBox.Text = "";
        }
    }

    private void ClearSearch()
    {
        _searchTimer.Stop();
        SearchBox.Text = "";
        ViewModel.ExitSearch();
    }

    // ------------------------------------------------------------------ keyboard

    private bool IsTextInputFocused() =>
        FocusManager.GetFocusedElement(Content.XamlRoot) is TextBox or PasswordBox or RichEditBox;

    private void Accelerator_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        var key = sender.Key;
        var ctrl = (sender.Modifiers & VirtualKeyModifiers.Control) != 0;
        var shift = (sender.Modifiers & VirtualKeyModifiers.Shift) != 0;

        if (key == VirtualKey.Escape)
        {
            if (IsSettingsOpen) CloseSettings();
            else if (ViewModel.IsSearchMode || SearchBox.Text.Length > 0) ClearSearch();
            else return;
            args.Handled = true;
            return;
        }
        if (IsSettingsOpen) return;

        var typing = IsTextInputFocused();
        var targets = ViewModel.ActionTargets();
        switch (key)
        {
            case VirtualKey.N when ctrl:
                ViewModel.Compose(ComposeModes.New);
                break;
            case VirtualKey.R when ctrl && shift:
                ViewModel.Compose(ComposeModes.ReplyAll);
                break;
            case VirtualKey.R when ctrl:
                ViewModel.Compose(ComposeModes.Reply);
                break;
            case VirtualKey.F when ctrl && shift:
                ViewModel.Compose(ComposeModes.Forward);
                break;
            case VirtualKey.F when ctrl:
            case VirtualKey.E when ctrl:
                SearchBox.Focus(FocusState.Keyboard);
                break;
            case VirtualKey.Delete when !typing:
                _ = ViewModel.DeleteAsync(targets);
                break;
            case VirtualKey.Back when !typing:
            case VirtualKey.E when !ctrl && !typing:
                _ = ViewModel.ArchiveAsync(targets);
                break;
            case VirtualKey.Q when ctrl:
                _ = ViewModel.SetSeenAsync(targets, true);
                break;
            case VirtualKey.U when ctrl:
                _ = ViewModel.SetSeenAsync(targets, false);
                break;
            case VirtualKey.F5:
                _ = ViewModel.SyncNowAsync();
                break;
            default:
                return; // not ours: let the focused control handle the key
        }
        args.Handled = true;
    }

    private void Sync_Click(object sender, RoutedEventArgs e) => _ = ViewModel.SyncNowAsync();

    // ------------------------------------------------------------------ view model events

    private void OnComposeRequested(object? sender, ComposeRequest request)
    {
        if (ViewModel.Accounts.Count == 0)
        {
            ViewModel.ShowNotice(Loc.Get("Compose_NoAccounts"), "", InfoBarSeverity.Warning);
            return;
        }
        var window = new ComposeWindow(ViewModel, request);
        window.Activate();
    }

    private void OnSettingsApplied(object? sender, Settings settings)
    {
        ThemeHelper.Apply(settings.Theme);
        if (UiStateStore.PersistPreferences && UiStateStore.Current.Language != settings.Language)
        {
            UiStateStore.Current.Language = settings.Language;
            UiStateStore.Save();
        }
    }

    private void OnListReloaded(object? sender, EventArgs e)
    {
        if (!_firstDataLogged)
        {
            _firstDataLogged = true;
            StartupTrace.Mark("first data");
        }
        if (_prewarmScheduled) return;
        _prewarmScheduled = true;
        // Create the reader's WebView2 once the UI is idle so the first message opens instantly.
        _prewarmTimer = DispatcherQueue.CreateTimer(); // field: a local timer could be garbage-collected before firing
        _prewarmTimer.Interval = TimeSpan.FromMilliseconds(1200);
        _prewarmTimer.IsRepeating = false;
        _prewarmTimer.Tick += (t, _) =>
        {
            t.Stop();
            _prewarmTimer = null;
            Reader.Prewarm();
        };
        _prewarmTimer.Start();
    }

    // ------------------------------------------------------------------ closing

    private async void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (_exitConfirmed) return;
        var drafts = ComposeWindow.OpenWindows.ToList();
        if (drafts.Count == 0)
        {
            _exitConfirmed = true;
            SavePlacement();
            return;
        }
        args.Cancel = true;
        var dirty = false;
        foreach (var draft in drafts)
        {
            if (await draft.HasUnsavedChangesAsync())
            {
                dirty = true;
                break;
            }
        }
        if (dirty && !await AccountFlows.ConfirmAsync(Content.XamlRoot, Loc.Get("Exit_DraftsTitle"), Loc.Get("Exit_DraftsMessage"), Loc.Get("Exit_DraftsConfirm")))
            return;
        _exitConfirmed = true;
        SavePlacement();
        foreach (var draft in drafts) draft.ForceClose();
        Close();
    }

    private void OnClosed(object sender, WindowEventArgs args)
    {
        // Closing the main window ends the UI process; the daemon keeps running.
        App.Current.Shutdown();
    }
}

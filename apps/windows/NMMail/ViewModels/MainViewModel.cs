using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Controls;
using NMMail.Helpers;
using NMMail.Models;
using NMMail.Services;

namespace NMMail.ViewModels;

/// <summary>What the compose window must open: new (optionally prefilled from a mailto: URI), reply, reply all, forward.</summary>
public sealed record ComposeRequest(string Mode, long? MessageId, long? AccountId, MailtoData? Prefill = null);

public enum ListKind
{
    Unified,
    Folder,
    Search,
}

/// <summary>
/// State of the main window: sidebar (accounts/folders), message list (grouped by date, incrementally loaded)
/// and the reader. All members are used on the UI thread only.
/// </summary>
public sealed partial class MainViewModel : ObservableObject
{
    private const int PageSize = 50;
    private const int MaxRefreshWindow = 500;

    private readonly IDaemonClient _client;
    private readonly DispatcherQueueTimer _markReadTimer;
    private readonly DispatcherQueueTimer _refreshTimer;
    private readonly DispatcherQueueTimer _bannerTimer;
    private readonly DispatcherQueueTimer _noticeTimer;
    private readonly Dictionary<long, MessageItem> _items = [];
    private readonly Dictionary<long, Account> _accounts = [];
    private readonly Dictionary<long, Folder> _folders = [];
    private readonly Dictionary<string, SidebarItem> _sidebarByKey = [];
    private readonly List<SidebarItem> _accountItems = [];
    private readonly SidebarItem _unified;
    private SidebarItem _selectedSidebar;
    private ListKind _kind = ListKind.Unified;
    private long _folderId;
    private string _searchQuery = "";
    private int _queryVersion;
    private bool _hasMoreCached;
    private bool _olderExhausted;
    private bool _initialized;
    private bool _everConnected;
    private long? _pendingOpenId;
    private MessageItem? _markReadTarget;
    private DateTime _groupingDay = DateTime.Today;

    public MainViewModel(IDaemonClient client, DispatcherQueue dispatcher)
    {
        _client = client;
        Reader = new ReaderViewModel(client);
        _unified = SidebarItem.CreateUnified();
        _unified.IsSelected = true;
        _selectedSidebar = _unified;
        _sidebarByKey[_unified.Key] = _unified;
        SidebarItems.Add(_unified);

        ListTitle = _unified.Title;
        ListSubtitle = Loc.Get("List_AllAccounts");
        EmptyTitle = "";
        EmptyGlyph = Glyphs.Mail;
        NoticeTitle = "";
        NoticeMessage = "";
        NoticeSeverity = InfoBarSeverity.Informational;
        IsLoadingList = true;

        _markReadTimer = CreateTimer(dispatcher, 800, OnMarkReadTick);
        _refreshTimer = CreateTimer(dispatcher, 200, (_, _) => _ = RefreshListAsync());
        _bannerTimer = CreateTimer(dispatcher, 1500, (_, _) =>
        {
            ShowReconnectBanner = _client.ConnectionState != DaemonConnectionState.Connected;
            // Nothing to wait for while the service is down: stop the spinner (the banner explains the state).
            if (ShowReconnectBanner && !_initialized) IsLoadingList = false;
        });
        _noticeTimer = CreateTimer(dispatcher, 5000, (_, _) => IsNoticeOpen = false);

        _client.ConnectionStateChanged += OnConnectionStateChanged;
        _client.AccountStatus += OnAccountStatus;
        _client.AccountsChanged += OnAccountsChanged;
        _client.FoldersChanged += OnFoldersChanged;
        _client.MessagesChanged += OnMessagesChanged;
        _client.MailNew += OnMailNew;
        _client.OutboxSent += OnOutboxSent;
        _client.OutboxFailed += OnOutboxFailed;
        _client.UiShow += OnUiShow;
        _client.SettingsChanged += OnSettingsChanged;
        _bannerTimer.Start();
    }

    private static DispatcherQueueTimer CreateTimer(DispatcherQueue dispatcher, int ms,
        Windows.Foundation.TypedEventHandler<DispatcherQueueTimer, object> tick)
    {
        var timer = dispatcher.CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(ms);
        timer.IsRepeating = false;
        timer.Tick += tick;
        return timer;
    }

    public IDaemonClient Client => _client;
    public ReaderViewModel Reader { get; }
    public ObservableCollection<SidebarItem> SidebarItems { get; } = [];
    public ObservableCollection<MessageGroup> Groups { get; } = [];
    public IReadOnlyList<MessageItem> SelectedItems { get; private set; } = [];
    public Settings? CurrentSettings { get; private set; }
    public bool IsSearchActive => _kind == ListKind.Search;

    public IReadOnlyList<Account> Accounts => [.. _accounts.Values.OrderBy(a => a.Id)];

    /// <summary>Asks the view to select (and scroll to) a list item; null clears the selection.</summary>
    public event EventHandler<MessageItem?>? SelectRequested;
    public event EventHandler<ComposeRequest>? ComposeRequested;
    public event EventHandler? BringToFrontRequested;
    public event EventHandler<Account>? ReauthRequested;
    public event EventHandler<Settings>? SettingsApplied;
    public event EventHandler? AccountsUpdated;
    public event EventHandler? ListReloaded;

    [ObservableProperty]
    public partial bool ShowReconnectBanner { get; set; }

    [ObservableProperty]
    public partial bool IsLoadingList { get; set; }

    [ObservableProperty]
    public partial bool IsLoadingMore { get; set; }

    [ObservableProperty]
    public partial bool IsListEmpty { get; set; }

    [ObservableProperty]
    public partial string ListTitle { get; set; }

    [ObservableProperty]
    public partial string ListSubtitle { get; set; }

    [ObservableProperty]
    public partial string EmptyTitle { get; set; }

    [ObservableProperty]
    public partial string EmptyGlyph { get; set; }

    [ObservableProperty]
    public partial bool ShowAddAccountHint { get; set; }

    [ObservableProperty]
    public partial bool IsSearchMode { get; set; }

    [ObservableProperty]
    public partial bool UnreadOnly { get; set; }

    [ObservableProperty]
    public partial bool HasAccounts { get; set; }

    [ObservableProperty]
    public partial bool IsNoticeOpen { get; set; }

    [ObservableProperty]
    public partial string NoticeTitle { get; set; }

    [ObservableProperty]
    public partial string NoticeMessage { get; set; }

    [ObservableProperty]
    public partial InfoBarSeverity NoticeSeverity { get; set; }

    partial void OnUnreadOnlyChanged(bool value)
    {
        if (_initialized && _kind != ListKind.Search) _ = LoadListAsync();
    }

    // ------------------------------------------------------------------ connection & initial load

    private void OnConnectionStateChanged(object? sender, DaemonConnectionState state)
    {
        if (state == DaemonConnectionState.Connected)
        {
            _bannerTimer.Stop();
            ShowReconnectBanner = false;
            _everConnected = true;
            if (!_initialized) IsLoadingList = true;
            _ = ReloadAllAsync();
        }
        else if (state == DaemonConnectionState.Disconnected)
        {
            if (_everConnected) ShowReconnectBanner = true;
            else if (!_bannerTimer.IsRunning) _bannerTimer.Start();
        }
    }

    private async Task ReloadAllAsync()
    {
        try
        {
            var accountsTask = _client.ListAccountsAsync();
            var foldersTask = _client.ListFoldersAsync();
            await Task.WhenAll(accountsTask, foldersTask);
            RebuildSidebar(accountsTask.Result, foldersTask.Result);

            if (!_initialized)
            {
                _initialized = true;
                _ = LoadSettingsAsync();
                await LoadListAsync();
                _ = SyncCurrentAsync();
            }
            else
            {
                await RefreshListAsync();
            }

            if (_pendingOpenId is long id) OpenMessage(id);
            if (_pendingCompose is { } compose)
            {
                _pendingCompose = null;
                ComposeMailto(compose);
            }
        }
        catch (Exception ex)
        {
            AppInfo.Log("initial load failed: " + ex);
            ShowError(Loc.Get("Error_LoadAccounts"), ex);
            IsLoadingList = false;
        }
    }

    private async Task LoadSettingsAsync()
    {
        try
        {
            CurrentSettings = await _client.GetSettingsAsync();
            SettingsApplied?.Invoke(this, CurrentSettings);
        }
        catch (Exception ex)
        {
            AppInfo.Log("settings.get failed: " + ex.Message);
        }
    }

    // ------------------------------------------------------------------ sidebar

    private async Task ReloadSidebarAsync()
    {
        try
        {
            var accountsTask = _client.ListAccountsAsync();
            var foldersTask = _client.ListFoldersAsync();
            await Task.WhenAll(accountsTask, foldersTask);
            RebuildSidebar(accountsTask.Result, foldersTask.Result);
        }
        catch (Exception ex)
        {
            AppInfo.Log("sidebar reload failed: " + ex.Message);
        }
    }

    private void RebuildSidebar(IReadOnlyList<Account> accounts, IReadOnlyList<Folder> folders)
    {
        _accounts.Clear();
        foreach (var a in accounts) _accounts[a.Id] = a;
        _folders.Clear();
        foreach (var f in folders) _folders[f.Id] = f;

        var collapsed = UiStateStore.Current.CollapsedAccounts;
        var live = new HashSet<string> { _unified.Key };
        _accountItems.Clear();
        foreach (var account in accounts.OrderBy(a => a.Id))
        {
            var key = "acc:" + account.Id;
            if (!_sidebarByKey.TryGetValue(key, out var accountItem))
            {
                accountItem = SidebarItem.CreateAccount(account);
                accountItem.IsExpanded = !collapsed.Contains(account.Id);
                _sidebarByKey[key] = accountItem;
            }
            else
            {
                accountItem.Update(account);
            }
            live.Add(key);
            _accountItems.Add(accountItem);
            BuildFolderTree(accountItem, folders.Where(f => f.AccountId == account.Id), live);
        }

        foreach (var stale in _sidebarByKey.Keys.Where(k => !live.Contains(k)).ToList())
            _sidebarByKey.Remove(stale);

        UpdateUnifiedCount();
        HasAccounts = _accounts.Count > 0;
        SyncSidebar();

        if (!_sidebarByKey.ContainsKey(_selectedSidebar.Key))
        {
            // The selected folder/account disappeared.
            _ = SelectSidebarItemAsync(_unified);
        }
        else if (_selectedSidebar.Kind == SidebarKind.Folder)
        {
            ListTitle = _selectedSidebar.Title;
        }
        AccountsUpdated?.Invoke(this, EventArgs.Empty);
    }

    private void BuildFolderTree(SidebarItem accountItem, IEnumerable<Folder> folders, HashSet<string> live)
    {
        var list = folders.ToList();
        var items = new Dictionary<long, SidebarItem>(list.Count);
        foreach (var folder in list)
        {
            var key = "fld:" + folder.Id;
            if (_sidebarByKey.TryGetValue(key, out var item)) item.Update(folder);
            else _sidebarByKey[key] = item = SidebarItem.CreateFolder(folder);
            item.Children.Clear();
            items[folder.Id] = item;
            live.Add(key);
        }

        accountItem.Children.Clear();
        foreach (var folder in list)
        {
            var item = items[folder.Id];
            var parent = folder.ParentId is long pid && pid != folder.Id && items.TryGetValue(pid, out var p) ? p : accountItem;
            parent.Children.Add(item);
        }
        SortAndDepth(accountItem, -1);
    }

    private static void SortAndDepth(SidebarItem parent, int depth)
    {
        parent.Children.Sort(static (a, b) =>
        {
            var byRole = Glyphs.RoleOrder(a.Folder?.Role).CompareTo(Glyphs.RoleOrder(b.Folder?.Role));
            return byRole != 0 ? byRole : string.Compare(a.Title, b.Title, StringComparison.CurrentCultureIgnoreCase);
        });
        foreach (var child in parent.Children)
        {
            child.SetDepth(depth + 1);
            SortAndDepth(child, depth + 1);
        }
        parent.NotifyChildrenChanged();
    }

    private void SyncSidebar()
    {
        var flat = new List<SidebarItem>(_sidebarByKey.Count) { _unified };
        foreach (var account in _accountItems)
        {
            flat.Add(account);
            if (account.IsExpanded) AddVisibleChildren(account, flat);
        }
        CollectionSync.Sync(SidebarItems, flat);
    }

    private static void AddVisibleChildren(SidebarItem parent, List<SidebarItem> flat)
    {
        foreach (var child in parent.Children)
        {
            flat.Add(child);
            if (child.IsExpanded && child.HasChildren) AddVisibleChildren(child, flat);
        }
    }

    private void UpdateUnifiedCount() => _unified.Unread = _accounts.Values.Sum(a => a.Unread);

    public void ToggleExpanded(SidebarItem item)
    {
        if (!item.HasChildren && item.Kind != SidebarKind.Account) return;
        item.IsExpanded = !item.IsExpanded;
        SyncSidebar();
        if (item.Kind == SidebarKind.Account && item.Account is { } account)
        {
            var collapsed = UiStateStore.Current.CollapsedAccounts.Where(id => id != account.Id).ToList();
            if (!item.IsExpanded) collapsed.Add(account.Id);
            UiStateStore.Current.CollapsedAccounts = [.. collapsed];
        }
    }

    public Task OnSidebarItemInvokedAsync(SidebarItem item)
    {
        if (item.Kind == SidebarKind.Account)
        {
            ToggleExpanded(item);
            return Task.CompletedTask;
        }
        if (!item.IsSelectable)
        {
            ToggleExpanded(item);
            return Task.CompletedTask;
        }
        if (ReferenceEquals(item, _selectedSidebar) && !IsSearchMode) return RefreshListAsync();
        return SelectSidebarItemAsync(item);
    }

    public Task SelectSidebarItemAsync(SidebarItem item)
    {
        _selectedSidebar.IsSelected = false;
        _selectedSidebar = item;
        item.IsSelected = true;
        IsSearchMode = false;
        ApplySidebarQuery(item);
        var load = LoadListAsync();
        _ = SyncCurrentAsync();
        return load;
    }

    private void ApplySidebarQuery(SidebarItem item)
    {
        if (item.Kind == SidebarKind.Folder && item.Folder is { } folder)
        {
            _kind = ListKind.Folder;
            _folderId = folder.Id;
            ListTitle = item.Title;
            ListSubtitle = _accounts.TryGetValue(folder.AccountId, out var account) ? account.Email : "";
        }
        else
        {
            _kind = ListKind.Unified;
            ListTitle = _unified.Title;
            ListSubtitle = Loc.Get("List_AllAccounts");
        }
    }

    private async Task SyncCurrentAsync()
    {
        try
        {
            if (_kind == ListKind.Folder)
            {
                await _client.SyncFolderAsync(_folderId);
            }
            else if (_kind == ListKind.Unified)
            {
                await Task.WhenAll(InboxFolderIds().Select(id => _client.SyncFolderAsync(id)));
            }
        }
        catch (Exception ex)
        {
            AppInfo.Log("folders.sync failed: " + ex.Message);
        }
    }

    private List<long> InboxFolderIds() =>
        [.. _folders.Values.Where(f => f.Role == FolderRoles.Inbox).Select(f => f.Id)];

    public void RequestReauth(SidebarItem item)
    {
        if (item.Account is { } account) ReauthRequested?.Invoke(this, account);
    }

    public long? DefaultAccountId()
    {
        if (_selectedSidebar.Folder is { } folder) return folder.AccountId;
        return _accounts.Count > 0 ? _accounts.Keys.Min() : null;
    }

    /// <summary>Selectable folders of an account for "Sposta in…", in sidebar order.</summary>
    public IReadOnlyList<SidebarItem> MoveTargets(long accountId)
    {
        var result = new List<SidebarItem>();
        var account = _accountItems.FirstOrDefault(a => a.Account?.Id == accountId);
        if (account is not null) Collect(account);
        return result;

        void Collect(SidebarItem parent)
        {
            foreach (var child in parent.Children)
            {
                if (child.IsSelectable) result.Add(child);
                Collect(child);
            }
        }
    }

    // ------------------------------------------------------------------ message list

    private async Task LoadListAsync()
    {
        var version = ++_queryVersion;
        _markReadTimer.Stop();
        _items.Clear();
        Groups.Clear();
        Reader.Clear();
        IsListEmpty = false;
        IsLoadingList = true;
        try
        {
            var (messages, hasMore) = await FetchAsync(null, PageSize);
            if (version != _queryVersion) return;
            _hasMoreCached = hasMore;
            _olderExhausted = _kind == ListKind.Search;
            _groupingDay = DateTime.Today;
            foreach (var m in messages) AddItem(m);
            if (_kind == ListKind.Search)
                ListSubtitle = Loc.Format("List_SearchCount", _items.Count);
        }
        catch (Exception ex)
        {
            if (version == _queryVersion) ShowError(Loc.Get("Error_LoadList"), ex);
        }
        finally
        {
            if (version == _queryVersion)
            {
                IsLoadingList = false;
                UpdateEmptyState();
                ListReloaded?.Invoke(this, EventArgs.Empty);
            }
        }
    }

    private async Task<(IReadOnlyList<MessageSummary> Messages, bool HasMore)> FetchAsync(MessageItem? after, int limit)
    {
        switch (_kind)
        {
            case ListKind.Search:
                return (await _client.SearchAsync(_searchQuery, null, 200), false);
            case ListKind.Folder:
            {
                var page = await _client.ListMessagesAsync(new ListMessagesParams
                {
                    FolderId = _folderId,
                    Limit = limit,
                    BeforeDate = after?.Date,
                    BeforeId = after?.Id,
                    UnreadOnly = UnreadOnly ? true : null,
                });
                return (page.Messages ?? [], page.HasMore);
            }
            default:
            {
                var page = await _client.ListMessagesAsync(new ListMessagesParams
                {
                    Unified = "inbox",
                    Limit = limit,
                    BeforeDate = after?.Date,
                    BeforeId = after?.Id,
                    UnreadOnly = UnreadOnly ? true : null,
                });
                return (page.Messages ?? [], page.HasMore);
            }
        }
    }

    /// <summary>Called by the view when the list is scrolled near its end.</summary>
    public async Task LoadMoreAsync()
    {
        if (IsLoadingMore || IsLoadingList || _kind == ListKind.Search || _items.Count == 0) return;
        if (!_hasMoreCached && _olderExhausted) return;
        var version = _queryVersion;
        var last = LastItem();
        if (last is null) return;
        IsLoadingMore = true;
        try
        {
            if (_hasMoreCached)
            {
                var (messages, hasMore) = await FetchAsync(last, PageSize);
                if (version != _queryVersion) return;
                _hasMoreCached = hasMore;
                foreach (var m in messages) AddItem(m);
            }
            else
            {
                // Beyond the local cache window: fetch from the server.
                var folderIds = _kind == ListKind.Folder ? [_folderId] : InboxFolderIds();
                var pages = await Task.WhenAll(folderIds.Select(id => _client.LoadOlderAsync(id, last.Date)));
                if (version != _queryVersion) return;
                foreach (var page in pages)
                {
                    foreach (var m in page.Messages ?? [])
                    {
                        if (UnreadOnly && m.Seen) continue;
                        AddItem(m);
                    }
                }
                _olderExhausted = pages.All(p => !p.HasMore);
            }
        }
        catch (Exception ex)
        {
            if (version == _queryVersion)
            {
                _olderExhausted = true;
                ShowError(Loc.Get("Error_LoadOlder"), ex);
            }
        }
        finally
        {
            if (version == _queryVersion)
            {
                IsLoadingMore = false;
                UpdateEmptyState();
            }
        }
    }

    private MessageItem? LastItem()
    {
        for (var g = Groups.Count - 1; g >= 0; g--)
        {
            if (Groups[g].Count > 0) return Groups[g][^1];
        }
        return null;
    }

    private async Task RefreshListAsync()
    {
        if (!_initialized || IsLoadingList) return;
        var version = _queryVersion;
        try
        {
            var limit = Math.Clamp(_items.Count, PageSize, MaxRefreshWindow);
            var (messages, hasMore) = await FetchAsync(null, limit);
            if (version != _queryVersion) return;
            Merge(messages, hasMore);
        }
        catch (Exception ex)
        {
            AppInfo.Log("refresh failed: " + ex.Message);
        }
    }

    public void ScheduleRefresh()
    {
        _refreshTimer.Stop();
        _refreshTimer.Start();
    }

    /// <summary>Updates the list in place so selection and scroll position survive a refresh.</summary>
    private void Merge(IReadOnlyList<MessageSummary> messages, bool hasMore)
    {
        RegroupIfDayChanged();
        var incoming = new HashSet<long>(messages.Count);
        var oldest = long.MaxValue;
        foreach (var m in messages)
        {
            incoming.Add(m.Id);
            oldest = Math.Min(oldest, m.Date);
        }

        var removed = new List<MessageItem>();
        foreach (var item in _items.Values)
        {
            if (incoming.Contains(item.Id)) continue;
            // Items older than the refreshed window are untouched when more pages exist.
            if (!hasMore || item.Date >= oldest) removed.Add(item);
        }
        foreach (var item in removed) RemoveItem(item);

        foreach (var m in messages)
        {
            if (_items.TryGetValue(m.Id, out var existing))
            {
                var oldDate = existing.Date;
                var oldBucket = existing.Bucket;
                existing.Apply(m);
                if (existing.Date != oldDate || existing.Bucket != oldBucket)
                {
                    RemoveFromGroup(existing);
                    InsertItem(existing);
                }
            }
            else
            {
                AddItem(m);
            }
        }

        if (_kind != ListKind.Search) _hasMoreCached = hasMore;
        else ListSubtitle = Loc.Format("List_SearchCount", _items.Count);
        UpdateEmptyState();
    }

    private void RegroupIfDayChanged()
    {
        if (_groupingDay == DateTime.Today) return;
        _groupingDay = DateTime.Today;
        var all = _items.Values.ToList();
        Groups.Clear();
        foreach (var item in all)
        {
            item.RefreshTime();
            InsertItem(item);
        }
    }

    /// <summary>Call when the window is activated: fixes "Oggi/Ieri" after midnight.</summary>
    public void OnActivated() => RegroupIfDayChanged();

    private bool AddItem(MessageSummary summary)
    {
        if (_items.TryGetValue(summary.Id, out var existing))
        {
            existing.Apply(summary);
            return false;
        }
        var item = new MessageItem(summary, showAccount: _kind != ListKind.Folder && _accounts.Count > 1);
        _items[summary.Id] = item;
        InsertItem(item);
        return true;
    }

    private void InsertItem(MessageItem item)
    {
        var group = GetOrCreateGroup(item.Bucket);
        var index = group.Count;
        while (index > 0 && MessageItem.Compare(group[index - 1], item) > 0) index--;
        group.Insert(index, item);
    }

    private MessageGroup GetOrCreateGroup(DateBucket bucket)
    {
        for (var i = 0; i < Groups.Count; i++)
        {
            if (Groups[i].Bucket == bucket) return Groups[i];
            if (Groups[i].Bucket > bucket)
            {
                var inserted = new MessageGroup(bucket);
                Groups.Insert(i, inserted);
                return inserted;
            }
        }
        var group = new MessageGroup(bucket);
        Groups.Add(group);
        return group;
    }

    private void RemoveItem(MessageItem item)
    {
        _items.Remove(item.Id);
        RemoveFromGroup(item);
        item.IsHovered = false;
    }

    private void RemoveFromGroup(MessageItem item)
    {
        for (var i = 0; i < Groups.Count; i++)
        {
            var group = Groups[i];
            var index = group.IndexOf(item);
            if (index < 0) continue;
            group.RemoveAt(index);
            if (group.Count == 0) Groups.RemoveAt(i);
            return;
        }
    }

    private void UpdateEmptyState()
    {
        IsListEmpty = !IsLoadingList && _items.Count == 0;
        if (!IsListEmpty) return;
        ShowAddAccountHint = false;
        if (_kind == ListKind.Search)
        {
            EmptyTitle = Loc.Format("List_EmptySearch", _searchQuery);
            EmptyGlyph = Glyphs.Search;
        }
        else if (!HasAccounts && _initialized)
        {
            EmptyTitle = Loc.Get("List_EmptyNoAccounts");
            EmptyGlyph = Glyphs.Mail;
            ShowAddAccountHint = true;
        }
        else if (UnreadOnly)
        {
            EmptyTitle = Loc.Get("List_EmptyUnread");
            EmptyGlyph = Glyphs.MailRead;
        }
        else
        {
            EmptyTitle = Loc.Get("List_Empty");
            EmptyGlyph = Glyphs.MailRead;
        }
    }

    // ------------------------------------------------------------------ selection & reading

    public void OnSelectionChanged(IReadOnlyList<MessageItem> selected)
    {
        SelectedItems = selected;
        _markReadTimer.Stop();
        _markReadTarget = null;
        switch (selected.Count)
        {
            case 0:
                Reader.Clear();
                break;
            case 1:
                var item = selected[0];
                _ = Reader.ShowAsync(item);
                if (item.IsUnread)
                {
                    _markReadTarget = item;
                    _markReadTimer.Start();
                }
                break;
            default:
                Reader.ShowMultiple(selected.Count);
                break;
        }
    }

    private void OnMarkReadTick(DispatcherQueueTimer sender, object args)
    {
        var item = _markReadTarget;
        _markReadTarget = null;
        if (item is null || !item.IsUnread) return;
        if (SelectedItems.Count != 1 || !ReferenceEquals(SelectedItems[0], item)) return;
        _ = SetSeenAsync([item], true);
    }

    /// <summary>Opens a message by id (from --open, ui.show or a redirected activation).</summary>
    public async void OpenMessage(long id)
    {
        if (!_initialized)
        {
            _pendingOpenId = id;
            return;
        }
        _pendingOpenId = null;
        if (!IsSearchMode && _items.TryGetValue(id, out var loaded))
        {
            SelectRequested?.Invoke(this, loaded);
            return;
        }
        try
        {
            var detail = await _client.GetMessageAsync(id);
            SidebarItem? target = null;
            if (_folders.TryGetValue(detail.FolderId, out var folder))
                target = folder.Role == FolderRoles.Inbox ? _unified : _sidebarByKey.GetValueOrDefault("fld:" + folder.Id);
            target ??= _unified;
            if (!ReferenceEquals(target, _selectedSidebar) || IsSearchMode || !_items.ContainsKey(id))
                await SelectSidebarItemAsync(target);
            if (_items.TryGetValue(id, out var item)) SelectRequested?.Invoke(this, item);
            else await Reader.ShowByIdAsync(id);
        }
        catch (Exception ex)
        {
            ShowError(Loc.Get("Error_OpenMessage"), ex);
        }
    }

    // ------------------------------------------------------------------ actions

    public IReadOnlyList<MessageItem> ActionTargets() =>
        SelectedItems.Count > 0 ? SelectedItems : Reader.Item is { } item ? [item] : [];

    public Task ArchiveAsync(IReadOnlyList<MessageItem> items) =>
        RemoveWithActionAsync(items, ids => _client.ArchiveAsync(ids), "Error_Archive");

    public Task DeleteAsync(IReadOnlyList<MessageItem> items) =>
        RemoveWithActionAsync(items, ids => _client.DeleteAsync(ids), "Error_Delete");

    public Task MoveAsync(IReadOnlyList<MessageItem> items, long folderId)
    {
        if (_kind == ListKind.Folder && folderId == _folderId) return Task.CompletedTask;
        return RemoveWithActionAsync(items, ids => _client.MoveAsync(ids, folderId), "Error_Move");
    }

    private async Task RemoveWithActionAsync(IReadOnlyList<MessageItem> items, Func<long[], Task> action, string errorKey)
    {
        if (items.Count == 0) return;
        var targets = items.ToList();
        var ids = targets.Select(i => i.Id).ToArray();
        // Move the selection only if the open/selected message goes away (a hover quick action on another row
        // must not change what the user is reading). Select first, then remove: focus stays in the list.
        var selectionAffected = targets.Any(t => SelectedItems.Contains(t)) || (Reader.Item is { } open && targets.Contains(open));
        if (selectionAffected)
        {
            var next = FindNeighbor(targets);
            if (next is not null) SelectRequested?.Invoke(this, next);
            else Reader.Clear();
        }
        foreach (var item in targets)
        {
            if (item.IsUnread) AdjustUnread(item, -1);
            RemoveItem(item);
        }
        UpdateEmptyState();

        try
        {
            await action(ids);
        }
        catch (Exception ex)
        {
            ShowError(Loc.Get(errorKey), ex);
            await RefreshListAsync();
            _ = ReloadSidebarAsync();
        }
    }

    private MessageItem? FindNeighbor(List<MessageItem> removing)
    {
        var flat = Groups.SelectMany(g => g).ToList();
        var set = new HashSet<MessageItem>(removing);
        var first = flat.FindIndex(set.Contains);
        var last = flat.FindLastIndex(set.Contains);
        if (first < 0) return null;
        for (var i = last + 1; i < flat.Count; i++)
        {
            if (!set.Contains(flat[i])) return flat[i];
        }
        for (var i = first - 1; i >= 0; i--)
        {
            if (!set.Contains(flat[i])) return flat[i];
        }
        return null;
    }

    public async Task SetSeenAsync(IReadOnlyList<MessageItem> items, bool seen)
    {
        var changed = items.Where(i => i.IsUnread == seen).ToList();
        if (changed.Count == 0) return;
        foreach (var item in changed) SetSeenLocal(item, seen);
        try
        {
            await _client.SetFlagsAsync(changed.Select(i => i.Id).ToArray(), seen: seen);
        }
        catch (Exception ex)
        {
            foreach (var item in changed) SetSeenLocal(item, !seen);
            ShowError(Loc.Get("Error_Action"), ex);
        }
    }

    private void SetSeenLocal(MessageItem item, bool seen)
    {
        if (item.IsUnread != seen) return;
        item.IsUnread = !seen;
        item.Summary.Seen = seen;
        AdjustUnread(item, seen ? -1 : 1);
    }

    public async Task ToggleFlagAsync(IReadOnlyList<MessageItem> items)
    {
        if (items.Count == 0) return;
        var flag = items.Any(i => !i.IsFlagged);
        var changed = items.Where(i => i.IsFlagged != flag).ToList();
        foreach (var item in changed)
        {
            item.IsFlagged = flag;
            item.Summary.Flagged = flag;
        }
        try
        {
            await _client.SetFlagsAsync(changed.Select(i => i.Id).ToArray(), flagged: flag);
        }
        catch (Exception ex)
        {
            foreach (var item in changed)
            {
                item.IsFlagged = !flag;
                item.Summary.Flagged = !flag;
            }
            ShowError(Loc.Get("Error_Action"), ex);
        }
    }

    /// <summary>Optimistic unread counters (the daemon sends the authoritative ones right after).</summary>
    private void AdjustUnread(MessageItem item, int delta)
    {
        if (!_folders.TryGetValue(item.FolderId, out var folder)) return;
        folder.Unread = Math.Max(0, folder.Unread + delta);
        if (_sidebarByKey.TryGetValue("fld:" + folder.Id, out var folderItem)) folderItem.Update(folder);
        if (folder.Role != FolderRoles.Inbox || !_accounts.TryGetValue(folder.AccountId, out var account)) return;
        account.Unread = Math.Max(0, account.Unread + delta);
        if (_sidebarByKey.TryGetValue("acc:" + account.Id, out var accountItem)) accountItem.Update(account);
        UpdateUnifiedCount();
    }

    /// <summary>Opens a new message prefilled from a mailto: URI (--compose, also via single-instance redirection).</summary>
    public void ComposeMailto(MailtoData data)
    {
        if (!_initialized)
        {
            _pendingCompose = data; // accounts are not known yet
            return;
        }
        ComposeRequested?.Invoke(this, new ComposeRequest(ComposeModes.New, null, DefaultAccountId(), data));
    }

    private MailtoData? _pendingCompose;

    public void Compose(string mode)
    {
        if (mode == ComposeModes.New)
        {
            ComposeRequested?.Invoke(this, new ComposeRequest(mode, null, DefaultAccountId()));
            return;
        }
        var item = Reader.Item ?? (SelectedItems.Count == 1 ? SelectedItems[0] : null);
        if (item is null) return;
        ComposeRequested?.Invoke(this, new ComposeRequest(mode, item.Id, item.AccountId));
    }

    public async Task SyncNowAsync()
    {
        try
        {
            await _client.SyncNowAsync();
            ScheduleRefresh();
        }
        catch (Exception ex)
        {
            ShowError(Loc.Get("Error_Sync"), ex);
        }
    }

    // ------------------------------------------------------------------ search

    public async Task SearchAsync(string query)
    {
        query = query.Trim();
        if (query.Length == 0)
        {
            ExitSearch();
            return;
        }
        _kind = ListKind.Search;
        _searchQuery = query;
        IsSearchMode = true;
        ListTitle = Loc.Format("List_SearchResults", query);
        ListSubtitle = "";
        await LoadListAsync();
    }

    public void ExitSearch()
    {
        if (!IsSearchMode) return;
        IsSearchMode = false;
        ApplySidebarQuery(_selectedSidebar);
        _ = LoadListAsync();
    }

    // ------------------------------------------------------------------ daemon events

    private void OnAccountStatus(object? sender, Account account)
    {
        _accounts[account.Id] = account;
        if (_sidebarByKey.TryGetValue("acc:" + account.Id, out var item))
        {
            item.Update(account);
            UpdateUnifiedCount();
            AccountsUpdated?.Invoke(this, EventArgs.Empty);
        }
        else
        {
            _ = ReloadSidebarAsync();
        }
    }

    private void OnAccountsChanged(object? sender, EventArgs e) => _ = ReloadSidebarAsync();

    private async void OnFoldersChanged(object? sender, FoldersChangedEvent e)
    {
        try
        {
            var folders = await _client.ListFoldersAsync(e.AccountId);
            var all = _folders.Values.Where(f => f.AccountId != e.AccountId).Concat(folders).ToList();
            RebuildSidebar([.. _accounts.Values], all);
        }
        catch (Exception ex)
        {
            AppInfo.Log("folders reload failed: " + ex.Message);
        }
    }

    private void OnMessagesChanged(object? sender, MessagesChangedEvent e)
    {
        if (AffectsCurrentList(e.AccountId, e.FolderIds)) ScheduleRefresh();
    }

    private void OnMailNew(object? sender, MailNewEvent e)
    {
        if (_kind == ListKind.Unified ||
            (_kind == ListKind.Folder && _folders.TryGetValue(_folderId, out var f) && f.AccountId == e.AccountId))
        {
            ScheduleRefresh();
        }
    }

    private bool AffectsCurrentList(long accountId, long[]? folderIds)
    {
        if (!_initialized) return false;
        var all = folderIds is null || folderIds.Length == 0;
        return _kind switch
        {
            ListKind.Search => true,
            ListKind.Folder => all ? _folders.TryGetValue(_folderId, out var f) && f.AccountId == accountId : folderIds!.Contains(_folderId),
            _ => all || folderIds!.Any(id => _folders.TryGetValue(id, out var folder) && folder.Role == FolderRoles.Inbox),
        };
    }

    private void OnOutboxSent(object? sender, OutboxSentEvent e) =>
        ShowNotice(Loc.Get("Notice_Sent"), "", InfoBarSeverity.Success);

    private void OnOutboxFailed(object? sender, OutboxFailedEvent e) =>
        ShowNotice(Loc.Get("Notice_SendFailed"), e.Error ?? "", InfoBarSeverity.Error, autoHide: false);

    private void OnUiShow(object? sender, UiShowEvent e)
    {
        BringToFrontRequested?.Invoke(this, EventArgs.Empty);
        if (e.MessageId is long id) OpenMessage(id);
    }

    private void OnSettingsChanged(object? sender, Settings settings)
    {
        CurrentSettings = settings;
        SettingsApplied?.Invoke(this, settings);
    }

    public void ApplySettingsLocally(Settings settings)
    {
        CurrentSettings = settings;
        SettingsApplied?.Invoke(this, settings);
    }

    // ------------------------------------------------------------------ notices

    public void ShowNotice(string title, string message, InfoBarSeverity severity, bool autoHide = true)
    {
        NoticeTitle = title;
        NoticeMessage = message;
        NoticeSeverity = severity;
        IsNoticeOpen = true;
        _noticeTimer.Stop();
        if (autoHide) _noticeTimer.Start();
    }

    public void ShowError(string title, Exception ex)
    {
        var message = ex is DaemonException { Code: DaemonErrorCodes.Unavailable or DaemonErrorCodes.Timeout }
            ? Loc.Get("Error_ServiceUnavailable")
            : ex.Message;
        ShowNotice(title, message, InfoBarSeverity.Error, autoHide: false);
    }
}

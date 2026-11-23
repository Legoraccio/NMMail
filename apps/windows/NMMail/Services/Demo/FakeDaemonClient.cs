using System.Globalization;
using System.Net;
using Microsoft.UI.Dispatching;
using NMMail.Models;

namespace NMMail.Services.Demo;

/// <summary>
/// In-memory daemon used by <c>NMMail.exe --demo</c>: realistic sample data and simulated behaviour
/// (flags, archive, delete, move, send, search, OAuth progress, a new mail every ~30 s).
/// All members run on the UI thread (methods are awaited from the UI and events are posted to it).
/// </summary>
public sealed class FakeDaemonClient(DispatcherQueue dispatcher) : IDaemonClient
{
    private readonly DispatcherQueue _dispatcher = dispatcher;
    private readonly HashSet<long> _olderServed = [];
    private readonly long _startedAt = Environment.TickCount64;
    private List<Account> _accounts = [];
    private List<Folder> _folders = [];
    private List<DemoMessage> _messages = [];
    private Settings _settings = new();
    private DispatcherQueueTimer? _newMailTimer;
    private CancellationTokenSource? _authCts;
    private long _nextMessageId = 5000;
    private long _nextOutboxId = 1;
    private int _incomingSequence;

    public bool IsDemo => true;
    public DaemonConnectionState ConnectionState { get; private set; } = DaemonConnectionState.Connecting;
    public string? DaemonVersion => "0.1.0-demo";

    public event EventHandler<DaemonConnectionState>? ConnectionStateChanged;
    public event EventHandler<Account>? AccountStatus;
    public event EventHandler? AccountsChanged;
    public event EventHandler<FoldersChangedEvent>? FoldersChanged;
    public event EventHandler<MessagesChangedEvent>? MessagesChanged;
    public event EventHandler<MailNewEvent>? MailNew;
    public event EventHandler<OutboxSentEvent>? OutboxSent;
    public event EventHandler<OutboxFailedEvent>? OutboxFailed;
    public event EventHandler<AuthProgressEvent>? AuthProgress;
    public event EventHandler<UiShowEvent>? UiShow;
    public event EventHandler<Settings>? SettingsChanged;

    public void Start()
    {
        if (_messages.Count > 0) return;
        _accounts = DemoData.Accounts();
        _folders = DemoData.Folders();
        _messages = DemoData.Messages();
        RecomputeCounts();
        SetConnected(true);

        // The Outlook account finishes connecting a moment later (NMMAIL_DEMO_STATE overrides the final state).
        Later(2500, () =>
        {
            var state = Environment.GetEnvironmentVariable("NMMAIL_DEMO_STATE");
            if (string.IsNullOrWhiteSpace(state)) state = AccountStates.Online;
            SetAccountState(DemoData.OutlookId, state, state == AccountStates.Error ? "Connessione rifiutata da outlook.office365.com" : null);
        });

        _newMailTimer = _dispatcher.CreateTimer();
        _newMailTimer.Interval = TimeSpan.FromSeconds(30);
        _newMailTimer.IsRepeating = true;
        _newMailTimer.Tick += (_, _) => DeliverNewMail();
        _newMailTimer.Start();
    }

    public ValueTask DisposeAsync()
    {
        _newMailTimer?.Stop();
        _authCts?.Cancel();
        return ValueTask.CompletedTask;
    }

    // ------------------------------------------------------------------ helpers

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());

    private static async void Later(int ms, Action action)
    {
        await Task.Delay(ms);
        action();
    }

    private static Task Latency(int ms = 20) => Task.Delay(ms);

    private void SetConnected(bool connected)
    {
        var state = connected ? DaemonConnectionState.Connected : DaemonConnectionState.Disconnected;
        Post(() =>
        {
            ConnectionState = state;
            ConnectionStateChanged?.Invoke(this, state);
        });
    }

    private void EnsureConnected()
    {
        if (ConnectionState != DaemonConnectionState.Connected)
            throw new DaemonException(DaemonErrorCodes.Unavailable, "Servizio NM Mail (demo) non raggiungibile");
    }

    private Folder? FindFolder(long id) => _folders.FirstOrDefault(f => f.Id == id);

    private DemoMessage? Find(long id) => _messages.FirstOrDefault(m => m.Detail.Id == id);

    private string? RoleOf(long folderId) => FindFolder(folderId)?.Role;

    private bool IsJunkOrTrash(DemoMessage m) => RoleOf(m.Detail.FolderId) is FolderRoles.Trash or FolderRoles.Spam;

    private IEnumerable<DemoMessage> MessagesIn(Folder folder) => folder.Role switch
    {
        FolderRoles.Flagged => _messages.Where(m => m.Detail.AccountId == folder.AccountId && m.Detail.Flagged && !IsJunkOrTrash(m)),
        FolderRoles.Important => _messages.Where(m => m.Detail.AccountId == folder.AccountId && m.Important && !IsJunkOrTrash(m)),
        FolderRoles.All => _messages.Where(m => m.Detail.AccountId == folder.AccountId && !IsJunkOrTrash(m) && RoleOf(m.Detail.FolderId) != FolderRoles.Drafts),
        _ => _messages.Where(m => m.Detail.FolderId == folder.Id),
    };

    private void RecomputeCounts()
    {
        foreach (var folder in _folders)
        {
            var list = MessagesIn(folder).ToList();
            folder.Total = list.Count;
            folder.Unread = list.Count(m => !m.Detail.Seen);
        }
        foreach (var account in _accounts)
        {
            account.Unread = _folders.FirstOrDefault(f => f.AccountId == account.Id && f.Role == FolderRoles.Inbox)?.Unread ?? 0;
        }
    }

    /// <summary>Recomputes counters and raises messages.changed / folders.changed / account.status like the daemon.</summary>
    private void NotifyChanged(IEnumerable<long> accountIds)
    {
        RecomputeCounts();
        foreach (var accountId in accountIds.Distinct().ToList())
        {
            var account = _accounts.FirstOrDefault(a => a.Id == accountId);
            if (account is null) continue;
            var folderIds = _folders.Where(f => f.AccountId == accountId).Select(f => f.Id).ToArray();
            var accountCopy = account.Clone();
            Post(() =>
            {
                MessagesChanged?.Invoke(this, new MessagesChangedEvent { AccountId = accountId, FolderIds = folderIds });
                FoldersChanged?.Invoke(this, new FoldersChangedEvent { AccountId = accountId });
                AccountStatus?.Invoke(this, accountCopy);
            });
        }
    }

    private void SetAccountState(long accountId, string state, string? error = null)
    {
        var account = _accounts.FirstOrDefault(a => a.Id == accountId);
        if (account is null) return;
        account.State = state;
        account.Error = error;
        if (state == AccountStates.Online) account.LastSync = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        var copy = account.Clone();
        Post(() => AccountStatus?.Invoke(this, copy));
    }

    private static MessageSummary ToSummary(DemoMessage m)
    {
        var d = m.Detail;
        return new MessageSummary
        {
            Id = d.Id, AccountId = d.AccountId, FolderId = d.FolderId, Subject = d.Subject, From = d.From, To = d.To,
            Date = d.Date, Snippet = d.Snippet, Seen = d.Seen, Flagged = d.Flagged, Answered = d.Answered,
            HasAttachments = d.HasAttachments, Size = d.Size,
        };
    }

    private void DeliverNewMail()
    {
        if (ConnectionState != DaemonConnectionState.Connected) return;
        var accountId = DemoData.IncomingAccount(_incomingSequence);
        var inbox = _folders.FirstOrDefault(f => f.AccountId == accountId && f.Role == FolderRoles.Inbox)
                    ?? _folders.FirstOrDefault(f => f.Role == FolderRoles.Inbox);
        if (inbox is null) return;
        var message = DemoData.Incoming(_nextMessageId++, _incomingSequence++, inbox.Id);
        message.Detail.AccountId = inbox.AccountId;
        _messages.Add(message);
        var id = message.Detail.Id;
        Post(() => MailNew?.Invoke(this, new MailNewEvent { AccountId = inbox.AccountId, MessageIds = [id] }));
        NotifyChanged([inbox.AccountId]);
    }

    // ------------------------------------------------------------------ daemon / accounts

    public async Task<DaemonStatus> GetStatusAsync(CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        return new DaemonStatus
        {
            Version = "0.1.0 (demo)",
            Pid = Environment.ProcessId,
            UptimeSec = (Environment.TickCount64 - _startedAt) / 1000,
            MemoryBytes = 7_340_032,
            Accounts = [.. _accounts.Select(a => a.Clone())],
            OAuthBuiltIn = new OAuthBuiltIn { Google = true, Microsoft = true },
        };
    }

    public async Task ShutdownAsync(CancellationToken ct = default)
    {
        await Latency();
        // Simulate the service going away and coming back (exercises the reconnect banner).
        SetConnected(false);
        Later(5000, () =>
        {
            SetConnected(true);
            foreach (var account in _accounts) SetAccountState(account.Id, AccountStates.Online);
        });
    }

    public async Task<IReadOnlyList<Account>> ListAccountsAsync(CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        return [.. _accounts.Select(a => a.Clone())];
    }

    public async Task<Account> AddOAuthAccountAsync(string provider, CancellationToken ct = default)
    {
        EnsureConnected();
        _authCts?.Cancel();
        var cts = _authCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        try
        {
            PostAuth(AuthStages.Browser, provider == Providers.Gmail
                ? "https://accounts.google.com/o/oauth2/v2/auth?client_id=demo"
                : "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=demo");
            await Task.Delay(3000, cts.Token);
            PostAuth(AuthStages.Exchanging);
            await Task.Delay(900, cts.Token);
            PostAuth(AuthStages.Connecting);
            await Task.Delay(1100, cts.Token);
            // Re-authentication of an account that needed it (the daemon matches by the signed-in address).
            if (_accounts.FirstOrDefault(a => a.Provider == provider && a.State == AccountStates.AuthRequired) is { } existing)
            {
                SetAccountState(existing.Id, AccountStates.Online);
                PostAuth(AuthStages.Done);
                return existing.Clone();
            }
            var name = provider == Providers.Gmail ? "gmail.com" : "outlook.it";
            var n = _accounts.Count + 1;
            var account = AddAccount(provider, $"nuovo.account{n}@{name}", "Nuovo account");
            PostAuth(AuthStages.Done);
            Post(() => AccountsChanged?.Invoke(this, EventArgs.Empty));
            return account.Clone();
        }
        catch (OperationCanceledException)
        {
            throw new DaemonException(DaemonErrorCodes.Cancelled, "Accesso annullato");
        }
        finally
        {
            if (ReferenceEquals(_authCts, cts)) _authCts = null;
        }
    }

    private void PostAuth(string stage, string? url = null) =>
        Post(() => AuthProgress?.Invoke(this, new AuthProgressEvent { Stage = stage, Url = url }));

    public async Task CancelAuthAsync(CancellationToken ct = default)
    {
        await Latency();
        _authCts?.Cancel();
    }

    public async Task<Account> AddImapAccountAsync(AddImapParams request, CancellationToken ct = default)
    {
        await Latency(400);
        EnsureConnected();
        if (!request.Email.Contains('@'))
            throw new DaemonException(DaemonErrorCodes.BadRequest, "Indirizzo e-mail non valido");
        if (string.IsNullOrWhiteSpace(request.ImapHost) || string.IsNullOrWhiteSpace(request.SmtpHost))
            throw new DaemonException(DaemonErrorCodes.BadRequest, "Server IMAP e SMTP obbligatori");
        await Task.Delay(1000, ct);
        if (request.ImapHost.Contains("fail", StringComparison.OrdinalIgnoreCase))
            throw new DaemonException(DaemonErrorCodes.Server, $"Impossibile connettersi a {request.ImapHost}:{request.ImapPort}");
        var existing = _accounts.FirstOrDefault(a => a.Email.Equals(request.Email, StringComparison.OrdinalIgnoreCase));
        if (existing is not null)
        {
            existing.State = AccountStates.Online; // re-authentication of an existing account
            Post(() => AccountsChanged?.Invoke(this, EventArgs.Empty));
            return existing.Clone();
        }
        var account = AddAccount(Providers.Imap, request.Email, string.IsNullOrWhiteSpace(request.DisplayName) ? request.Email : request.DisplayName);
        Post(() => AccountsChanged?.Invoke(this, EventArgs.Empty));
        return account.Clone();
    }

    private Account AddAccount(string provider, string email, string displayName)
    {
        var id = _accounts.Count == 0 ? 1 : _accounts.Max(a => a.Id) + 1;
        var account = new Account
        {
            Id = id, Provider = provider, Email = email, DisplayName = displayName, State = AccountStates.Online,
            Notify = true, NotifyFilter = NotifyFilterNames.All, LastSync = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
        };
        _accounts.Add(account);
        var baseId = id * 100;
        var archiveRole = provider == Providers.Gmail ? FolderRoles.All : FolderRoles.Archive;
        _folders.AddRange(
        [
            new Folder { Id = baseId + 1, AccountId = id, Name = "INBOX", DisplayName = "Posta in arrivo", Role = FolderRoles.Inbox },
            new Folder { Id = baseId + 2, AccountId = id, Name = "Sent", DisplayName = "Posta inviata", Role = FolderRoles.Sent },
            new Folder { Id = baseId + 3, AccountId = id, Name = "Drafts", DisplayName = "Bozze", Role = FolderRoles.Drafts },
            new Folder { Id = baseId + 4, AccountId = id, Name = "Archive", DisplayName = archiveRole == FolderRoles.All ? "Tutti i messaggi" : "Archivio", Role = archiveRole },
            new Folder { Id = baseId + 5, AccountId = id, Name = "Trash", DisplayName = "Cestino", Role = FolderRoles.Trash },
        ]);
        var welcome = DemoData.Incoming(_nextMessageId++, 0, baseId + 1);
        welcome.Detail.AccountId = id;
        welcome.Detail.Subject = "Benvenuto in NM Mail";
        welcome.Detail.From = new Addr { Name = "NM Mail", Address = "benvenuto@nmmail.example" };
        welcome.BodyHtml = DemoData.TextToHtml("Il tuo account è pronto: da ora NM Mail ti avvisa appena arriva una nuova e-mail, anche a finestra chiusa.\n\nBuona lettura!");
        welcome.Detail.Text = "Il tuo account è pronto.";
        welcome.Detail.Snippet = "Il tuo account è pronto: da ora NM Mail ti avvisa appena arriva una nuova e-mail, anche a finestra chiusa.";
        welcome.Detail.Attachments = [];
        welcome.Detail.HasAttachments = false;
        _messages.Add(welcome);
        RecomputeCounts();
        return account;
    }

    public async Task<Account> UpdateAccountAsync(UpdateAccountParams request, CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        var account = _accounts.FirstOrDefault(a => a.Id == request.Id)
                      ?? throw new DaemonException(DaemonErrorCodes.NotFound, $"account {request.Id} not found");
        if (request.DisplayName is not null) account.DisplayName = request.DisplayName;
        if (request.Notify is bool notify) account.Notify = notify;
        if (request.NotifyFilter is not null) account.NotifyFilter = request.NotifyFilter;
        var copy = account.Clone();
        Post(() => AccountStatus?.Invoke(this, copy));
        return account.Clone();
    }

    public async Task RemoveAccountAsync(long id, CancellationToken ct = default)
    {
        await Latency(150);
        EnsureConnected();
        _accounts.RemoveAll(a => a.Id == id);
        _folders.RemoveAll(f => f.AccountId == id);
        _messages.RemoveAll(m => m.Detail.AccountId == id);
        Post(() => AccountsChanged?.Invoke(this, EventArgs.Empty));
    }

    // ------------------------------------------------------------------ folders & messages

    public async Task<IReadOnlyList<Folder>> ListFoldersAsync(long? accountId = null, CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        return [.. _folders.Where(f => accountId is null || f.AccountId == accountId).Select(f => f.Clone())];
    }

    public async Task SyncFolderAsync(long folderId, CancellationToken ct = default)
    {
        await Latency(150);
        EnsureConnected();
        var folder = FindFolder(folderId) ?? throw new DaemonException(DaemonErrorCodes.NotFound, $"folder {folderId} not found");
        var accountId = folder.AccountId;
        Post(() => MessagesChanged?.Invoke(this, new MessagesChangedEvent { AccountId = accountId, FolderIds = [folderId] }));
    }

    public async Task<Folder> SetFolderMonitoredAsync(long folderId, bool monitored, CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        var folder = FindFolder(folderId) ?? throw new DaemonException(DaemonErrorCodes.NotFound, $"folder {folderId} not found");
        if (folder.Role == FolderRoles.Inbox) return folder.Clone();
        if (monitored && !folder.Monitored &&
            _folders.Count(f => f.AccountId == folder.AccountId && f.Monitored && f.Role != FolderRoles.Inbox) >= 5)
        {
            throw new DaemonException(DaemonErrorCodes.BadRequest, "at most 5 monitored folders per account");
        }
        folder.Monitored = monitored;
        var accountId = folder.AccountId;
        Post(() => FoldersChanged?.Invoke(this, new FoldersChangedEvent { AccountId = accountId }));
        return folder.Clone();
    }

    public async Task<MessagePage> ListMessagesAsync(ListMessagesParams request, CancellationToken ct = default)
    {
        await Latency(35);
        EnsureConnected();
        IEnumerable<DemoMessage> source;
        if (request.Unified == "inbox")
            source = _messages.Where(m => RoleOf(m.Detail.FolderId) == FolderRoles.Inbox);
        else if (request.FolderId is long folderId && FindFolder(folderId) is { } folder)
            source = MessagesIn(folder);
        else
            throw new DaemonException(DaemonErrorCodes.NotFound, "folder not found");

        if (request.UnreadOnly == true) source = source.Where(m => !m.Detail.Seen);
        source = source.OrderByDescending(m => m.Detail.Date).ThenByDescending(m => m.Detail.Id);
        if (request.BeforeDate is long before)
        {
            var beforeId = request.BeforeId ?? long.MaxValue;
            source = source.Where(m => m.Detail.Date < before || (m.Detail.Date == before && m.Detail.Id < beforeId));
        }
        var limit = Math.Clamp(request.Limit ?? 50, 1, 1000);
        var page = source.Take(limit + 1).ToList();
        return new MessagePage { Messages = [.. page.Take(limit).Select(ToSummary)], HasMore = page.Count > limit };
    }

    public async Task<MessagePage> LoadOlderAsync(long folderId, long beforeDate, CancellationToken ct = default)
    {
        await Latency(700); // "from the server"
        EnsureConnected();
        var folder = FindFolder(folderId) ?? throw new DaemonException(DaemonErrorCodes.NotFound, $"folder {folderId} not found");
        if (folder.Role != FolderRoles.Inbox || !_olderServed.Add(folderId)) return new MessagePage { HasMore = false };
        var older = DemoData.Older(_nextMessageId, folder.AccountId, folderId, beforeDate);
        _nextMessageId += older.Count;
        _messages.AddRange(older);
        RecomputeCounts();
        return new MessagePage { Messages = [.. older.Select(ToSummary)], HasMore = false };
    }

    public async Task<MessageDetail> GetMessageAsync(long id, bool allowRemote = false, CancellationToken ct = default)
    {
        await Latency(25);
        EnsureConnected();
        var m = Find(id) ?? throw new DaemonException(DaemonErrorCodes.NotFound, $"message {id} not found");
        var d = m.Detail;
        return new MessageDetail
        {
            Id = d.Id, AccountId = d.AccountId, FolderId = d.FolderId, Subject = d.Subject, From = d.From, To = d.To,
            Date = d.Date, Snippet = d.Snippet, Seen = d.Seen, Flagged = d.Flagged, Answered = d.Answered,
            HasAttachments = d.HasAttachments, Size = d.Size, Cc = d.Cc, Bcc = d.Bcc, ReplyTo = d.ReplyTo,
            MessageIdHeader = d.MessageIdHeader,
            Html = m.TextOnly ? null : DemoData.Document(m.BodyHtml, allowRemote && m.RemoteImages),
            Text = d.Text,
            Attachments = [.. d.Attachments],
            HasRemoteContent = m.RemoteImages,
            RemoteAllowed = allowRemote && m.RemoteImages,
        };
    }

    public async Task SetFlagsAsync(IReadOnlyCollection<long> ids, bool? seen = null, bool? flagged = null, CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        var touched = new List<long>();
        foreach (var id in ids)
        {
            if (Find(id) is not { } m) continue;
            if (seen is bool s) m.Detail.Seen = s;
            if (flagged is bool f) m.Detail.Flagged = f;
            touched.Add(m.Detail.AccountId);
        }
        NotifyChanged(touched);
    }

    public async Task ArchiveAsync(IReadOnlyCollection<long> ids, CancellationToken ct = default)
    {
        await Latency(60);
        EnsureConnected();
        var touched = new List<long>();
        foreach (var id in ids)
        {
            if (Find(id) is not { } m) continue;
            var target = _folders.FirstOrDefault(f => f.AccountId == m.Detail.AccountId && f.Role == FolderRoles.Archive)
                         ?? _folders.FirstOrDefault(f => f.AccountId == m.Detail.AccountId && f.Role == FolderRoles.All)
                         ?? throw new DaemonException(DaemonErrorCodes.BadRequest, "Nessuna cartella Archivio per questo account");
            m.Detail.FolderId = target.Id;
            touched.Add(m.Detail.AccountId);
        }
        NotifyChanged(touched);
    }

    public async Task DeleteAsync(IReadOnlyCollection<long> ids, CancellationToken ct = default)
    {
        await Latency(60);
        EnsureConnected();
        var touched = new List<long>();
        foreach (var id in ids)
        {
            if (Find(id) is not { } m) continue;
            touched.Add(m.Detail.AccountId);
            if (RoleOf(m.Detail.FolderId) == FolderRoles.Trash)
            {
                _messages.Remove(m);
                continue;
            }
            var trash = _folders.FirstOrDefault(f => f.AccountId == m.Detail.AccountId && f.Role == FolderRoles.Trash);
            if (trash is null) _messages.Remove(m);
            else m.Detail.FolderId = trash.Id;
        }
        NotifyChanged(touched);
    }

    public async Task MoveAsync(IReadOnlyCollection<long> ids, long folderId, CancellationToken ct = default)
    {
        await Latency(60);
        EnsureConnected();
        var target = FindFolder(folderId) ?? throw new DaemonException(DaemonErrorCodes.NotFound, $"folder {folderId} not found");
        var touched = new List<long>();
        foreach (var id in ids)
        {
            if (Find(id) is not { } m) continue;
            if (m.Detail.AccountId != target.AccountId)
                throw new DaemonException(DaemonErrorCodes.BadRequest, "Impossibile spostare tra account diversi");
            if (target.Role is FolderRoles.Flagged or FolderRoles.Important)
            {
                m.Detail.Flagged = true;
            }
            else
            {
                m.Detail.FolderId = target.Id;
            }
            touched.Add(m.Detail.AccountId);
        }
        NotifyChanged(touched);
    }

    public async Task<IReadOnlyList<MessageSummary>> SearchAsync(string query, long? accountId = null, int? limit = null, CancellationToken ct = default)
    {
        await Latency(40);
        EnsureConnected();
        var compare = CultureInfo.CurrentCulture.CompareInfo;
        const CompareOptions options = CompareOptions.IgnoreCase | CompareOptions.IgnoreNonSpace;
        var terms = query.Split(' ', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        return [.. _messages
            .Where(m => accountId is null || m.Detail.AccountId == accountId)
            .Where(m => !IsJunkOrTrash(m))
            .Where(m =>
            {
                var d = m.Detail;
                var haystack = $"{d.Subject}\n{d.From?.Name}\n{d.From?.Address}\n{d.Snippet}\n{d.Text}\n{string.Join(' ', d.Attachments.Select(a => a.Filename))}";
                return terms.All(t => compare.IndexOf(haystack, t, options) >= 0);
            })
            .OrderByDescending(m => m.Detail.Date)
            .Take(Math.Clamp(limit ?? 100, 1, 500))
            .Select(ToSummary)];
    }

    // ------------------------------------------------------------------ attachments & contacts

    public async Task<string> SaveAttachmentAsync(long messageId, long partId, string path, CancellationToken ct = default)
    {
        await Latency(80);
        EnsureConnected();
        var m = Find(messageId) ?? throw new DaemonException(DaemonErrorCodes.NotFound, $"message {messageId} not found");
        var attachment = m.Detail.Attachments.FirstOrDefault(a => a.PartId == partId)
                         ?? throw new DaemonException(DaemonErrorCodes.NotFound, $"part {partId} not found");
        var content = DemoData.AttachmentContent(m, attachment);
        await Task.Run(() =>
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllBytes(path, content);
        }, ct);
        return path;
    }

    public async Task<IReadOnlyList<Addr>> SuggestContactsAsync(string prefix, int? limit = null, CancellationToken ct = default)
    {
        await Latency(15);
        EnsureConnected();
        prefix = prefix.Trim();
        if (prefix.Length == 0) return [];
        var own = new HashSet<string>(_accounts.Select(a => a.Email), StringComparer.OrdinalIgnoreCase);
        return [.. _messages
            .SelectMany(m => m.Detail.To.Concat(m.Detail.Cc).Append(m.Detail.From!))
            .Where(a => a is not null && !own.Contains(a.Address))
            .GroupBy(a => a.Address, StringComparer.OrdinalIgnoreCase)
            .Select(g => (Addr: g.First(), Count: g.Count()))
            .Where(x => Matches(x.Addr, prefix))
            .OrderByDescending(x => x.Count)
            .ThenBy(x => x.Addr.DisplayName, StringComparer.CurrentCultureIgnoreCase)
            .Take(Math.Clamp(limit ?? 8, 1, 50))
            .Select(x => new Addr { Name = x.Addr.Name, Address = x.Addr.Address })];

        static bool Matches(Addr a, string p) =>
            a.Address.StartsWith(p, StringComparison.CurrentCultureIgnoreCase) ||
            (a.Name ?? "").Split(' ', StringSplitOptions.RemoveEmptyEntries).Any(w => w.StartsWith(p, StringComparison.CurrentCultureIgnoreCase)) ||
            (a.Name ?? "").StartsWith(p, StringComparison.CurrentCultureIgnoreCase);
    }

    // ------------------------------------------------------------------ compose

    public async Task<ComposePrepared> PrepareComposeAsync(string mode, long messageId, string? lang = null, CancellationToken ct = default)
    {
        await Latency(40);
        EnsureConnected();
        var m = Find(messageId) ?? throw new DaemonException(DaemonErrorCodes.NotFound, $"message {messageId} not found");
        var d = m.Detail;
        var account = _accounts.FirstOrDefault(a => a.Id == d.AccountId);
        var own = account?.Email ?? "";
        var english = lang == "en";
        var culture = CultureInfo.GetCultureInfo(english ? "en-US" : "it-IT");
        var date = DateTimeOffset.FromUnixTimeMilliseconds(d.Date).LocalDateTime.ToString("f", culture);
        var from = d.From ?? new Addr { Address = "" };
        var fromText = WebUtility.HtmlEncode(from.ToString());
        var original = m.TextOnly ? DemoData.TextToHtml(d.Text ?? "") : m.BodyHtml;

        var result = new ComposePrepared { AccountId = d.AccountId };
        if (mode == ComposeModes.Forward)
        {
            result.Subject = Prefix(d.Subject, "Fwd: ", "Fwd:", "I:");
            var header = english
                ? $"---------- Forwarded message ----------<br>From: {fromText}<br>Date: {date}<br>Subject: {WebUtility.HtmlEncode(d.Subject)}<br>To: {WebUtility.HtmlEncode(string.Join(", ", d.To.Select(a => a.ToString())))}"
                : $"---------- Messaggio inoltrato ----------<br>Da: {fromText}<br>Data: {date}<br>Oggetto: {WebUtility.HtmlEncode(d.Subject)}<br>A: {WebUtility.HtmlEncode(string.Join(", ", d.To.Select(a => a.ToString())))}";
            result.Html = $"<div class=\"nm-forward\" style=\"color:#555\">{header}</div><br>{original}";
            result.Attachments = [.. d.Attachments.Where(a => !a.Inline).Select(a => new ComposeAttachment
            {
                MessageId = d.Id, PartId = a.PartId, Filename = a.Filename, Size = a.Size,
            })];
        }
        else
        {
            result.Subject = Prefix(d.Subject, "Re: ", "Re:", "R:");
            var replyTo = d.ReplyTo.Length > 0 ? d.ReplyTo : [from];
            if (mode == ComposeModes.ReplyAll)
            {
                result.To = [.. replyTo.Concat(d.To).Where(a => !a.Address.Equals(own, StringComparison.OrdinalIgnoreCase))
                    .DistinctBy(a => a.Address.ToLowerInvariant())];
                result.Cc = [.. d.Cc.Where(a => !a.Address.Equals(own, StringComparison.OrdinalIgnoreCase))];
            }
            else
            {
                result.To = replyTo;
            }
            var header = english ? $"On {date}, {fromText} wrote:" : $"Il giorno {date} {fromText} ha scritto:";
            result.Html = $"<div class=\"nm-quote\"><div style=\"color:#555\">{header}</div>" +
                          $"<blockquote style=\"margin:0 0 0 .8ex;border-left:2px solid #ccc;padding-left:1ex\">{original}</blockquote></div>";
            result.InReplyTo = System.Text.Json.JsonSerializer.SerializeToElement(d.MessageIdHeader ?? "", IpcJson.Default.String);
            result.References = result.InReplyTo;
        }
        result.Text = DemoData.HtmlToText(result.Html);
        return result;

        static string Prefix(string subject, string prefix, params string[] existing) =>
            existing.Any(e => subject.StartsWith(e, StringComparison.OrdinalIgnoreCase)) ? subject : prefix + subject;
    }

    public async Task<SendResult> SendAsync(ComposeSendParams request, CancellationToken ct = default)
    {
        await Latency(250);
        EnsureConnected();
        var recipients = request.To.Concat(request.Cc).Concat(request.Bcc).ToList();
        if (recipients.Count == 0)
            throw new DaemonException(DaemonErrorCodes.BadRequest, "Nessun destinatario");
        var invalid = recipients.FirstOrDefault(a => !a.Address.Contains('@') || a.Address.EndsWith('@'));
        if (invalid is not null)
            throw new DaemonException(DaemonErrorCodes.BadRequest, $"Indirizzo non valido: {invalid.Address}");
        // Demo hooks: "rifiuta" → permanent SMTP error, "offline" → queued in the outbox.
        var rejected = recipients.FirstOrDefault(a => a.Address.Contains("rifiuta", StringComparison.OrdinalIgnoreCase));
        if (rejected is not null)
            throw new DaemonException(DaemonErrorCodes.Server, $"550 5.1.1 <{rejected.Address}>: destinatario inesistente");
        var queued = recipients.Any(a => a.Address.Contains("offline", StringComparison.OrdinalIgnoreCase));

        var account = _accounts.FirstOrDefault(a => a.Id == request.AccountId)
                      ?? throw new DaemonException(DaemonErrorCodes.NotFound, "account not found");
        var sent = _folders.FirstOrDefault(f => f.AccountId == account.Id && f.Role == FolderRoles.Sent);
        var outboxId = _nextOutboxId++;
        if (sent is not null)
        {
            var text = string.IsNullOrWhiteSpace(request.Text) ? DemoData.HtmlToText(request.Html) : request.Text;
            var message = new DemoMessage
            {
                BodyHtml = string.IsNullOrWhiteSpace(request.Html) ? DemoData.TextToHtml(text) : request.Html,
                Detail = new MessageDetail
                {
                    Id = _nextMessageId++, AccountId = account.Id, FolderId = sent.Id, Subject = request.Subject,
                    From = new Addr { Name = account.DisplayName, Address = account.Email }, To = request.To, Cc = request.Cc, Bcc = request.Bcc,
                    Date = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(), Snippet = DemoData.Snippet(text), Seen = true,
                    Text = text, HasAttachments = request.Attachments.Length + (request.ForwardAttachments?.Length ?? 0) > 0,
                    Attachments = [.. request.Attachments.Select((p, i) => new Attachment
                    {
                        PartId = i + 2, Filename = Path.GetFileName(p), Mime = DemoData.MimeFor(p),
                        Size = File.Exists(p) ? new FileInfo(p).Length : 0,
                    })],
                },
            };
            if (request.ReplyToMessageId is long original && Find(original) is { } originalMessage) originalMessage.Detail.Answered = true;
            var failLater = queued && recipients.Any(a => a.Address.Contains("fail", StringComparison.OrdinalIgnoreCase));
            Later(queued ? 8000 : 1200, () =>
            {
                if (failLater)
                {
                    Post(() => OutboxFailed?.Invoke(this, new OutboxFailedEvent { OutboxId = outboxId, Error = "Server SMTP non raggiungibile dopo 3 tentativi" }));
                    return;
                }
                _messages.Add(message);
                Post(() => OutboxSent?.Invoke(this, new OutboxSentEvent { OutboxId = outboxId }));
                NotifyChanged([account.Id]);
            });
        }
        return new SendResult { OutboxId = outboxId, Queued = queued };
    }

    // ------------------------------------------------------------------ settings & misc

    public async Task<Settings> GetSettingsAsync(CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        return _settings.Clone();
    }

    public async Task<Settings> SetSettingsAsync(SettingsPatch patch, CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        if (patch.Autostart is bool autostart) _settings.Autostart = autostart;
        if (patch.NotificationsEnabled is bool enabled) _settings.NotificationsEnabled = enabled;
        if (patch.NotificationSound is bool sound) _settings.NotificationSound = sound;
        if (patch.ShowPreview is bool preview) _settings.ShowPreview = preview;
        if (patch.CacheDays is int days) _settings.CacheDays = days;
        if (patch.Theme is not null) _settings.Theme = patch.Theme;
        if (patch.Language is not null) _settings.Language = patch.Language;
        if (patch.GoogleClientId is not null) _settings.GoogleClientId = patch.GoogleClientId;
        if (patch.GoogleClientSecret is not null) _settings.GoogleClientSecret = patch.GoogleClientSecret;
        if (patch.MicrosoftClientId is not null) _settings.MicrosoftClientId = patch.MicrosoftClientId;
        var copy = _settings.Clone();
        Post(() => SettingsChanged?.Invoke(this, copy));
        return _settings.Clone();
    }

    public async Task SyncNowAsync(long? accountId = null, CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        var targets = _accounts.Where(a => accountId is null || a.Id == accountId).Select(a => a.Id).ToList();
        foreach (var id in targets) SetAccountState(id, AccountStates.Connecting);
        Later(1200, () =>
        {
            foreach (var id in targets) SetAccountState(id, AccountStates.Online);
            NotifyChanged(targets);
        });
    }

    public async Task PauseNotificationsAsync(long? until, CancellationToken ct = default)
    {
        await Latency();
        EnsureConnected();
        _settings.QuietUntil = until;
        var copy = _settings.Clone();
        Post(() => SettingsChanged?.Invoke(this, copy));
    }

    public async Task<string> TestNotificationAsync(CancellationToken ct = default)
    {
        await Latency(150);
        EnsureConnected();
        // NMMAIL_DEMO_NOTIFY lets the "disabled" path of the settings page be exercised.
        return Environment.GetEnvironmentVariable("NMMAIL_DEMO_NOTIFY") ?? "enabled; actionCenter=1 (demo: nessuna notifica reale)";
    }

    public async Task ShowUiAsync(long? messageId = null, CancellationToken ct = default)
    {
        await Latency();
        Post(() => UiShow?.Invoke(this, new UiShowEvent { MessageId = messageId }));
    }

    public async Task ActivateAsync(string uri, CancellationToken ct = default)
    {
        await Latency();
        if (!Uri.TryCreate(uri, UriKind.Absolute, out var parsed)) throw new DaemonException(DaemonErrorCodes.BadRequest, "invalid uri");
        var query = parsed.Query.TrimStart('?').Split('&', StringSplitOptions.RemoveEmptyEntries)
            .Select(p => p.Split('=', 2)).Where(p => p.Length == 2).ToDictionary(p => p[0], p => Uri.UnescapeDataString(p[1]));
        long? id = query.TryGetValue("msg", out var msg) && long.TryParse(msg, out var parsedId) ? parsedId : null;
        switch (parsed.Host)
        {
            case "open":
                await ShowUiAsync(id, ct);
                break;
            case "markread" when id is long markId:
                await SetFlagsAsync([markId], seen: true, ct: ct);
                break;
            case "archive" when id is long archiveId:
                await ArchiveAsync([archiveId], ct);
                break;
            case "delete" when id is long deleteId:
                await DeleteAsync([deleteId], ct);
                break;
        }
    }
}

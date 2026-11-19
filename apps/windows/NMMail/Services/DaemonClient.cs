using System.Buffers;
using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text.Encodings.Web;
using System.Text.Json;
using System.Text.Json.Serialization.Metadata;
using Microsoft.UI.Dispatching;
using NMMail.Models;

namespace NMMail.Services;

/// <summary>
/// JSON-lines client for the nmmaild named pipe (docs/IPC.md).
/// One background loop owns the connection: it connects (launching the daemon if needed), sends <c>hello</c>,
/// reads responses/events and reconnects when the pipe breaks. Requests are correlated by <c>id</c>.
/// </summary>
public sealed class DaemonClient : IDaemonClient
{
    private const int InitialBufferSize = 16 * 1024;
    private const int MaxLineBytes = 512 * 1024 * 1024;
    private static readonly TimeSpan DefaultTimeout = TimeSpan.FromSeconds(30);
    private static readonly TimeSpan ReadyTimeout = TimeSpan.FromSeconds(8);
    private static readonly TimeSpan LaunchRetryWindow = TimeSpan.FromSeconds(6);
    private static readonly JsonWriterOptions WriterOptions = new() { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };

    private readonly DispatcherQueue _dispatcher;
    private readonly string _pipeName;
    private readonly CancellationTokenSource _lifetime = new();
    private readonly SemaphoreSlim _writeLock = new(1, 1);
    private readonly ConcurrentDictionary<long, PendingRequest> _pending = new();
    private TaskCompletionSource<NamedPipeClientStream> _ready = NewReady();
    private NamedPipeClientStream? _pipe;
    private long _nextId;
    private long _lastLaunchTimestamp;
    private Task? _loop;
    private DaemonConnectionState _state = DaemonConnectionState.Connecting;

    public DaemonClient(DispatcherQueue dispatcher)
    {
        _dispatcher = dispatcher;
        // NMMAIL_PIPE overrides the pipe name (development/tests only, e.g. to run a second daemon).
        var overrideName = Environment.GetEnvironmentVariable("NMMAIL_PIPE");
        _pipeName = string.IsNullOrWhiteSpace(overrideName) ? "nmmail-" + Environment.UserName : overrideName.Trim();
    }

    public bool IsDemo => false;
    public DaemonConnectionState ConnectionState => _state;
    public string? DaemonVersion { get; private set; }

    /// <summary>Last problem met while launching the daemon (diagnostics only).</summary>
    public string? LastLaunchError { get; private set; }

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

    public void Start() => _loop ??= Task.Run(() => RunAsync(_lifetime.Token));

    public async ValueTask DisposeAsync()
    {
        _lifetime.Cancel();
        Volatile.Read(ref _pipe)?.Dispose();
        if (_loop is not null)
        {
            try { await _loop.ConfigureAwait(false); } catch { /* shutting down */ }
        }
        _lifetime.Dispose();
    }

    // ------------------------------------------------------------------ connection loop

    private static TaskCompletionSource<NamedPipeClientStream> NewReady() =>
        new(TaskCreationOptions.RunContinuationsAsynchronously);

    private async Task RunAsync(CancellationToken ct)
    {
        var failures = 0;
        while (!ct.IsCancellationRequested)
        {
            var pipe = await TryConnectAsync(ct).ConfigureAwait(false);
            if (pipe is null)
            {
                failures++;
                SetState(DaemonConnectionState.Disconnected);
                MaybeLaunchDaemon();
                var delay = Stopwatch.GetElapsedTime(_lastLaunchTimestamp) < LaunchRetryWindow
                    ? TimeSpan.FromMilliseconds(200)
                    : TimeSpan.FromMilliseconds(failures < 30 ? 1000 : 3000);
                try { await Task.Delay(delay, ct).ConfigureAwait(false); }
                catch (OperationCanceledException) { break; }
                continue;
            }

            failures = 0;
            var readLoop = ReadLoopAsync(pipe, ct);
            try
            {
                var hello = await SendRequestAsync(pipe, "hello",
                    // client "ui": the daemon then sends ui.show events here instead of launching a new window.
                    new HelloParams { Client = "ui", Version = AppInfo.Version },
                    IpcJson.Default.HelloParams, IpcJson.Default.HelloResult, TimeSpan.FromSeconds(5), ct).ConfigureAwait(false);
                DaemonVersion = hello?.DaemonVersion;
                Volatile.Write(ref _pipe, pipe);
                _ready.TrySetResult(pipe);
                SetState(DaemonConnectionState.Connected);
            }
            catch (Exception ex)
            {
                Debug.WriteLine($"[ipc] hello failed: {ex.Message}");
                pipe.Dispose();
            }

            await readLoop.ConfigureAwait(false);

            // The pipe is gone: fail in-flight requests and start over.
            Volatile.Write(ref _pipe, null);
            if (_ready.Task.IsCompleted) _ready = NewReady();
            pipe.Dispose();
            FailAllPending(new DaemonException(DaemonErrorCodes.Unavailable, "Connection to nmmaild lost"));
            if (ct.IsCancellationRequested) break;
            SetState(DaemonConnectionState.Disconnected);
            try { await Task.Delay(250, ct).ConfigureAwait(false); }
            catch (OperationCanceledException) { break; }
        }
    }

    private async Task<NamedPipeClientStream?> TryConnectAsync(CancellationToken ct)
    {
        // Cheap existence probe: WaitNamedPipe fails immediately with ERROR_FILE_NOT_FOUND when no server
        // exists, so we never busy-spin inside ConnectAsync while the daemon is down.
        if (!NativeMethods.WaitNamedPipe(@"\\.\pipe\" + _pipeName, 1) &&
            Marshal.GetLastPInvokeError() == NativeMethods.ERROR_FILE_NOT_FOUND)
        {
            return null;
        }

        // Identification: the server may identify us but never impersonate us (defends against pipe squatting).
        var pipe = new NamedPipeClientStream(".", _pipeName, PipeDirection.InOut, PipeOptions.Asynchronous,
            TokenImpersonationLevel.Identification);
        try
        {
            await pipe.ConnectAsync(2000, ct).ConfigureAwait(false);
            return pipe;
        }
        catch (Exception ex) when (ex is TimeoutException or IOException or UnauthorizedAccessException or OperationCanceledException)
        {
            pipe.Dispose();
            return null;
        }
    }

    private void MaybeLaunchDaemon()
    {
        var last = _lastLaunchTimestamp;
        if (last != 0 && Stopwatch.GetElapsedTime(last) < TimeSpan.FromSeconds(20)) return;
        _lastLaunchTimestamp = Stopwatch.GetTimestamp();

        var path = Environment.GetEnvironmentVariable("NMMAIL_DAEMON");
        if (string.IsNullOrWhiteSpace(path)) path = Path.Combine(AppContext.BaseDirectory, "nmmaild.exe");
        if (!File.Exists(path))
        {
            LastLaunchError = $"Daemon not found: {path}";
            Debug.WriteLine("[ipc] " + LastLaunchError);
            return;
        }

        try
        {
            // A daemon that is still starting up owns no pipe yet: don't start a second copy.
            var running = Process.GetProcessesByName(Path.GetFileNameWithoutExtension(path));
            var alreadyRunning = running.Length > 0;
            foreach (var p in running) p.Dispose();
            if (alreadyRunning) return;

            using var process = Process.Start(new ProcessStartInfo(path, "--background")
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                WorkingDirectory = Path.GetDirectoryName(path)!,
            });
            LastLaunchError = null;
        }
        catch (Exception ex)
        {
            LastLaunchError = ex.Message;
            Debug.WriteLine("[ipc] launch failed: " + ex.Message);
        }
    }

    private void SetState(DaemonConnectionState state)
    {
        if (_state == state) return;
        _state = state;
        _dispatcher.TryEnqueue(() => ConnectionStateChanged?.Invoke(this, state));
    }

    // ------------------------------------------------------------------ reading

    private async Task ReadLoopAsync(NamedPipeClientStream pipe, CancellationToken ct)
    {
        var buffer = new byte[InitialBufferSize];
        int start = 0, end = 0;
        try
        {
            while (true)
            {
                var nl = buffer.AsSpan(start, end - start).IndexOf((byte)'\n');
                if (nl >= 0)
                {
                    var length = nl;
                    if (length > 0 && buffer[start + length - 1] == (byte)'\r') length--;
                    if (length > 0) HandleLine(new ReadOnlyMemory<byte>(buffer, start, length));
                    start += nl + 1;
                    continue;
                }

                if (start > 0)
                {
                    Buffer.BlockCopy(buffer, start, buffer, 0, end - start);
                    end -= start;
                    start = 0;
                }
                if (end == 0 && buffer.Length > 1024 * 1024)
                {
                    buffer = new byte[InitialBufferSize]; // give back memory after a huge message
                }
                else if (end == buffer.Length)
                {
                    if (buffer.Length >= MaxLineBytes) throw new InvalidDataException("IPC line too long");
                    Array.Resize(ref buffer, buffer.Length * 2);
                }

                var read = await pipe.ReadAsync(buffer.AsMemory(end), ct).ConfigureAwait(false);
                if (read == 0) break;
                end += read;
            }
        }
        catch (Exception ex) when (ex is OperationCanceledException or IOException or ObjectDisposedException or InvalidDataException)
        {
            Debug.WriteLine($"[ipc] read loop ended: {ex.GetType().Name}");
        }
    }

    private void HandleLine(ReadOnlyMemory<byte> line)
    {
        try
        {
            using var doc = JsonDocument.Parse(line);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object) return;

            if (root.TryGetProperty("event", out var ev))
            {
                root.TryGetProperty("data", out var data);
                DispatchEvent(ev.GetString(), data);
                return;
            }

            if (!root.TryGetProperty("id", out var idElement) || !idElement.TryGetInt64(out var id)) return;
            if (!_pending.TryRemove(id, out var pending)) return;

            if (root.TryGetProperty("error", out var error) && error.ValueKind == JsonValueKind.Object)
            {
                var code = error.TryGetProperty("code", out var c) && c.ValueKind == JsonValueKind.String ? c.GetString()! : DaemonErrorCodes.Internal;
                var message = error.TryGetProperty("message", out var m) && m.ValueKind == JsonValueKind.String ? m.GetString()! : code;
                pending.Fail(new DaemonException(code, message));
                return;
            }

            root.TryGetProperty("result", out var result);
            try
            {
                pending.Complete(result); // deserializes on this (background) thread
            }
            catch (Exception ex)
            {
                pending.Fail(new DaemonException(DaemonErrorCodes.Internal, "Invalid response: " + ex.Message));
            }
        }
        catch (JsonException ex)
        {
            Debug.WriteLine("[ipc] malformed line: " + ex.Message);
        }
    }

    private void DispatchEvent(string? name, JsonElement data)
    {
        try
        {
            switch (name)
            {
                case "account.status":
                    if (data.Deserialize(IpcJson.Default.Account) is { } account)
                        Post(() => AccountStatus?.Invoke(this, account));
                    break;
                case "accounts.changed":
                    Post(() => AccountsChanged?.Invoke(this, EventArgs.Empty));
                    break;
                case "folders.changed":
                    if (data.Deserialize(IpcJson.Default.FoldersChangedEvent) is { } folders)
                        Post(() => FoldersChanged?.Invoke(this, folders));
                    break;
                case "messages.changed":
                    if (data.Deserialize(IpcJson.Default.MessagesChangedEvent) is { } changed)
                        Post(() => MessagesChanged?.Invoke(this, changed));
                    break;
                case "mail.new":
                    if (data.Deserialize(IpcJson.Default.MailNewEvent) is { } mail)
                        Post(() => MailNew?.Invoke(this, mail));
                    break;
                case "outbox.sent":
                    if (data.Deserialize(IpcJson.Default.OutboxSentEvent) is { } sent)
                        Post(() => OutboxSent?.Invoke(this, sent));
                    break;
                case "outbox.failed":
                    if (data.Deserialize(IpcJson.Default.OutboxFailedEvent) is { } failed)
                        Post(() => OutboxFailed?.Invoke(this, failed));
                    break;
                case "auth.progress":
                    if (data.Deserialize(IpcJson.Default.AuthProgressEvent) is { } progress)
                        Post(() => AuthProgress?.Invoke(this, progress));
                    break;
                case "ui.show":
                    var show = data.ValueKind == JsonValueKind.Object
                        ? data.Deserialize(IpcJson.Default.UiShowEvent) ?? new UiShowEvent()
                        : new UiShowEvent();
                    Post(() => UiShow?.Invoke(this, show));
                    break;
                case "settings.changed":
                    if (data.Deserialize(IpcJson.Default.Settings) is { } settings)
                        Post(() => SettingsChanged?.Invoke(this, settings));
                    break;
                default:
                    Debug.WriteLine($"[ipc] unknown event {name}");
                    break;
            }
        }
        catch (Exception ex) when (ex is JsonException or InvalidOperationException)
        {
            Debug.WriteLine($"[ipc] bad event payload for {name}: {ex.Message}");
        }
    }

    private void Post(DispatcherQueueHandler handler) => _dispatcher.TryEnqueue(handler);

    // ------------------------------------------------------------------ requests

    private abstract class PendingRequest
    {
        public abstract void Complete(JsonElement result);
        public abstract void Fail(Exception error);
    }

    private sealed class PendingRequest<T>(JsonTypeInfo<T> typeInfo) : PendingRequest
    {
        private readonly TaskCompletionSource<T?> _tcs = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public Task<T?> Task => _tcs.Task;

        public override void Complete(JsonElement result) =>
            _tcs.TrySetResult(result.ValueKind is JsonValueKind.Undefined or JsonValueKind.Null
                ? default
                : result.Deserialize(typeInfo));

        public override void Fail(Exception error) => _tcs.TrySetException(error);
    }

    private void FailAllPending(Exception error)
    {
        foreach (var key in _pending.Keys)
        {
            if (_pending.TryRemove(key, out var pending)) pending.Fail(error);
        }
    }

    private async Task<NamedPipeClientStream> WaitReadyAsync(CancellationToken ct)
    {
        var pipe = Volatile.Read(ref _pipe);
        if (pipe is not null) return pipe;
        try
        {
            return await _ready.Task.WaitAsync(ReadyTimeout, ct).ConfigureAwait(false);
        }
        catch (TimeoutException)
        {
            throw new DaemonException(DaemonErrorCodes.Unavailable, "NM Mail service not reachable");
        }
    }

    private async Task<TResult?> CallAsync<TParams, TResult>(string method, TParams parameters,
        JsonTypeInfo<TParams> paramsInfo, JsonTypeInfo<TResult> resultInfo, CancellationToken ct, TimeSpan? timeout = null)
    {
        var pipe = await WaitReadyAsync(ct).ConfigureAwait(false);
        return await SendRequestAsync(pipe, method, parameters, paramsInfo, resultInfo, timeout ?? DefaultTimeout, ct).ConfigureAwait(false);
    }

    private async Task<TResult?> SendRequestAsync<TParams, TResult>(NamedPipeClientStream pipe, string method, TParams parameters,
        JsonTypeInfo<TParams> paramsInfo, JsonTypeInfo<TResult> resultInfo, TimeSpan timeout, CancellationToken ct)
    {
        var id = Interlocked.Increment(ref _nextId);
        var pending = new PendingRequest<TResult>(resultInfo);
        _pending[id] = pending;
        try
        {
            var payload = Encode(id, method, parameters, paramsInfo);
            await _writeLock.WaitAsync(ct).ConfigureAwait(false);
            try
            {
                await pipe.WriteAsync(payload, ct).ConfigureAwait(false);
                await pipe.FlushAsync(ct).ConfigureAwait(false);
            }
            finally
            {
                _writeLock.Release();
            }
            return await pending.Task.WaitAsync(timeout, ct).ConfigureAwait(false);
        }
        catch (TimeoutException)
        {
            throw new DaemonException(DaemonErrorCodes.Timeout, $"{method}: no response from NM Mail service");
        }
        catch (Exception ex) when (ex is IOException or ObjectDisposedException)
        {
            throw new DaemonException(DaemonErrorCodes.Unavailable, ex.Message);
        }
        finally
        {
            _pending.TryRemove(id, out _);
        }
    }

    private static ReadOnlyMemory<byte> Encode<TParams>(long id, string method, TParams parameters, JsonTypeInfo<TParams> info)
    {
        var buffer = new ArrayBufferWriter<byte>(256);
        using (var writer = new Utf8JsonWriter(buffer, WriterOptions))
        {
            writer.WriteStartObject();
            writer.WriteNumber("id", id);
            writer.WriteString("method", method);
            writer.WritePropertyName("params");
            JsonSerializer.Serialize(writer, parameters, info);
            writer.WriteEndObject();
        }
        buffer.Write("\n"u8);
        return buffer.WrittenMemory;
    }

    private Task<EmptyResult?> CallEmptyAsync<TParams>(string method, TParams parameters, JsonTypeInfo<TParams> info, CancellationToken ct) =>
        CallAsync(method, parameters, info, IpcJson.Default.EmptyResult, ct);

    private static T Require<T>(T? value, string method) where T : class =>
        value ?? throw new DaemonException(DaemonErrorCodes.Internal, $"{method}: empty result");

    // ------------------------------------------------------------------ typed API

    public async Task<DaemonStatus> GetStatusAsync(CancellationToken ct = default) =>
        Require(await CallAsync("daemon.status", EmptyParams.Instance, IpcJson.Default.EmptyParams, IpcJson.Default.DaemonStatus, ct), "daemon.status");

    public Task ShutdownAsync(CancellationToken ct = default) =>
        CallEmptyAsync("daemon.shutdown", EmptyParams.Instance, IpcJson.Default.EmptyParams, ct);

    public async Task<IReadOnlyList<Account>> ListAccountsAsync(CancellationToken ct = default) =>
        (await CallAsync("accounts.list", EmptyParams.Instance, IpcJson.Default.EmptyParams, IpcJson.Default.AccountsResult, ct))?.Accounts ?? [];

    public async Task<Account> AddOAuthAccountAsync(string provider, CancellationToken ct = default) =>
        Require((await CallAsync("accounts.addOAuth", new ProviderParams { Provider = provider }, IpcJson.Default.ProviderParams,
            IpcJson.Default.AccountResult, ct, Timeout.InfiniteTimeSpan))?.Account, "accounts.addOAuth");

    public Task CancelAuthAsync(CancellationToken ct = default) =>
        CallEmptyAsync("accounts.cancelAuth", EmptyParams.Instance, IpcJson.Default.EmptyParams, ct);

    public async Task<Account> AddImapAccountAsync(AddImapParams request, CancellationToken ct = default) =>
        Require((await CallAsync("accounts.addImap", request, IpcJson.Default.AddImapParams, IpcJson.Default.AccountResult, ct,
            TimeSpan.FromMinutes(2)))?.Account, "accounts.addImap");

    public async Task<Account> UpdateAccountAsync(UpdateAccountParams request, CancellationToken ct = default) =>
        Require((await CallAsync("accounts.update", request, IpcJson.Default.UpdateAccountParams, IpcJson.Default.AccountResult, ct))?.Account, "accounts.update");

    public Task RemoveAccountAsync(long id, CancellationToken ct = default) =>
        CallEmptyAsync("accounts.remove", new IdParams { Id = id }, IpcJson.Default.IdParams, ct);

    public async Task<IReadOnlyList<Folder>> ListFoldersAsync(long? accountId = null, CancellationToken ct = default) =>
        (await CallAsync("folders.list", new AccountFilterParams { AccountId = accountId }, IpcJson.Default.AccountFilterParams,
            IpcJson.Default.FoldersResult, ct))?.Folders ?? [];

    public Task SyncFolderAsync(long folderId, CancellationToken ct = default) =>
        CallEmptyAsync("folders.sync", new FolderIdParams { FolderId = folderId }, IpcJson.Default.FolderIdParams, ct);

    public async Task<Folder> SetFolderMonitoredAsync(long folderId, bool monitored, CancellationToken ct = default) =>
        Require((await CallAsync("folders.setMonitored", new SetMonitoredParams { FolderId = folderId, Monitored = monitored },
            IpcJson.Default.SetMonitoredParams, IpcJson.Default.FolderResult, ct))?.Folder, "folders.setMonitored");

    public async Task<MessagePage> ListMessagesAsync(ListMessagesParams request, CancellationToken ct = default) =>
        await CallAsync("messages.list", request, IpcJson.Default.ListMessagesParams, IpcJson.Default.MessagePage, ct) ?? new MessagePage();

    public async Task<MessagePage> LoadOlderAsync(long folderId, long beforeDate, CancellationToken ct = default) =>
        await CallAsync("messages.loadOlder", new LoadOlderParams { FolderId = folderId, BeforeDate = beforeDate },
            IpcJson.Default.LoadOlderParams, IpcJson.Default.MessagePage, ct, TimeSpan.FromMinutes(2)) ?? new MessagePage();

    public async Task<MessageDetail> GetMessageAsync(long id, bool allowRemote = false, CancellationToken ct = default) =>
        Require((await CallAsync("messages.get", new GetMessageParams { Id = id, AllowRemote = allowRemote ? true : null },
            IpcJson.Default.GetMessageParams, IpcJson.Default.MessageResult, ct, TimeSpan.FromMinutes(1)))?.Message, "messages.get");

    public Task SetFlagsAsync(IReadOnlyCollection<long> ids, bool? seen = null, bool? flagged = null, CancellationToken ct = default) =>
        CallEmptyAsync("messages.setFlags", new SetFlagsParams { Ids = [.. ids], Seen = seen, Flagged = flagged }, IpcJson.Default.SetFlagsParams, ct);

    public Task ArchiveAsync(IReadOnlyCollection<long> ids, CancellationToken ct = default) =>
        CallEmptyAsync("messages.archive", new IdsParams { Ids = [.. ids] }, IpcJson.Default.IdsParams, ct);

    public Task DeleteAsync(IReadOnlyCollection<long> ids, CancellationToken ct = default) =>
        CallEmptyAsync("messages.delete", new IdsParams { Ids = [.. ids] }, IpcJson.Default.IdsParams, ct);

    public Task MoveAsync(IReadOnlyCollection<long> ids, long folderId, CancellationToken ct = default) =>
        CallEmptyAsync("messages.move", new MoveParams { Ids = [.. ids], FolderId = folderId }, IpcJson.Default.MoveParams, ct);

    public async Task<IReadOnlyList<MessageSummary>> SearchAsync(string query, long? accountId = null, int? limit = null, CancellationToken ct = default) =>
        (await CallAsync("messages.search", new SearchParams { Query = query, AccountId = accountId, Limit = limit },
            IpcJson.Default.SearchParams, IpcJson.Default.MessagePage, ct))?.Messages ?? [];

    public async Task<string> SaveAttachmentAsync(long messageId, long partId, string path, CancellationToken ct = default) =>
        (await CallAsync("attachments.save", new SaveAttachmentParams { MessageId = messageId, PartId = partId, Path = path },
            IpcJson.Default.SaveAttachmentParams, IpcJson.Default.PathResult, ct, TimeSpan.FromMinutes(5)))?.Path is { Length: > 0 } saved ? saved : path;

    public async Task<IReadOnlyList<Addr>> SuggestContactsAsync(string prefix, int? limit = null, CancellationToken ct = default) =>
        (await CallAsync("contacts.suggest", new SuggestContactsParams { Prefix = prefix, Limit = limit },
            IpcJson.Default.SuggestContactsParams, IpcJson.Default.ContactsResult, ct))?.Contacts ?? [];

    public async Task<ComposePrepared> PrepareComposeAsync(string mode, long messageId, string? lang = null, CancellationToken ct = default) =>
        Require(await CallAsync("compose.prepare", new ComposePrepareParams { Mode = mode, MessageId = messageId, Lang = lang },
            IpcJson.Default.ComposePrepareParams, IpcJson.Default.ComposePrepared, ct, TimeSpan.FromMinutes(1)), "compose.prepare");

    public async Task<SendResult> SendAsync(ComposeSendParams request, CancellationToken ct = default) =>
        Require(await CallAsync("compose.send", request, IpcJson.Default.ComposeSendParams, IpcJson.Default.SendResult, ct,
            TimeSpan.FromMinutes(2)), "compose.send");

    public async Task<Settings> GetSettingsAsync(CancellationToken ct = default) =>
        Require((await CallAsync("settings.get", EmptyParams.Instance, IpcJson.Default.EmptyParams, IpcJson.Default.SettingsResult, ct))?.Settings, "settings.get");

    public async Task<Settings> SetSettingsAsync(SettingsPatch patch, CancellationToken ct = default) =>
        Require((await CallAsync("settings.set", patch, IpcJson.Default.SettingsPatch, IpcJson.Default.SettingsResult, ct))?.Settings, "settings.set");

    public Task SyncNowAsync(long? accountId = null, CancellationToken ct = default) =>
        CallEmptyAsync("sync.now", new AccountFilterParams { AccountId = accountId }, IpcJson.Default.AccountFilterParams, ct);

    public Task PauseNotificationsAsync(long? until, CancellationToken ct = default) =>
        CallEmptyAsync("notifications.pause", new PauseNotificationsParams { Until = until }, IpcJson.Default.PauseNotificationsParams, ct);

    public async Task<string> TestNotificationAsync(CancellationToken ct = default) =>
        (await CallAsync("notifications.test", EmptyParams.Instance, IpcJson.Default.EmptyParams, IpcJson.Default.NotificationTestResult, ct))?.Status ?? "";

    public Task ShowUiAsync(long? messageId = null, CancellationToken ct = default) =>
        CallEmptyAsync("ui.show", new UiShowParams { MessageId = messageId }, IpcJson.Default.UiShowParams, ct);

    public Task ActivateAsync(string uri, CancellationToken ct = default) =>
        CallEmptyAsync("activate", new ActivateParams { Uri = uri }, IpcJson.Default.ActivateParams, ct);
}

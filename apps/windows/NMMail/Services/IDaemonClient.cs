using NMMail.Models;

namespace NMMail.Services;

public enum DaemonConnectionState
{
    Connecting,
    Connected,
    Disconnected,
}

/// <summary>Error returned by the daemon (or synthesized by the client, see <see cref="DaemonErrorCodes"/>).</summary>
public sealed class DaemonException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

/// <summary>
/// Typed access to every method and event of docs/IPC.md. Events are always raised on the UI thread.
/// </summary>
public interface IDaemonClient : IAsyncDisposable
{
    bool IsDemo { get; }
    DaemonConnectionState ConnectionState { get; }
    string? DaemonVersion { get; }

    event EventHandler<DaemonConnectionState>? ConnectionStateChanged;

    // Events pushed by the daemon.
    event EventHandler<Account>? AccountStatus;
    event EventHandler? AccountsChanged;
    event EventHandler<FoldersChangedEvent>? FoldersChanged;
    event EventHandler<MessagesChangedEvent>? MessagesChanged;
    event EventHandler<MailNewEvent>? MailNew;
    event EventHandler<OutboxSentEvent>? OutboxSent;
    event EventHandler<OutboxFailedEvent>? OutboxFailed;
    event EventHandler<AuthProgressEvent>? AuthProgress;
    event EventHandler<UiShowEvent>? UiShow;
    event EventHandler<Settings>? SettingsChanged;

    /// <summary>Starts connecting (and keeps reconnecting) in the background.</summary>
    void Start();

    Task<DaemonStatus> GetStatusAsync(CancellationToken ct = default);
    Task ShutdownAsync(CancellationToken ct = default);

    Task<IReadOnlyList<Account>> ListAccountsAsync(CancellationToken ct = default);
    Task<Account> AddOAuthAccountAsync(string provider, CancellationToken ct = default);
    Task CancelAuthAsync(CancellationToken ct = default);
    Task<Account> AddImapAccountAsync(AddImapParams request, CancellationToken ct = default);
    Task<Account> UpdateAccountAsync(UpdateAccountParams request, CancellationToken ct = default);
    Task RemoveAccountAsync(long id, CancellationToken ct = default);

    Task<IReadOnlyList<Folder>> ListFoldersAsync(long? accountId = null, CancellationToken ct = default);
    Task SyncFolderAsync(long folderId, CancellationToken ct = default);
    /// <summary>Instant push + notifications for a folder (mail moved by rules/filters).</summary>
    Task<Folder> SetFolderMonitoredAsync(long folderId, bool monitored, CancellationToken ct = default);

    Task<MessagePage> ListMessagesAsync(ListMessagesParams request, CancellationToken ct = default);
    Task<MessagePage> LoadOlderAsync(long folderId, long beforeDate, CancellationToken ct = default);
    Task<MessageDetail> GetMessageAsync(long id, bool allowRemote = false, CancellationToken ct = default);
    Task SetFlagsAsync(IReadOnlyCollection<long> ids, bool? seen = null, bool? flagged = null, CancellationToken ct = default);
    Task ArchiveAsync(IReadOnlyCollection<long> ids, CancellationToken ct = default);
    Task DeleteAsync(IReadOnlyCollection<long> ids, CancellationToken ct = default);
    Task MoveAsync(IReadOnlyCollection<long> ids, long folderId, CancellationToken ct = default);
    Task<IReadOnlyList<MessageSummary>> SearchAsync(string query, long? accountId = null, int? limit = null, CancellationToken ct = default);

    Task<string> SaveAttachmentAsync(long messageId, long partId, string path, CancellationToken ct = default);
    Task<IReadOnlyList<Addr>> SuggestContactsAsync(string prefix, int? limit = null, CancellationToken ct = default);

    /// <param name="lang">"it" or "en": language of the quote header.</param>
    Task<ComposePrepared> PrepareComposeAsync(string mode, long messageId, string? lang = null, CancellationToken ct = default);

    /// <summary>Sends (or queues when offline: <see cref="SendResult.Queued"/>). Permanent errors throw.</summary>
    Task<SendResult> SendAsync(ComposeSendParams request, CancellationToken ct = default);

    Task<Settings> GetSettingsAsync(CancellationToken ct = default);
    Task<Settings> SetSettingsAsync(SettingsPatch patch, CancellationToken ct = default);

    Task SyncNowAsync(long? accountId = null, CancellationToken ct = default);
    Task PauseNotificationsAsync(long? until, CancellationToken ct = default);

    /// <summary>notifications.test: shows a test toast; returns the daemon's status string.</summary>
    Task<string> TestNotificationAsync(CancellationToken ct = default);

    /// <summary>ui.show method (used by tray/notifications; the UI normally only receives the event).</summary>
    Task ShowUiAsync(long? messageId = null, CancellationToken ct = default);

    /// <summary>activate method (internal: notification clicks, nmmail:// URIs).</summary>
    Task ActivateAsync(string uri, CancellationToken ct = default);
}

using System.Text.Json;
using System.Text.Json.Serialization;

namespace NMMail.Models;

// Typed models for the daemon <-> UI protocol (docs/IPC.md, v1).
// Enum-like protocol values are kept as strings (with constants below) so that a newer daemon adding a
// value never breaks deserialization of a whole response.

public static class AccountStates
{
    public const string Connecting = "connecting";
    public const string Online = "online";
    public const string Offline = "offline";
    public const string Error = "error";
    public const string AuthRequired = "authRequired";
}

public static class Providers
{
    public const string Gmail = "gmail";
    public const string Outlook = "outlook";
    public const string Imap = "imap";
}

public static class FolderRoles
{
    public const string Inbox = "inbox";
    public const string Sent = "sent";
    public const string Drafts = "drafts";
    public const string Trash = "trash";
    public const string Archive = "archive";
    public const string Spam = "spam";
    public const string All = "all";
    public const string Flagged = "flagged";
    public const string Important = "important";
}

public static class NotifyFilterNames
{
    public const string All = "all";
    public const string Primary = "primary";
}

public static class ComposeModes
{
    public const string New = "new"; // UI only, never sent to the daemon
    public const string Reply = "reply";
    public const string ReplyAll = "replyAll";
    public const string Forward = "forward";
}

public static class ConnectionSecurity
{
    public const string Tls = "tls";
    public const string StartTls = "starttls";
    public const string None = "none";
}

public static class AuthStages
{
    public const string Browser = "browser";
    public const string Exchanging = "exchanging";
    public const string Connecting = "connecting";
    public const string Done = "done";
}

public static class ThemeNames
{
    public const string System = "system";
    public const string Light = "light";
    public const string Dark = "dark";
}

public static class LanguageNames
{
    public const string System = "system";
    public const string Italian = "it";
    public const string English = "en";
}

public static class DaemonErrorCodes
{
    public const string BadRequest = "badRequest";
    public const string NotFound = "notFound";
    public const string AuthRequired = "authRequired";
    public const string Offline = "offline";
    public const string Cancelled = "cancelled";
    public const string Server = "server";
    public const string Internal = "internal";

    // Client-side only.
    public const string Unavailable = "unavailable";
    public const string Timeout = "timeout";
}

// ---------------------------------------------------------------- types

[JsonConverter(typeof(AddrJsonConverter))]
public sealed class Addr
{
    public string? Name { get; set; }
    public string Address { get; set; } = "";

    public string DisplayName => string.IsNullOrWhiteSpace(Name) ? Address : Name!;

    public override string ToString() =>
        string.IsNullOrWhiteSpace(Name) ? Address : $"{Name} <{Address}>";

    /// <summary>Parses "Name &lt;a@b&gt;", "&lt;a@b&gt;" or "a@b".</summary>
    public static Addr Parse(string text)
    {
        text = text.Trim();
        var lt = text.LastIndexOf('<');
        var gt = text.LastIndexOf('>');
        if (lt >= 0 && gt > lt)
        {
            var address = text[(lt + 1)..gt].Trim();
            var name = text[..lt].Trim().Trim('"').Trim();
            return new Addr { Name = name.Length == 0 ? null : name, Address = address };
        }
        return new Addr { Address = text.Trim('"', ' ') };
    }
}

/// <summary>Accepts both {"name":..,"address":..} objects and plain "a@b" strings.</summary>
public sealed class AddrJsonConverter : JsonConverter<Addr>
{
    public override Addr? Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        switch (reader.TokenType)
        {
            case JsonTokenType.Null:
                return null;
            case JsonTokenType.String:
                return Addr.Parse(reader.GetString() ?? "");
            case JsonTokenType.StartObject:
                string? name = null;
                var address = "";
                while (reader.Read() && reader.TokenType != JsonTokenType.EndObject)
                {
                    if (reader.TokenType != JsonTokenType.PropertyName)
                    {
                        reader.Skip();
                        continue;
                    }
                    var prop = reader.GetString();
                    reader.Read();
                    if (reader.TokenType == JsonTokenType.String && prop == "name")
                        name = reader.GetString();
                    else if (reader.TokenType == JsonTokenType.String && prop == "address")
                        address = reader.GetString() ?? "";
                    else
                        reader.Skip();
                }
                return new Addr { Name = name, Address = address };
            default:
                throw new JsonException("Invalid address");
        }
    }

    public override void Write(Utf8JsonWriter writer, Addr value, JsonSerializerOptions options)
    {
        writer.WriteStartObject();
        if (string.IsNullOrWhiteSpace(value.Name))
            writer.WriteNull("name");
        else
            writer.WriteString("name", value.Name);
        writer.WriteString("address", value.Address);
        writer.WriteEndObject();
    }
}

public sealed class Account
{
    public long Id { get; set; }
    public string Provider { get; set; } = Providers.Imap;
    public string Email { get; set; } = "";
    public string DisplayName { get; set; } = "";
    public string State { get; set; } = AccountStates.Offline;
    public string? Error { get; set; }
    public long? LastSync { get; set; }
    public int Unread { get; set; }
    public bool Notify { get; set; } = true;
    public string NotifyFilter { get; set; } = NotifyFilterNames.All;

    public Account Clone() => (Account)MemberwiseClone();
}

public sealed class Folder
{
    public long Id { get; set; }
    public long AccountId { get; set; }
    public string Name { get; set; } = "";
    public string DisplayName { get; set; } = "";
    public string? Role { get; set; }
    public long? ParentId { get; set; }
    public int Depth { get; set; }
    public int Unread { get; set; }
    public int Total { get; set; }
    public bool Selectable { get; set; } = true;
    /// <summary>New mail here is pushed and notified (always true for INBOX).</summary>
    public bool Monitored { get; set; }

    public Folder Clone() => (Folder)MemberwiseClone();
}

public class MessageSummary
{
    public long Id { get; set; }
    public long AccountId { get; set; }
    public long FolderId { get; set; }
    public string Subject { get; set; } = "";
    public Addr? From { get; set; }
    public Addr[] To { get; set; } = [];
    public long Date { get; set; }
    public string Snippet { get; set; } = "";
    public bool Seen { get; set; }
    public bool Flagged { get; set; }
    public bool Answered { get; set; }
    public bool HasAttachments { get; set; }
    public long Size { get; set; }
}

public sealed class Attachment
{
    public long PartId { get; set; }
    public string Filename { get; set; } = "";
    public string Mime { get; set; } = "";
    public long Size { get; set; }
    public bool Inline { get; set; }
}

public sealed class MessageDetail : MessageSummary
{
    public Addr[] Cc { get; set; } = [];
    public Addr[] Bcc { get; set; } = [];
    public Addr[] ReplyTo { get; set; } = [];
    public string? MessageIdHeader { get; set; }
    public string? Html { get; set; }
    public string? Text { get; set; }
    public Attachment[] Attachments { get; set; } = [];
    public bool HasRemoteContent { get; set; }
    public bool RemoteAllowed { get; set; }
}

public sealed class Settings
{
    public bool Autostart { get; set; } = true;
    public bool NotificationsEnabled { get; set; } = true;
    public bool NotificationSound { get; set; } = true;
    public bool ShowPreview { get; set; } = true;
    public int CacheDays { get; set; } = 90;
    public string Theme { get; set; } = ThemeNames.System;
    public string Language { get; set; } = LanguageNames.System;
    public long? QuietUntil { get; set; }
    public string GoogleClientId { get; set; } = "";
    public string GoogleClientSecret { get; set; } = "";
    public string MicrosoftClientId { get; set; } = "";

    public Settings Clone() => (Settings)MemberwiseClone();
}

// ---------------------------------------------------------------- request params

public sealed class EmptyParams
{
    public static readonly EmptyParams Instance = new();
}

public sealed class HelloParams
{
    public string Client { get; set; } = "";
    public string Version { get; set; } = "";
}

public sealed class IdParams
{
    public long Id { get; set; }
}

public sealed class IdsParams
{
    public long[] Ids { get; set; } = [];
}

public sealed class AccountFilterParams
{
    public long? AccountId { get; set; }
}

public sealed class ProviderParams
{
    public string Provider { get; set; } = "";
}

public sealed class AddImapParams
{
    public string Email { get; set; } = "";
    public string DisplayName { get; set; } = "";
    public string? Username { get; set; }
    public string Password { get; set; } = "";
    public string ImapHost { get; set; } = "";
    public int ImapPort { get; set; } = 993;
    public string ImapSecurity { get; set; } = ConnectionSecurity.Tls;
    public string SmtpHost { get; set; } = "";
    public int SmtpPort { get; set; } = 465;
    public string SmtpSecurity { get; set; } = ConnectionSecurity.Tls;
}

public sealed class UpdateAccountParams
{
    public long Id { get; set; }
    public string? DisplayName { get; set; }
    public bool? Notify { get; set; }
    public string? NotifyFilter { get; set; }
}

public sealed class SetMonitoredParams
{
    public long FolderId { get; set; }
    public bool Monitored { get; set; }
}

public sealed class FolderIdParams
{
    public long FolderId { get; set; }
}

public sealed class ListMessagesParams
{
    public long? FolderId { get; set; }
    public string? Unified { get; set; }
    public long? BeforeDate { get; set; }
    public long? BeforeId { get; set; }
    public int? Limit { get; set; }
    public bool? UnreadOnly { get; set; }
}

public sealed class LoadOlderParams
{
    public long FolderId { get; set; }
    public long BeforeDate { get; set; }
}

public sealed class GetMessageParams
{
    public long Id { get; set; }
    public bool? AllowRemote { get; set; }
}

public sealed class SetFlagsParams
{
    public long[] Ids { get; set; } = [];
    public bool? Seen { get; set; }
    public bool? Flagged { get; set; }
}

public sealed class MoveParams
{
    public long[] Ids { get; set; } = [];
    public long FolderId { get; set; }
}

public sealed class SearchParams
{
    public string Query { get; set; } = "";
    public long? AccountId { get; set; }
    public int? Limit { get; set; }
}

public sealed class SaveAttachmentParams
{
    public long MessageId { get; set; }
    public long PartId { get; set; }
    public string Path { get; set; } = "";
}

public sealed class SuggestContactsParams
{
    public string Prefix { get; set; } = "";
    public int? Limit { get; set; }
}

public sealed class ComposePrepareParams
{
    public string Mode { get; set; } = ComposeModes.Reply;
    public long MessageId { get; set; }

    /// <summary>"it" | "en": language of the quote header ("Il giorno … ha scritto:").</summary>
    public string? Lang { get; set; }
}

public sealed class UiShowParams
{
    public long? MessageId { get; set; }
}

public sealed class ActivateParams
{
    public string Uri { get; set; } = "";
}

public sealed class ForwardAttachmentRef
{
    public long MessageId { get; set; }
    public long PartId { get; set; }
}

public sealed class ComposeSendParams
{
    public long AccountId { get; set; }
    public Addr[] To { get; set; } = [];
    public Addr[] Cc { get; set; } = [];
    public Addr[] Bcc { get; set; } = [];
    public string Subject { get; set; } = "";
    public string Html { get; set; } = "";
    public string Text { get; set; } = "";
    public string[] Attachments { get; set; } = [];
    public ForwardAttachmentRef[]? ForwardAttachments { get; set; }
    // Opaque values round-tripped from compose.prepare (string or string[] depending on the daemon).
    public JsonElement? InReplyTo { get; set; }
    public JsonElement? References { get; set; }
    public long? ReplyToMessageId { get; set; }
}

/// <summary>Partial&lt;Settings&gt;: null properties are not sent.</summary>
public sealed class SettingsPatch
{
    public bool? Autostart { get; set; }
    public bool? NotificationsEnabled { get; set; }
    public bool? NotificationSound { get; set; }
    public bool? ShowPreview { get; set; }
    public int? CacheDays { get; set; }
    public string? Theme { get; set; }
    public string? Language { get; set; }
    public string? GoogleClientId { get; set; }
    public string? GoogleClientSecret { get; set; }
    public string? MicrosoftClientId { get; set; }
}

public sealed class PauseNotificationsParams
{
    // null = resume; must be written explicitly.
    [JsonIgnore(Condition = JsonIgnoreCondition.Never)]
    public long? Until { get; set; }
}

// ---------------------------------------------------------------- results

public sealed class EmptyResult
{
}

public sealed class HelloResult
{
    public string DaemonVersion { get; set; } = "";
    public int Protocol { get; set; }
}

public sealed class DaemonStatus
{
    public string Version { get; set; } = "";
    public long Pid { get; set; }
    public long UptimeSec { get; set; }
    public long MemoryBytes { get; set; }
    public Account[] Accounts { get; set; } = [];

    /// <summary>Providers whose OAuth client is built into this daemon (nothing to configure).</summary>
    [JsonPropertyName("oauthBuiltIn")]
    public OAuthBuiltIn? OAuthBuiltIn { get; set; }
}

public sealed class OAuthBuiltIn
{
    public bool Google { get; set; }
    public bool Microsoft { get; set; }
}

public sealed class AccountsResult
{
    public Account[] Accounts { get; set; } = [];
}

public sealed class AccountResult
{
    public Account? Account { get; set; }
}

public sealed class FoldersResult
{
    public Folder[] Folders { get; set; } = [];
}

public sealed class FolderResult
{
    public Folder? Folder { get; set; }
}

public sealed class MessagePage
{
    public MessageSummary[] Messages { get; set; } = [];
    public bool HasMore { get; set; }
}

public sealed class MessageResult
{
    public MessageDetail? Message { get; set; }
}

public sealed class PathResult
{
    public string Path { get; set; } = "";
}

public sealed class ContactsResult
{
    public Addr[] Contacts { get; set; } = [];
}

public sealed class ComposeAttachment
{
    public long MessageId { get; set; }
    public long PartId { get; set; }
    public string Filename { get; set; } = "";
    public long Size { get; set; }
}

public sealed class ComposePrepared
{
    public long AccountId { get; set; }
    public Addr[] To { get; set; } = [];
    public Addr[] Cc { get; set; } = [];
    public string Subject { get; set; } = "";
    public string? Html { get; set; }
    public string? Text { get; set; }
    public JsonElement? InReplyTo { get; set; }
    public JsonElement? References { get; set; }
    public ComposeAttachment[] Attachments { get; set; } = [];
}

public sealed class SendResult
{
    public long OutboxId { get; set; }

    /// <summary>True when the daemon is offline: the message leaves automatically later (event outbox.sent).</summary>
    public bool Queued { get; set; }
}

public sealed class SettingsResult
{
    public Settings? Settings { get; set; }
}

public sealed class NotificationTestResult
{
    /// <summary>e.g. "enabled; actionCenter=1" or "disabledForUser; …".</summary>
    public string Status { get; set; } = "";
}

// ---------------------------------------------------------------- events

public sealed class FoldersChangedEvent
{
    public long AccountId { get; set; }
}

public sealed class MessagesChangedEvent
{
    public long AccountId { get; set; }
    public long[] FolderIds { get; set; } = [];
}

public sealed class MailNewEvent
{
    public long AccountId { get; set; }
    public long[] MessageIds { get; set; } = [];
}

public sealed class OutboxSentEvent
{
    public long OutboxId { get; set; }
}

public sealed class OutboxFailedEvent
{
    public long OutboxId { get; set; }
    public string? Error { get; set; }
}

public sealed class AuthProgressEvent
{
    public string Stage { get; set; } = "";
    public string? Url { get; set; }
}

public sealed class UiShowEvent
{
    public long? MessageId { get; set; }
}

// ---------------------------------------------------------------- local UI state (not part of the protocol)

public sealed class UiState
{
    public int? WindowX { get; set; }
    public int? WindowY { get; set; }
    public int? WindowWidth { get; set; }
    public int? WindowHeight { get; set; }
    public bool Maximized { get; set; }
    public double SidebarWidth { get; set; } = 264;
    public double ListWidth { get; set; } = 400;
    public bool SidebarCollapsed { get; set; }
    public string Theme { get; set; } = ThemeNames.System;
    public string Language { get; set; } = LanguageNames.System;
    public long[] CollapsedAccounts { get; set; } = [];
}

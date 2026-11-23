using System.Text.Json.Serialization;

namespace NMMail.Models;

/// <summary>
/// Source-generated (reflection-free, Native AOT safe) serialization for every IPC type.
/// </summary>
[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase,
    DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    GenerationMode = JsonSourceGenerationMode.Default)]
// types
[JsonSerializable(typeof(Addr))]
[JsonSerializable(typeof(Account))]
[JsonSerializable(typeof(Folder))]
[JsonSerializable(typeof(MessageSummary))]
[JsonSerializable(typeof(MessageDetail))]
[JsonSerializable(typeof(Attachment))]
[JsonSerializable(typeof(Settings))]
// params
[JsonSerializable(typeof(EmptyParams))]
[JsonSerializable(typeof(HelloParams))]
[JsonSerializable(typeof(IdParams))]
[JsonSerializable(typeof(IdsParams))]
[JsonSerializable(typeof(AccountFilterParams))]
[JsonSerializable(typeof(ProviderParams))]
[JsonSerializable(typeof(AddImapParams))]
[JsonSerializable(typeof(UpdateAccountParams))]
[JsonSerializable(typeof(FolderIdParams))]
[JsonSerializable(typeof(ListMessagesParams))]
[JsonSerializable(typeof(LoadOlderParams))]
[JsonSerializable(typeof(GetMessageParams))]
[JsonSerializable(typeof(SetFlagsParams))]
[JsonSerializable(typeof(MoveParams))]
[JsonSerializable(typeof(SearchParams))]
[JsonSerializable(typeof(SaveAttachmentParams))]
[JsonSerializable(typeof(SuggestContactsParams))]
[JsonSerializable(typeof(ComposePrepareParams))]
[JsonSerializable(typeof(ComposeSendParams))]
[JsonSerializable(typeof(SettingsPatch))]
[JsonSerializable(typeof(PauseNotificationsParams))]
[JsonSerializable(typeof(UiShowParams))]
[JsonSerializable(typeof(ActivateParams))]
// results
[JsonSerializable(typeof(EmptyResult))]
[JsonSerializable(typeof(HelloResult))]
[JsonSerializable(typeof(DaemonStatus))]
[JsonSerializable(typeof(AccountsResult))]
[JsonSerializable(typeof(AccountResult))]
[JsonSerializable(typeof(FoldersResult))]
[JsonSerializable(typeof(OAuthBuiltIn))]
[JsonSerializable(typeof(FolderResult))]
[JsonSerializable(typeof(SetMonitoredParams))]
[JsonSerializable(typeof(MessagePage))]
[JsonSerializable(typeof(MessageResult))]
[JsonSerializable(typeof(PathResult))]
[JsonSerializable(typeof(ContactsResult))]
[JsonSerializable(typeof(ComposePrepared))]
[JsonSerializable(typeof(SendResult))]
[JsonSerializable(typeof(SettingsResult))]
[JsonSerializable(typeof(NotificationTestResult))]
// events
[JsonSerializable(typeof(FoldersChangedEvent))]
[JsonSerializable(typeof(MessagesChangedEvent))]
[JsonSerializable(typeof(MailNewEvent))]
[JsonSerializable(typeof(OutboxSentEvent))]
[JsonSerializable(typeof(OutboxFailedEvent))]
[JsonSerializable(typeof(AuthProgressEvent))]
[JsonSerializable(typeof(UiShowEvent))]
// misc
[JsonSerializable(typeof(UiState))]
[JsonSerializable(typeof(string))]
[JsonSerializable(typeof(bool))]
internal sealed partial class IpcJson : JsonSerializerContext;

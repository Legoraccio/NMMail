using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Xaml.Media;
using NMMail.Helpers;
using NMMail.Models;
using NMMail.Services;

namespace NMMail.ViewModels;

public enum ReaderMode
{
    Empty,
    Message,
    Multiple,
    Error,
}

/// <summary>State of the reading pane. The view renders <see cref="Message"/> in WebView2 when <see cref="BodyReady"/> fires.</summary>
public sealed partial class ReaderViewModel : ObservableObject
{
    private readonly IDaemonClient _client;
    private CancellationTokenSource? _loadCts;

    public ReaderViewModel(IDaemonClient client)
    {
        _client = client;
        Subject = "";
        FromName = "";
        FromAddress = "";
        Initials = "";
        ToText = "";
        CcText = "";
        DateText = "";
        ErrorText = "";
        SelectionText = "";
        Mode = ReaderMode.Empty;
    }

    /// <summary>Raised when a message body must be (re)rendered.</summary>
    public event EventHandler<MessageDetail>? BodyReady;

    /// <summary>Raised when the reader goes back to an empty/multi-selection state.</summary>
    public event EventHandler? Cleared;

    public ObservableCollection<AttachmentItem> Attachments { get; } = [];

    public MessageItem? Item { get; private set; }
    public MessageDetail? Message { get; private set; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IsEmpty), nameof(IsMessage), nameof(IsMultiple), nameof(IsError))]
    public partial ReaderMode Mode { get; set; }

    [ObservableProperty]
    public partial bool IsLoading { get; set; }

    [ObservableProperty]
    public partial string Subject { get; set; }

    [ObservableProperty]
    public partial string FromName { get; set; }

    [ObservableProperty]
    public partial string FromAddress { get; set; }

    [ObservableProperty]
    public partial string Initials { get; set; }

    [ObservableProperty]
    public partial SolidColorBrush? AvatarBrush { get; set; }

    [ObservableProperty]
    public partial string ToText { get; set; }

    [ObservableProperty]
    public partial string CcText { get; set; }

    [ObservableProperty]
    public partial string DateText { get; set; }

    [ObservableProperty]
    public partial bool ShowRemoteBanner { get; set; }

    [ObservableProperty]
    public partial bool HasAttachments { get; set; }

    [ObservableProperty]
    public partial string ErrorText { get; set; }

    [ObservableProperty]
    public partial string SelectionText { get; set; }

    [ObservableProperty]
    public partial bool IsLoadingRemote { get; set; }

    /// <summary>Mirrors the shown list item's unread flag (drives the "Segna come letto/non letto" button).</summary>
    [ObservableProperty]
    public partial bool ItemIsUnread { get; set; }

    [ObservableProperty]
    public partial bool ItemIsFlagged { get; set; }

    private MessageItem? _tracked;

    private void Track(MessageItem? item)
    {
        if (ReferenceEquals(_tracked, item)) return;
        if (_tracked is not null) _tracked.PropertyChanged -= OnTrackedChanged;
        _tracked = item;
        if (item is null) return;
        item.PropertyChanged += OnTrackedChanged;
        ItemIsUnread = item.IsUnread;
        ItemIsFlagged = item.IsFlagged;
    }

    private void OnTrackedChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_tracked is null) return;
        if (e.PropertyName == nameof(MessageItem.IsUnread)) ItemIsUnread = _tracked.IsUnread;
        else if (e.PropertyName == nameof(MessageItem.IsFlagged)) ItemIsFlagged = _tracked.IsFlagged;
    }

    public bool IsEmpty => Mode == ReaderMode.Empty;
    public bool IsMessage => Mode == ReaderMode.Message;
    public bool IsMultiple => Mode == ReaderMode.Multiple;
    public bool IsError => Mode == ReaderMode.Error;

    public Task ShowAsync(MessageItem item) => LoadAsync(item.Id, item, allowRemote: false);

    public Task ShowByIdAsync(long id) => LoadAsync(id, null, allowRemote: false);

    public async Task ShowRemoteContentAsync()
    {
        if (Message is null) return;
        IsLoadingRemote = true;
        try
        {
            await LoadAsync(Message.Id, Item, allowRemote: true);
        }
        finally
        {
            IsLoadingRemote = false;
        }
    }

    private async Task LoadAsync(long id, MessageItem? item, bool allowRemote)
    {
        _loadCts?.Cancel();
        var cts = _loadCts = new CancellationTokenSource();
        Item = item;
        Track(item);
        if (!allowRemote)
        {
            Mode = ReaderMode.Message;
            // Show the header immediately from the list data: the body follows.
            if (item is not null) ApplyHeader(item.Summary, null);
        }
        IsLoading = true;
        try
        {
            var detail = await _client.GetMessageAsync(id, allowRemote, cts.Token);
            if (cts.IsCancellationRequested) return;
            Message = detail;
            ApplyHeader(detail, detail);
            if (item is null)
            {
                ItemIsUnread = !detail.Seen;
                ItemIsFlagged = detail.Flagged;
            }
            ShowRemoteBanner = detail.HasRemoteContent && !detail.RemoteAllowed;
            Attachments.Clear();
            foreach (var a in detail.Attachments ?? [])
            {
                if (!a.Inline) Attachments.Add(new AttachmentItem(detail.Id, a));
            }
            HasAttachments = Attachments.Count > 0;
            Mode = ReaderMode.Message;
            BodyReady?.Invoke(this, detail);
        }
        catch (OperationCanceledException)
        {
        }
        catch (Exception ex)
        {
            if (cts.IsCancellationRequested) return;
            ErrorText = ex is DaemonException { Code: DaemonErrorCodes.NotFound }
                ? Loc.Get("Reader_NotFound")
                : Loc.Get("Reader_LoadError") + " " + ex.Message;
            Mode = ReaderMode.Error;
            Cleared?.Invoke(this, EventArgs.Empty);
        }
        finally
        {
            if (ReferenceEquals(_loadCts, cts)) IsLoading = false;
        }
    }

    private void ApplyHeader(MessageSummary summary, MessageDetail? detail)
    {
        Subject = string.IsNullOrWhiteSpace(summary.Subject) ? Loc.Get("Message_NoSubject") : summary.Subject;
        var from = summary.From;
        FromName = from is null ? Loc.Get("Message_NoSender") : from.DisplayName;
        FromAddress = from is null || string.IsNullOrWhiteSpace(from.Name) ? "" : "<" + from.Address + ">";
        Initials = Fmt.Initials(from?.Name, from?.Address);
        AvatarBrush = Palette.AvatarBrush(from?.Address);
        ToText = Fmt.DisplayList(summary.To);
        CcText = detail is null ? "" : Fmt.DisplayList(detail.Cc);
        DateText = Fmt.FullDate(summary.Date);
        if (detail is null)
        {
            ShowRemoteBanner = false;
            HasAttachments = false;
            Attachments.Clear();
        }
    }

    public void ShowMultiple(int count)
    {
        CancelLoad();
        Track(null);
        Item = null;
        Message = null;
        SelectionText = Loc.Format("Reader_MultipleSelected", count);
        Mode = ReaderMode.Multiple;
        Cleared?.Invoke(this, EventArgs.Empty);
    }

    public void Clear()
    {
        CancelLoad();
        Track(null);
        Item = null;
        Message = null;
        Attachments.Clear();
        HasAttachments = false;
        ShowRemoteBanner = false;
        Mode = ReaderMode.Empty;
        Cleared?.Invoke(this, EventArgs.Empty);
    }

    private void CancelLoad()
    {
        _loadCts?.Cancel();
        _loadCts = null;
        IsLoading = false;
    }
}

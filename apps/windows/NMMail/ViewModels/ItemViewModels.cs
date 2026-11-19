using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using NMMail.Helpers;
using NMMail.Models;
using NMMail.Services;
using Windows.UI.Text;

namespace NMMail.ViewModels;

/// <summary>A row of the message list.</summary>
public sealed partial class MessageItem : ObservableObject
{
    public MessageItem(MessageSummary summary, bool showAccount)
    {
        Id = summary.Id;
        Summary = summary;
        ShowAccount = showAccount;
        SenderName = "";
        Subject = "";
        Snippet = "";
        TimeText = "";
        Apply(summary);
    }

    public long Id { get; }
    public MessageSummary Summary { get; private set; }
    public long AccountId => Summary.AccountId;
    public long FolderId => Summary.FolderId;
    public long Date => Summary.Date;
    public DateBucket Bucket { get; private set; }
    public bool ShowAccount { get; }
    public SolidColorBrush AccountBrush => Palette.AccountBrush(Summary.AccountId);

    [ObservableProperty]
    public partial string SenderName { get; set; }

    [ObservableProperty]
    public partial string Subject { get; set; }

    [ObservableProperty]
    public partial string Snippet { get; set; }

    [ObservableProperty]
    public partial string TimeText { get; set; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(Weight))]
    public partial bool IsUnread { get; set; }

    [ObservableProperty]
    public partial bool IsFlagged { get; set; }

    [ObservableProperty]
    public partial bool HasAttachments { get; set; }

    [ObservableProperty]
    public partial bool IsAnswered { get; set; }

    /// <summary>True while the pointer is over the row (shows the quick actions).</summary>
    [ObservableProperty]
    public partial bool IsHovered { get; set; }

    public FontWeight Weight => IsUnread ? FontWeights.SemiBold : FontWeights.Normal;

    public void Apply(MessageSummary s)
    {
        Summary = s;
        var from = s.From;
        SenderName = from is null ? Loc.Get("Message_NoSender") : from.DisplayName;
        Subject = string.IsNullOrWhiteSpace(s.Subject) ? Loc.Get("Message_NoSubject") : s.Subject;
        Snippet = s.Snippet ?? "";
        IsUnread = !s.Seen;
        IsFlagged = s.Flagged;
        HasAttachments = s.HasAttachments;
        IsAnswered = s.Answered;
        RefreshTime();
    }

    public void RefreshTime()
    {
        TimeText = Fmt.ListTime(Summary.Date);
        Bucket = DateBuckets.For(Summary.Date);
    }

    /// <summary>Accessible name of the list row (ListViewItem uses ToString()).</summary>
    public override string ToString() =>
        (IsUnread ? Loc.Get("Accessibility_Unread") + ", " : "") + SenderName + ", " + Subject + ", " + TimeText;

    /// <summary>Sort order: newest first, ties broken by id.</summary>
    public static int Compare(MessageItem a, MessageItem b)
    {
        var byDate = b.Date.CompareTo(a.Date);
        return byDate != 0 ? byDate : b.Id.CompareTo(a.Id);
    }
}

/// <summary>A date group of the message list (Oggi, Ieri, ...).</summary>
public sealed partial class MessageGroup : ObservableCollection<MessageItem>
{
    public MessageGroup(DateBucket bucket)
    {
        Bucket = bucket;
        Key = DateBuckets.Title(bucket);
    }

    public DateBucket Bucket { get; }
    public string Key { get; }

    public override string ToString() => Key;
}

public enum SidebarKind
{
    Unified,
    Account,
    Folder,
}

/// <summary>A row of the left pane: the unified inbox, an account header or a folder.</summary>
public sealed partial class SidebarItem : ObservableObject
{
    private SidebarItem(SidebarKind kind, string key)
    {
        Kind = kind;
        Key = key;
        Title = "";
        Subtitle = "";
        Glyph = Glyphs.Folder;
        State = AccountStates.Online;
        StateText = "";
    }

    public SidebarKind Kind { get; }
    public string Key { get; }
    public Account? Account { get; private set; }
    public Folder? Folder { get; private set; }
    public List<SidebarItem> Children { get; } = [];
    public int Depth { get; private set; }

    public bool HasChildren => Children.Count > 0;
    public bool IsSelectable => Kind == SidebarKind.Unified || (Kind == SidebarKind.Folder && Folder!.Selectable);
    public SolidColorBrush AccountBrush => Palette.AccountBrush(Account?.Id ?? Folder?.AccountId ?? 1);
    public Thickness Indent => new(Kind == SidebarKind.Folder ? 18 + 16 * Depth : 0, 0, 10, 0);
    public Visibility ChevronVisibility => HasChildren ? Visibility.Visible : Visibility.Collapsed;

    [ObservableProperty]
    public partial string Title { get; set; }

    [ObservableProperty]
    public partial string Subtitle { get; set; }

    [ObservableProperty]
    public partial string Glyph { get; set; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(UnreadText), nameof(HasUnread), nameof(TitleWeight))]
    public partial int Unread { get; set; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ChevronGlyph))]
    public partial bool IsExpanded { get; set; }

    [ObservableProperty]
    public partial bool IsSelected { get; set; }

    /// <summary>The folder's new mail is pushed and notified (rules/filters).</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowMonitoredBell), nameof(MonitorMenuText))]
    public partial bool IsMonitored { get; set; }

    /// <summary>The bell marks monitored folders other than the INBOX (always watched).</summary>
    public bool ShowMonitoredBell => IsMonitored && Folder?.Role != FolderRoles.Inbox;

    /// <summary>Only real folders other than the INBOX can be toggled.</summary>
    public bool CanToggleMonitoring => Kind == SidebarKind.Folder && Folder is { Selectable: true } f && f.Role != FolderRoles.Inbox;

    public string MonitorMenuText => Loc.Get(IsMonitored ? "Sidebar_StopNotifyFolder" : "Sidebar_NotifyFolder");

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(StateGlyph), nameof(StateBrush), nameof(ShowReauth), nameof(IsConnecting), nameof(ShowStateGlyph), nameof(StateGlyphSize))]
    public partial string State { get; set; }

    public double StateGlyphSize => State == AccountStates.Online ? 8 : 13;

    [ObservableProperty]
    public partial string StateText { get; set; }

    public string UnreadText => Fmt.Count(Unread);
    public bool HasUnread => Unread > 0;
    public FontWeight TitleWeight => Unread > 0 ? FontWeights.SemiBold : FontWeights.Normal;
    public string ChevronGlyph => IsExpanded ? Glyphs.ChevronDown : Glyphs.ChevronRight;
    public bool ShowReauth => State == AccountStates.AuthRequired;
    public bool IsConnecting => State == AccountStates.Connecting;
    public bool ShowStateGlyph => State != AccountStates.Connecting;

    public string StateGlyph => State switch
    {
        AccountStates.Online => "", // filled dot
        AccountStates.Offline => Glyphs.Offline,
        AccountStates.Error => Glyphs.Error,
        AccountStates.AuthRequired => Glyphs.Key,
        _ => "",
    };

    public SolidColorBrush StateBrush => State switch
    {
        AccountStates.Online => s_success ??= new SolidColorBrush(Palette.Success),
        AccountStates.AuthRequired => s_caution ??= new SolidColorBrush(Palette.Caution),
        AccountStates.Error => s_critical ??= new SolidColorBrush(Palette.Critical),
        _ => s_neutral ??= new SolidColorBrush(Palette.Neutral),
    };

    private static SolidColorBrush? s_success, s_caution, s_critical, s_neutral;

    public static SidebarItem CreateUnified() => new(SidebarKind.Unified, "unified")
    {
        Title = Loc.Get("Sidebar_UnifiedInbox"),
        Glyph = Glyphs.Mail,
    };

    public static SidebarItem CreateAccount(Account account)
    {
        var item = new SidebarItem(SidebarKind.Account, "acc:" + account.Id) { IsExpanded = true };
        item.Update(account);
        return item;
    }

    public static SidebarItem CreateFolder(Folder folder)
    {
        var item = new SidebarItem(SidebarKind.Folder, "fld:" + folder.Id) { IsExpanded = true };
        item.Update(folder);
        return item;
    }

    public void Update(Account account)
    {
        Account = account;
        Title = string.IsNullOrWhiteSpace(account.DisplayName) ? account.Email : account.DisplayName;
        Subtitle = account.Email;
        Unread = account.Unread;
        State = account.State;
        StateText = account.State switch
        {
            AccountStates.Online => Loc.Get("AccountState_Online"),
            AccountStates.Connecting => Loc.Get("AccountState_Connecting"),
            AccountStates.Offline => Loc.Get("AccountState_Offline"),
            AccountStates.AuthRequired => Loc.Get("AccountState_AuthRequired"),
            AccountStates.Error => string.IsNullOrWhiteSpace(account.Error)
                ? Loc.Get("AccountState_Error")
                : Loc.Get("AccountState_Error") + ": " + account.Error,
            _ => account.State,
        };
    }

    public void Update(Folder folder)
    {
        Folder = folder;
        Depth = Math.Max(0, folder.Depth);
        Title = string.IsNullOrWhiteSpace(folder.DisplayName) ? folder.Name : folder.DisplayName;
        Glyph = Glyphs.ForRole(folder.Role);
        IsMonitored = folder.Monitored || folder.Role == FolderRoles.Inbox;
        OnPropertyChanged(nameof(CanToggleMonitoring));
        Unread = folder.Role is FolderRoles.Sent or FolderRoles.Drafts or FolderRoles.Trash or FolderRoles.Spam or FolderRoles.All
            ? 0 // unread counts of these folders are noise
            : folder.Unread;
        OnPropertyChanged(nameof(Indent));
    }

    /// <summary>Accessible name of the sidebar row.</summary>
    public override string ToString() => Kind switch
    {
        SidebarKind.Account => Title + ", " + Subtitle + ", " + StateText,
        _ => Unread > 0 ? Title + ", " + Loc.Format("Accessibility_UnreadCount", Unread) : Title,
    };

    public void SetDepth(int depth)
    {
        if (Depth == depth) return;
        Depth = depth;
        OnPropertyChanged(nameof(Indent));
    }

    public void NotifyChildrenChanged() => OnPropertyChanged(nameof(ChevronVisibility));
}

/// <summary>An attachment chip in the reader.</summary>
public sealed class AttachmentItem(long messageId, Attachment attachment)
{
    public long MessageId { get; } = messageId;
    public Attachment Attachment { get; } = attachment;
    public string Name => string.IsNullOrWhiteSpace(Attachment.Filename) ? Loc.Get("Attachment_Unnamed") : Attachment.Filename;
    public string SizeText => Fmt.FileSize(Attachment.Size);
    public string Glyph => Glyphs.ForAttachment(Attachment.Mime, Attachment.Filename);
}

/// <summary>An attachment chip in the compose window (local file or forwarded part).</summary>
public sealed class ComposeAttachmentItem
{
    public string? Path { get; init; }
    public ComposeAttachment? Forwarded { get; init; }
    public required string Name { get; init; }
    public long Size { get; init; }
    public string SizeText => Fmt.FileSize(Size);
    public string Glyph => Glyphs.ForAttachment(null, Name);
}

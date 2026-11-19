using System.Collections.ObjectModel;
using System.Globalization;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using NMMail.Models;
using NMMail.Services;
using Windows.UI;
using Windows.UI.Text;

namespace NMMail.Helpers;

/// <summary>Formatting helpers, also used from x:Bind function bindings.</summary>
public static class Fmt
{
    public static DateTime ToLocal(long unixMs) => DateTimeOffset.FromUnixTimeMilliseconds(unixMs).LocalDateTime;

    /// <summary>List column: HH:mm for today, otherwise "dd MMM".</summary>
    public static string ListTime(long unixMs)
    {
        var local = ToLocal(unixMs);
        return local.Date == DateTime.Today
            ? local.ToString("HH:mm", CultureInfo.CurrentCulture)
            : local.ToString("dd MMM", CultureInfo.CurrentCulture);
    }

    /// <summary>Reader header: long date + short time in the current culture.</summary>
    public static string FullDate(long unixMs) => ToLocal(unixMs).ToString("f", CultureInfo.CurrentCulture);

    public static string FileSize(long bytes)
    {
        var culture = CultureInfo.CurrentCulture;
        if (bytes < 1024) return string.Format(culture, "{0} B", bytes);
        if (bytes < 1024 * 1024) return string.Format(culture, "{0:0} KB", Math.Ceiling(bytes / 1024.0));
        if (bytes < 1024L * 1024 * 1024) return string.Format(culture, "{0:0.#} MB", bytes / (1024.0 * 1024));
        return string.Format(culture, "{0:0.##} GB", bytes / (1024.0 * 1024 * 1024));
    }

    public static string Count(int value) =>
        value <= 0 ? "" : value > 999 ? "999+" : value.ToString(CultureInfo.CurrentCulture);

    public static string AddressList(IEnumerable<Addr>? list) =>
        list is null ? "" : string.Join(", ", list.Select(a => a.ToString()));

    public static string DisplayList(IEnumerable<Addr>? list) =>
        list is null ? "" : string.Join(", ", list.Select(a => a.DisplayName));

    public static string Initials(string? name, string? address)
    {
        var source = string.IsNullOrWhiteSpace(name) ? address ?? "" : name;
        source = source.Trim();
        if (source.Length == 0) return "?";
        var at = source.IndexOf('@');
        if (string.IsNullOrWhiteSpace(name) && at > 0) source = source[..at];
        var parts = source.Split([' ', '.', '_', '-'], StringSplitOptions.RemoveEmptyEntries);
        var initials = parts.Length >= 2
            ? $"{char.ToUpperInvariant(parts[0][0])}{char.ToUpperInvariant(parts[^1][0])}"
            : char.ToUpperInvariant(source[0]).ToString();
        return char.IsLetterOrDigit(initials[0]) ? initials : "?";
    }

    public static string Duration(long seconds)
    {
        var t = TimeSpan.FromSeconds(seconds);
        return t.TotalDays >= 1
            ? string.Format(CultureInfo.CurrentCulture, "{0}d {1}h {2}m", (int)t.TotalDays, t.Hours, t.Minutes)
            : string.Format(CultureInfo.CurrentCulture, "{0}h {1}m {2}s", t.Hours, t.Minutes, t.Seconds);
    }

    public static string Megabytes(long bytes) =>
        string.Format(CultureInfo.CurrentCulture, "{0:0.0} MB", bytes / (1024.0 * 1024.0));

    // ---- x:Bind helpers

    public static Visibility Visible(bool value) => value ? Visibility.Visible : Visibility.Collapsed;

    public static Visibility Collapsed(bool value) => value ? Visibility.Collapsed : Visibility.Visible;

    public static Visibility VisibleIfText(string? value) =>
        string.IsNullOrEmpty(value) ? Visibility.Collapsed : Visibility.Visible;

    public static Visibility VisibleIfAny(int count) => count > 0 ? Visibility.Visible : Visibility.Collapsed;

    public static Visibility VisibleIfEqual(int value, int expected) => value == expected ? Visibility.Visible : Visibility.Collapsed;

    public static FontWeight Weight(bool strong) => strong ? FontWeights.SemiBold : FontWeights.Normal;

    public static bool Not(bool value) => !value;

    /// <summary>Quick action glyph: open envelope = "mark as read", closed envelope = "mark as unread".</summary>
    public static string ReadToggleGlyph(bool isUnread) => isUnread ? Glyphs.MailRead : Glyphs.Mail;

    public static string ReadToggleLabel(bool isUnread) => Loc.Get(isUnread ? "Reader_MarkRead" : "Reader_MarkUnread");
}

public static class Glyphs
{
    public const string Mail = "";
    public const string MailRead = "";
    public const string Send = "";
    public const string Drafts = "";
    public const string Delete = "";
    public const string Archive = "";
    public const string Spam = "";
    public const string AllMail = "";
    public const string Flag = "";
    public const string Important = "";
    public const string Folder = "";
    public const string Settings = "";
    public const string Attach = "";
    public const string Reply = "";
    public const string ReplyAll = "";
    public const string Forward = "";
    public const string MoveTo = "";
    public const string Sync = "";
    public const string ChevronDown = "";
    public const string ChevronRight = "";
    public const string Offline = "";
    public const string Error = "";
    public const string Key = "";
    public const string Search = "";
    public const string Document = "";
    public const string Image = "";
    public const string Calendar = "";
    public const string Online = "";

    public static string ForRole(string? role) => role switch
    {
        FolderRoles.Inbox => Mail,
        FolderRoles.Sent => Send,
        FolderRoles.Drafts => Drafts,
        FolderRoles.Trash => Delete,
        FolderRoles.Archive => Archive,
        FolderRoles.Spam => Spam,
        FolderRoles.All => AllMail,
        FolderRoles.Flagged => Flag,
        FolderRoles.Important => Important,
        _ => Folder,
    };

    public static int RoleOrder(string? role) => role switch
    {
        FolderRoles.Inbox => 0,
        FolderRoles.Flagged => 1,
        FolderRoles.Important => 2,
        FolderRoles.Drafts => 3,
        FolderRoles.Sent => 4,
        FolderRoles.Archive => 5,
        FolderRoles.All => 6,
        FolderRoles.Spam => 7,
        FolderRoles.Trash => 8,
        _ => 9,
    };

    public static string ForAttachment(string? mime, string? filename)
    {
        mime ??= "";
        var ext = Path.GetExtension(filename ?? "").ToLowerInvariant();
        if (mime.StartsWith("image/", StringComparison.OrdinalIgnoreCase)) return Image;
        if (mime.Equals("text/calendar", StringComparison.OrdinalIgnoreCase) || ext == ".ics") return Calendar;
        return Document;
    }
}

/// <summary>Stable colors for accounts and sender avatars (brushes are created on the UI thread and cached).</summary>
public static class Palette
{
    private static readonly Color[] s_colors =
    [
        Color.FromArgb(255, 0x0F, 0x6C, 0xBD), // blue
        Color.FromArgb(255, 0xC2, 0x39, 0xB3), // magenta
        Color.FromArgb(255, 0x03, 0x83, 0x87), // teal
        Color.FromArgb(255, 0xCA, 0x50, 0x10), // orange
        Color.FromArgb(255, 0x87, 0x64, 0xB8), // purple
        Color.FromArgb(255, 0x49, 0x82, 0x05), // green
        Color.FromArgb(255, 0xD1, 0x34, 0x38), // red
        Color.FromArgb(255, 0x98, 0x6F, 0x0B), // gold
        Color.FromArgb(255, 0x00, 0x5B, 0x70), // petrol
        Color.FromArgb(255, 0x6B, 0x69, 0xD6), // indigo
    ];

    private static readonly SolidColorBrush?[] s_brushes = new SolidColorBrush?[s_colors.Length];

    public static SolidColorBrush AccountBrush(long accountId) => Brush((int)((accountId - 1) % s_colors.Length + s_colors.Length) % s_colors.Length);

    public static SolidColorBrush AvatarBrush(string? key)
    {
        // FNV-1a: stable across runs (string.GetHashCode is randomized).
        uint hash = 2166136261;
        foreach (var c in (key ?? "").ToLowerInvariant())
        {
            hash ^= c;
            hash *= 16777619;
        }
        return Brush((int)(hash % (uint)s_colors.Length));
    }

    private static SolidColorBrush Brush(int index) => s_brushes[index] ??= new SolidColorBrush(s_colors[index]);

    public static readonly Color Success = Color.FromArgb(255, 0x10, 0x7C, 0x10);
    public static readonly Color Caution = Color.FromArgb(255, 0xBC, 0x4B, 0x09);
    public static readonly Color Critical = Color.FromArgb(255, 0xC4, 0x2B, 0x1C);
    public static readonly Color Neutral = Colors.Gray;
}

public enum DateBucket
{
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Older,
}

public static class DateBuckets
{
    public static DateBucket For(long unixMs)
    {
        var local = Fmt.ToLocal(unixMs);
        var today = DateTime.Today;
        if (local >= today) return DateBucket.Today;
        if (local >= today.AddDays(-1)) return DateBucket.Yesterday;
        var firstDay = CultureInfo.CurrentCulture.DateTimeFormat.FirstDayOfWeek;
        var weekStart = today.AddDays(-(((int)today.DayOfWeek - (int)firstDay + 7) % 7));
        if (local >= weekStart) return DateBucket.ThisWeek;
        if (local >= new DateTime(today.Year, today.Month, 1)) return DateBucket.ThisMonth;
        return DateBucket.Older;
    }

    public static string Title(DateBucket bucket) => Loc.Get(bucket switch
    {
        DateBucket.Today => "Group_Today",
        DateBucket.Yesterday => "Group_Yesterday",
        DateBucket.ThisWeek => "Group_ThisWeek",
        DateBucket.ThisMonth => "Group_ThisMonth",
        _ => "Group_Older",
    });
}

public static class CollectionSync
{
    /// <summary>Applies the minimal moves/inserts/removes so that <paramref name="target"/> equals <paramref name="desired"/>.</summary>
    public static void Sync<T>(ObservableCollection<T> target, IReadOnlyList<T> desired) where T : class
    {
        var wanted = new HashSet<T>(desired, ReferenceEqualityComparer.Instance);
        for (var i = target.Count - 1; i >= 0; i--)
        {
            if (!wanted.Contains(target[i])) target.RemoveAt(i);
        }
        for (var i = 0; i < desired.Count; i++)
        {
            if (i < target.Count && ReferenceEquals(target[i], desired[i])) continue;
            var existing = -1;
            for (var j = i + 1; j < target.Count; j++)
            {
                if (ReferenceEquals(target[j], desired[i]))
                {
                    existing = j;
                    break;
                }
            }
            if (existing >= 0) target.Move(existing, i);
            else target.Insert(i, desired[i]);
        }
    }
}

using System.Diagnostics;
using System.Globalization;
using Microsoft.UI;
using Microsoft.Windows.Storage.Pickers;
using NMMail.ViewModels;

namespace NMMail.Services;

/// <summary>Thrown when an attachment type is never opened directly (executables, scripts, …).</summary>
public sealed class BlockedAttachmentException(string fileName) : Exception(fileName);

/// <summary>Open / "Salva con nome…" for attachments. Files are written by the daemon (attachments.save).</summary>
public static class AttachmentService
{
    private static readonly HashSet<string> s_dangerous = new(StringComparer.OrdinalIgnoreCase)
    {
        ".exe", ".com", ".bat", ".cmd", ".scr", ".pif", ".msi", ".msp", ".msix", ".msixbundle", ".appx", ".appxbundle",
        ".appinstaller", ".vbs", ".vbe", ".js", ".jse", ".wsf", ".wsh", ".ws", ".ps1", ".psm1", ".psd1", ".hta", ".cpl",
        ".jar", ".lnk", ".reg", ".dll", ".sys", ".application", ".gadget", ".msc", ".scf", ".url", ".inf", ".sct", ".chm",
        ".iso", ".img", ".vhd", ".vhdx", ".xll", ".library-ms", ".settingcontent-ms",
    };

    public static bool IsDangerous(string fileName) => s_dangerous.Contains(Path.GetExtension(fileName));

    public static string TempFolder(long messageId) =>
        Path.Combine(Path.GetTempPath(), "NMMail", messageId.ToString(CultureInfo.InvariantCulture));

    /// <summary>Saves to %TEMP%\NMMail\&lt;messageId&gt;\ and opens with the default app.</summary>
    public static async Task OpenAsync(IDaemonClient client, AttachmentItem attachment)
    {
        var name = SafeFileName(attachment.Name);
        if (IsDangerous(name)) throw new BlockedAttachmentException(name);
        var folder = TempFolder(attachment.MessageId);
        Directory.CreateDirectory(folder);
        var path = Path.Combine(folder, name);
        var saved = await client.SaveAttachmentAsync(attachment.MessageId, attachment.Attachment.PartId, path);
        MarkOfTheWeb(saved);
        Process.Start(new ProcessStartInfo(saved) { UseShellExecute = true })?.Dispose();
    }

    /// <summary>Asks for a destination with the Windows App SDK save picker (owned by <paramref name="owner"/>).</summary>
    public static async Task<string?> SaveAsAsync(IDaemonClient client, AttachmentItem attachment, WindowId owner)
    {
        var name = SafeFileName(attachment.Name);
        var extension = Path.GetExtension(name);
        if (string.IsNullOrEmpty(extension)) extension = ".bin";
        var picker = new FileSavePicker(owner)
        {
            SuggestedFileName = Path.GetFileNameWithoutExtension(name),
            SuggestedStartLocation = PickerLocationId.Downloads,
            DefaultFileExtension = extension,
        };
        picker.FileTypeChoices.Add(Loc.Format("Attachment_FileType", extension.TrimStart('.').ToUpperInvariant()), [extension]);
        var result = await picker.PickSaveFileAsync();
        if (result is null || string.IsNullOrEmpty(result.Path)) return null;
        var saved = await client.SaveAttachmentAsync(attachment.MessageId, attachment.Attachment.PartId, result.Path);
        MarkOfTheWeb(saved);
        return saved;
    }

    /// <summary>Zone.Identifier (Mark of the Web): Office opens in Protected View and SmartScreen checks the file.</summary>
    public static void MarkOfTheWeb(string path)
    {
        try
        {
            File.WriteAllText(path + ":Zone.Identifier", "[ZoneTransfer]\r\nZoneId=3\r\n");
        }
        catch (Exception ex)
        {
            AppInfo.Log("MOTW failed: " + ex.Message);
        }
    }

    public static string SafeFileName(string? name)
    {
        name = Path.GetFileName(name ?? "");
        var invalid = Path.GetInvalidFileNameChars();
        var chars = name.Select(c => invalid.Contains(c) || char.IsControl(c) ? '_' : c).ToArray();
        var safe = new string(chars).Trim().TrimEnd('.', ' ');
        if (safe.Length == 0) safe = "allegato";
        var stem = Path.GetFileNameWithoutExtension(safe);
        string[] reserved = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
            "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
        if (reserved.Contains(stem.ToUpperInvariant())) safe = "_" + safe;
        if (safe.Length > 120)
        {
            var ext = Path.GetExtension(safe);
            safe = safe[..(120 - ext.Length)] + ext;
        }
        return safe;
    }
}

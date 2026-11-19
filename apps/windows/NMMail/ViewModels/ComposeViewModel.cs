using System.Collections.ObjectModel;
using System.Net;
using System.Text;
using System.Text.Json;
using CommunityToolkit.Mvvm.ComponentModel;
using NMMail.Models;
using NMMail.Services;

namespace NMMail.ViewModels;

public sealed class ComposeValidationException(string message) : Exception(message);

/// <summary>State of a compose window (new, reply, reply all, forward, mailto:).</summary>
public sealed partial class ComposeViewModel : ObservableObject
{
    private readonly IDaemonClient _client;
    private readonly ComposeRequest _request;
    private JsonElement? _inReplyTo;
    private JsonElement? _references;
    private string _initialTo = "";
    private string _initialCc = "";
    private string _initialBcc = "";
    private string _initialSubject = "";
    private int _initialAttachments;

    public ComposeViewModel(IDaemonClient client, ComposeRequest request, IReadOnlyList<Account> accounts)
    {
        _client = client;
        _request = request;
        foreach (var account in accounts) Accounts.Add(account);
        var index = accounts.ToList().FindIndex(a => a.Id == request.AccountId);
        FromIndex = index >= 0 ? index : (accounts.Count > 0 ? 0 : -1);
        To = "";
        Cc = "";
        Bcc = "";
        Subject = "";
        ErrorMessage = "";
        InitialHtml = "";
        WindowTitle = Loc.Get("Compose_NewTitle");
    }

    public IDaemonClient Client => _client;
    public ObservableCollection<Account> Accounts { get; } = [];
    public ObservableCollection<ComposeAttachmentItem> Attachments { get; } = [];
    public ObservableCollection<Addr> Suggestions { get; } = [];
    public string InitialHtml { get; private set; }
    public string Mode => _request.Mode;

    [ObservableProperty]
    public partial int FromIndex { get; set; }

    [ObservableProperty]
    public partial string To { get; set; }

    [ObservableProperty]
    public partial string Cc { get; set; }

    [ObservableProperty]
    public partial string Bcc { get; set; }

    [ObservableProperty]
    public partial string Subject { get; set; }

    [ObservableProperty]
    public partial bool ShowCcBcc { get; set; }

    [ObservableProperty]
    public partial bool IsBusy { get; set; }

    [ObservableProperty]
    public partial string ErrorMessage { get; set; }

    [ObservableProperty]
    public partial bool IsErrorOpen { get; set; }

    [ObservableProperty]
    public partial string WindowTitle { get; set; }

    [ObservableProperty]
    public partial bool HasAttachments { get; set; }

    partial void OnSubjectChanged(string value) =>
        WindowTitle = string.IsNullOrWhiteSpace(value) ? Loc.Get("Compose_NewTitle") : value;

    public Account? From => FromIndex >= 0 && FromIndex < Accounts.Count ? Accounts[FromIndex] : null;

    public async Task PrepareAsync()
    {
        if (_request.Mode == ComposeModes.New)
        {
            if (_request.Prefill is { } prefill)
            {
                To = Join(prefill.To);
                Cc = Join(prefill.Cc);
                Bcc = Join(prefill.Bcc);
                Subject = prefill.Subject;
                InitialHtml = TextToHtml(prefill.Body);
                ShowCcBcc = Cc.Length > 0 || Bcc.Length > 0;
            }
        }
        else if (_request.MessageId is long messageId)
        {
            var prepared = await _client.PrepareComposeAsync(_request.Mode, messageId, Loc.LanguageCode);
            var index = Accounts.ToList().FindIndex(a => a.Id == prepared.AccountId);
            if (index >= 0) FromIndex = index;
            To = Join(prepared.To);
            Cc = Join(prepared.Cc);
            ShowCcBcc = Cc.Length > 0;
            Subject = prepared.Subject ?? "";
            _inReplyTo = prepared.InReplyTo;
            _references = prepared.References;
            // Two empty paragraphs above the quote: the caret starts in the first one.
            InitialHtml = "<p><br></p><p><br></p>" + (prepared.Html ?? TextToHtml(prepared.Text ?? ""));
            foreach (var a in prepared.Attachments ?? [])
                Attachments.Add(new ComposeAttachmentItem { Forwarded = a, Name = a.Filename, Size = a.Size });
            HasAttachments = Attachments.Count > 0;
        }
        _initialTo = To;
        _initialCc = Cc;
        _initialBcc = Bcc;
        _initialSubject = Subject;
        _initialAttachments = Attachments.Count;
    }

    public bool IsDirty(bool editorChanged) =>
        editorChanged || To != _initialTo || Cc != _initialCc || Bcc != _initialBcc || Subject != _initialSubject ||
        Attachments.Count != _initialAttachments;

    public void AddFiles(IEnumerable<string> paths)
    {
        foreach (var path in paths)
        {
            if (string.IsNullOrEmpty(path) || Attachments.Any(a => string.Equals(a.Path, path, StringComparison.OrdinalIgnoreCase))) continue;
            long size = 0;
            try { size = new FileInfo(path).Length; } catch (IOException) { }
            Attachments.Add(new ComposeAttachmentItem { Path = path, Name = Path.GetFileName(path), Size = size });
        }
        HasAttachments = Attachments.Count > 0;
        var total = Attachments.Sum(a => a.Size);
        if (total > 25L * 1024 * 1024) ShowError(Loc.Get("Compose_WarningLarge"));
    }

    public void RemoveAttachment(ComposeAttachmentItem item)
    {
        Attachments.Remove(item);
        HasAttachments = Attachments.Count > 0;
    }

    public void SetSuggestions(IReadOnlyList<Addr> contacts)
    {
        Suggestions.Clear();
        foreach (var c in contacts) Suggestions.Add(c);
    }

    public void ShowError(string message)
    {
        ErrorMessage = message;
        IsErrorOpen = true;
    }

    /// <summary>Validates the form and builds compose.send (html = editor content wrapped in a minimal document).</summary>
    public ComposeSendParams BuildSendRequest(string bodyHtml, string text)
    {
        if (From is not { } from) throw new ComposeValidationException(Loc.Get("Compose_ErrorNoAccount"));
        var to = ParseRecipients(To);
        var cc = ParseRecipients(Cc);
        var bcc = ParseRecipients(Bcc);
        if (to.Count + cc.Count + bcc.Count == 0) throw new ComposeValidationException(Loc.Get("Compose_ErrorNoRecipients"));

        var forwarded = Attachments.Where(a => a.Forwarded is not null)
            .Select(a => new ForwardAttachmentRef { MessageId = a.Forwarded!.MessageId, PartId = a.Forwarded.PartId })
            .ToArray();
        var html = "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>" +
                   "<div style=\"font-family:'Segoe UI',Arial,sans-serif;font-size:11pt;color:#1b1b1b\">" + bodyHtml + "</div></body></html>";
        return new ComposeSendParams
        {
            AccountId = from.Id,
            To = [.. to],
            Cc = [.. cc],
            Bcc = [.. bcc],
            Subject = Subject.Trim(),
            Html = html,
            Text = text,
            Attachments = [.. Attachments.Where(a => a.Path is not null).Select(a => a.Path!)],
            ForwardAttachments = forwarded.Length > 0 ? forwarded : null,
            InReplyTo = _inReplyTo,
            References = _references,
            ReplyToMessageId = _request.Mode is ComposeModes.Reply or ComposeModes.ReplyAll ? _request.MessageId : null,
        };
    }

    /// <summary>Splits "a@b.it, Nome &lt;c@d.it&gt;; \"Rossi, Mario\" &lt;e@f.it&gt;" into addresses.</summary>
    public static List<Addr> ParseRecipients(string text)
    {
        var result = new List<Addr>();
        foreach (var token in SplitRecipients(text))
        {
            var addr = Addr.Parse(token);
            if (addr.Address.Length == 0) continue;
            if (!IsValidAddress(addr.Address))
                throw new ComposeValidationException(Loc.Format("Compose_ErrorInvalidAddress", token.Trim()));
            result.Add(addr);
        }
        return result;
    }

    public static IEnumerable<string> SplitRecipients(string text)
    {
        var current = new StringBuilder();
        bool quoted = false, angle = false;
        foreach (var c in text)
        {
            if (c == '"') quoted = !quoted;
            else if (c == '<' && !quoted) angle = true;
            else if (c == '>' && !quoted) angle = false;
            if ((c == ',' || c == ';') && !quoted && !angle)
            {
                if (current.ToString().Trim().Length > 0) yield return current.ToString().Trim();
                current.Clear();
                continue;
            }
            current.Append(c);
        }
        if (current.ToString().Trim().Length > 0) yield return current.ToString().Trim();
    }

    private static bool IsValidAddress(string address)
    {
        var at = address.IndexOf('@');
        return at > 0 && at == address.LastIndexOf('@') && at < address.Length - 1 &&
               !address.Any(char.IsWhiteSpace) && !address.Contains("..");
    }

    private static string Join(IEnumerable<Addr>? list) =>
        list is null ? "" : string.Join(", ", list.Select(a => a.ToString()));

    private static string TextToHtml(string text)
    {
        if (string.IsNullOrEmpty(text)) return "";
        var sb = new StringBuilder();
        foreach (var paragraph in text.Replace("\r\n", "\n").Split("\n\n"))
            sb.Append("<p>").Append(WebUtility.HtmlEncode(paragraph).Replace("\n", "<br>")).Append("</p>");
        return sb.ToString();
    }
}

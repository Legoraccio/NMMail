using System.Net;
using System.Text;
using System.Text.RegularExpressions;
using NMMail.Models;

namespace NMMail.Helpers;

/// <summary>Builds the document shown in the reader's WebView2.</summary>
public static partial class MessageHtml
{
    // Defaults for unstyled mails, placed first in <head> so the mail's own CSS still wins.
    private const string Defaults =
        "<meta name=\"color-scheme\" content=\"light\">" +
        "<style>:root{color-scheme:light}html{background:#fff}" +
        "body{font-family:'Segoe UI',Arial,sans-serif;font-size:14px;line-height:1.5;color:#1b1b1b;margin:0;padding:16px 20px;overflow-wrap:break-word}</style>";

    public static string Prepare(MessageDetail detail) =>
        detail.Html is { Length: > 0 } html ? InjectDefaults(html) : FromText(detail.Text ?? "");

    private static string InjectDefaults(string html)
    {
        var head = html.IndexOf("<head", StringComparison.OrdinalIgnoreCase);
        if (head >= 0)
        {
            var close = html.IndexOf('>', head);
            if (close > 0) return html.Insert(close + 1, Defaults);
        }
        var htmlTag = html.IndexOf("<html", StringComparison.OrdinalIgnoreCase);
        if (htmlTag >= 0)
        {
            var close = html.IndexOf('>', htmlTag);
            if (close > 0) return html.Insert(close + 1, "<head>" + Defaults + "</head>");
        }
        return Defaults + html;
    }

    /// <summary>Plain-text mail: escaped, wrapped, with clickable http(s) links.</summary>
    public static string FromText(string text)
    {
        var encoded = WebUtility.HtmlEncode(text.Replace("\r\n", "\n"));
        var linked = UrlRegex().Replace(encoded, m =>
        {
            var url = m.Value;
            var trail = "";
            while (url.Length > 0 && ".,;:!?)]}".Contains(url[^1]))
            {
                trail = url[^1] + trail;
                url = url[..^1];
            }
            return $"<a href=\"{url}\">{url}</a>{trail}";
        });
        var sb = new StringBuilder(linked.Length + 512);
        sb.Append("<!DOCTYPE html><html><head><meta charset=\"utf-8\">");
        sb.Append("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">");
        sb.Append(Defaults);
        sb.Append("<style>.nm-text{white-space:pre-wrap;font-family:'Segoe UI',Arial,sans-serif}a{color:#0f6cbd}</style>");
        sb.Append("</head><body><div class=\"nm-text\">").Append(linked).Append("</div></body></html>");
        return sb.ToString();
    }

    [GeneratedRegex(@"https?://[^\s<>""']+", RegexOptions.IgnoreCase | RegexOptions.CultureInvariant)]
    private static partial Regex UrlRegex();
}

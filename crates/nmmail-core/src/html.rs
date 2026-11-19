//! Safe HTML rendering for the reading pane.
//!
//! Defense in depth: the HTML is sanitized here (no scripts, handlers, forms,
//! frames), remote resources are stripped unless allowed, `cid:` images are
//! inlined as `data:` URLs, and the result is wrapped in a document carrying a
//! strict Content-Security-Policy. The UI additionally disables scripts and
//! blocks network requests in its web view.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;

/// Result of rendering a message body.
pub struct Rendered {
    pub document: String,
    pub has_remote_content: bool,
}

const TAGS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "b",
    "bdi",
    "bdo",
    "big",
    "blockquote",
    "br",
    "caption",
    "center",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "details",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "font",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "main",
    "mark",
    "nav",
    "ol",
    "p",
    "picture",
    "pre",
    "q",
    "s",
    "samp",
    "section",
    "small",
    "span",
    "strike",
    "strong",
    "style",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "tt",
    "u",
    "ul",
    "var",
    "wbr",
    "article",
    "aside",
    "label",
    "map",
    "area",
    "source",
];

const GENERIC_ATTRS: &[&str] = &[
    "style",
    "class",
    "id",
    "dir",
    "lang",
    "title",
    "align",
    "valign",
    "width",
    "height",
    "bgcolor",
    "background",
    "border",
    "color",
    "nowrap",
    "role",
    "aria-hidden",
    "aria-label",
];

fn is_remote(url: &str) -> bool {
    let u = url.trim_start().to_ascii_lowercase();
    u.starts_with("http:") || u.starts_with("https:") || u.starts_with("//")
}

/// Sanitizes an HTML fragment. When `allow_remote` is false, remote images,
/// backgrounds and CSS `url()` references are removed; `remote_seen` reports
/// whether anything remote was present.
pub fn sanitize(html: &str, cids: &HashMap<String, String>, allow_remote: bool) -> (String, bool) {
    let remote_seen = Arc::new(AtomicBool::new(false));
    let flag = remote_seen.clone();
    let cids = cids.clone();

    let mut b = ammonia::Builder::default();
    b.rm_clean_content_tags(["style"])
        .tags(TAGS.iter().copied().collect())
        .add_clean_content_tags([
            "script", "noscript", "title", "head", "object", "iframe", "template", "svg", "math",
            "form", "select", "textarea", "button",
        ])
        .generic_attributes(GENERIC_ATTRS.iter().copied().collect())
        .add_tag_attributes("a", ["href", "name", "target"])
        .add_tag_attributes(
            "img",
            ["src", "alt", "srcset", "usemap", "hspace", "vspace"],
        )
        .add_tag_attributes("source", ["srcset", "media", "type"])
        .add_tag_attributes("area", ["href", "shape", "coords", "alt"])
        .add_tag_attributes("map", ["name"])
        .add_tag_attributes("font", ["face", "size"])
        .add_tag_attributes(
            "table",
            ["cellpadding", "cellspacing", "summary", "frame", "rules"],
        )
        .add_tag_attributes(
            "td",
            ["colspan", "rowspan", "headers", "scope", "abbr", "axis"],
        )
        .add_tag_attributes(
            "th",
            ["colspan", "rowspan", "headers", "scope", "abbr", "axis"],
        )
        .add_tag_attributes("col", ["span"])
        .add_tag_attributes("colgroup", ["span"])
        .add_tag_attributes("ol", ["start", "type", "reversed"])
        .add_tag_attributes("ul", ["type"])
        .add_tag_attributes("li", ["value", "type"])
        .add_tag_attributes("time", ["datetime"])
        .add_tag_attributes("style", ["type", "media"])
        .url_schemes(
            ["http", "https", "mailto", "tel", "data", "cid"]
                .into_iter()
                .collect(),
        )
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true)
        .attribute_filter(move |element, attribute, value| {
            match (element, attribute) {
                (_, "src") | ("img", "srcset") | ("source", "srcset") | (_, "background") => {
                    let v = value.trim();
                    if let Some(cid) = v.strip_prefix("cid:").or_else(|| v.strip_prefix("CID:")) {
                        let key = cid.trim_matches(['<', '>']);
                        return cids.get(key).map(|d| Cow::Owned(d.clone()));
                    }
                    if v.to_ascii_lowercase().starts_with("data:") {
                        // Only raster images are allowed as data URLs.
                        let lower = v.to_ascii_lowercase();
                        return if lower.starts_with("data:image/")
                            && !lower.starts_with("data:image/svg")
                        {
                            Some(Cow::Borrowed(value))
                        } else {
                            None
                        };
                    }
                    if is_remote(v) || v.contains("http") {
                        flag.store(true, Ordering::Relaxed);
                        return if allow_remote {
                            Some(Cow::Borrowed(value))
                        } else {
                            None
                        };
                    }
                    None
                }
                (_, "style") => {
                    let lower = value.to_ascii_lowercase();
                    if lower.contains("url(") {
                        if lower.contains("http") || lower.contains("//") {
                            flag.store(true, Ordering::Relaxed);
                        }
                        if !allow_remote {
                            return Some(Cow::Owned(strip_css_urls(value)));
                        }
                    }
                    // `expression()` and `behavior` are ancient IE script vectors.
                    if lower.contains("expression(") || lower.contains("behavior:") {
                        return None;
                    }
                    Some(Cow::Borrowed(value))
                }
                _ => Some(Cow::Borrowed(value)),
            }
        });
    let cleaned = b.clean(html).to_string();

    // <style> element bodies are not visible to the attribute filter.
    let lower = cleaned.to_ascii_lowercase();
    let mut has_remote = remote_seen.load(Ordering::Relaxed);
    if lower.contains("<style") && lower.contains("url(") && lower.contains("http") {
        has_remote = true;
    }
    if lower.contains("@import") {
        has_remote = true;
    }
    (cleaned, has_remote)
}

/// Replaces every CSS `url(...)` with `none`.
fn strip_css_urls(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let lower = css.to_ascii_lowercase();
    let mut i = 0;
    while let Some(pos) = lower[i..].find("url(") {
        let start = i + pos;
        out.push_str(&css[i..start]);
        match lower[start..].find(')') {
            Some(end) => {
                out.push_str("none");
                i = start + end + 1;
            }
            None => {
                i = css.len();
            }
        }
    }
    if i < css.len() {
        out.push_str(&css[i..]);
    }
    out
}

/// Escapes plain text and turns it into HTML preserving line breaks and links.
pub fn text_to_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8 + 64);
    out.push_str("<div class=\"nm-plain\">");
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push_str("<br>");
        }
        linkify_line(line, &mut out);
    }
    out.push_str("</div>");
    out
}

fn linkify_line(line: &str, out: &mut String) {
    let mut rest = line;
    loop {
        let found = ["https://", "http://", "www."]
            .iter()
            .filter_map(|p| rest.find(p).map(|i| (i, *p)))
            .min_by_key(|(i, _)| *i);
        let Some((idx, prefix)) = found else {
            out.push_str(&html_escape::encode_text(rest));
            return;
        };
        out.push_str(&html_escape::encode_text(&rest[..idx]));
        let tail = &rest[idx..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
            .unwrap_or(tail.len());
        let mut url = &tail[..end];
        while url.ends_with(['.', ',', ';', ':', ')', '!', '?']) {
            url = &url[..url.len() - 1];
        }
        if url.len() <= prefix.len() {
            out.push_str(&html_escape::encode_text(&tail[..prefix.len()]));
            rest = &tail[prefix.len()..];
            continue;
        }
        let href = if prefix == "www." {
            format!("https://{url}")
        } else {
            url.to_string()
        };
        out.push_str("<a href=\"");
        out.push_str(&html_escape::encode_double_quoted_attribute(&href));
        out.push_str("\">");
        out.push_str(&html_escape::encode_text(url));
        out.push_str("</a>");
        rest = &tail[url.len()..];
    }
}

const BASE_CSS: &str = "html,body{margin:0;padding:0;background:#fff}\
body{padding:16px 20px;font-family:'Segoe UI Variable Text','Segoe UI',system-ui,-apple-system,sans-serif;\
font-size:14px;line-height:1.5;color:#1b1b1b;word-wrap:break-word;overflow-wrap:anywhere}\
img{max-width:100%;height:auto}table{max-width:100%}pre{white-space:pre-wrap}\
blockquote{margin:0 0 0 .8ex;border-left:3px solid #c8c8c8;padding-left:1.2ex;color:#505050}\
.nm-plain{white-space:normal;font-family:inherit}a{color:#0b5cad}";

/// Wraps a sanitized fragment into a full document with a strict CSP.
pub fn document(fragment: &str, allow_remote: bool) -> String {
    let remote = if allow_remote { " https: http:" } else { "" };
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:{remote}; \
style-src 'unsafe-inline'{remote}; font-src data:{remote}; media-src 'none'; frame-src 'none'; \
form-action 'none'; base-uri 'none'\">\
<meta name=\"color-scheme\" content=\"light\"><meta name=\"referrer\" content=\"no-referrer\">\
<style>{BASE_CSS}</style></head><body>{fragment}</body></html>"
    )
}

/// Renders the body of a parsed message for display.
pub fn render_message(parsed: &crate::mime::Parsed, raw: &[u8], allow_remote: bool) -> Rendered {
    let mut cids = HashMap::new();
    if parsed
        .html
        .as_deref()
        .is_some_and(|h| h.to_ascii_lowercase().contains("cid:"))
    {
        for (cid, mime, bytes) in crate::mime::inline_parts(raw) {
            // Keep the document reasonably small.
            if bytes.len() <= 4 * 1024 * 1024 {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                cids.insert(cid, format!("data:{mime};base64,{b64}"));
            }
        }
    }
    let (fragment, has_remote) = match (&parsed.html, &parsed.text) {
        (Some(h), _) => sanitize(h, &cids, allow_remote),
        (None, Some(t)) => (text_to_html(t), false),
        (None, None) => (String::new(), false),
    };
    Rendered {
        document: document(&fragment, allow_remote),
        has_remote_content: has_remote,
    }
}

/// Sanitized HTML fragment suitable for quoting in a reply/forward.
pub fn quotable_fragment(parsed: &crate::mime::Parsed) -> String {
    match (&parsed.html, &parsed.text) {
        (Some(h), _) => sanitize(h, &HashMap::new(), false).0,
        (None, Some(t)) => text_to_html(t),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_scripts_and_handlers() {
        let (h, _) = sanitize(
            "<p onclick=\"x()\">a</p><script>alert(1)</script><iframe src=\"https://x\"></iframe><form><input></form>",
            &HashMap::new(),
            false,
        );
        assert_eq!(h, "<p>a</p>");
    }

    #[test]
    fn blocks_remote_images_but_keeps_style() {
        let (h, remote) = sanitize(
            "<style>.a{color:red}</style><img src=\"https://t.example/p.gif\" class=\"a\"><div style=\"background:url(https://x/y.png);color:blue\">x</div>",
            &HashMap::new(),
            false,
        );
        assert!(remote);
        assert!(!h.contains("https://t.example"));
        assert!(h.contains(".a{color:red}"));
        assert!(h.contains("background:none;color:blue"));

        let (h, remote) = sanitize(
            "<img src=\"https://t.example/p.gif\">",
            &HashMap::new(),
            true,
        );
        assert!(remote);
        assert!(h.contains("https://t.example/p.gif"));
    }

    #[test]
    fn inlines_cid_images() {
        let mut cids = HashMap::new();
        cids.insert("img1".to_string(), "data:image/png;base64,AAAA".to_string());
        let (h, remote) = sanitize("<img src=\"cid:img1\">", &cids, false);
        assert!(!remote);
        assert!(h.contains("data:image/png;base64,AAAA"));
    }

    #[test]
    fn links_get_rel() {
        let (h, _) = sanitize(
            "<a href=\"https://example.com\">x</a><a href=\"javascript:alert(1)\">y</a>",
            &HashMap::new(),
            false,
        );
        assert!(h.contains("rel=\"noopener noreferrer\""));
        assert!(!h.contains("javascript"));
    }

    #[test]
    fn plain_text_linkify() {
        let h = text_to_html("Vedi https://example.com/a?b=1&c=2.\n<b>no</b> www.test.it");
        assert!(h.contains("<a href=\"https://example.com/a?b=1&amp;c=2\">"));
        assert!(h.contains("&lt;b&gt;no&lt;/b&gt;"));
        assert!(h.contains("href=\"https://www.test.it\""));
    }

    #[test]
    fn document_has_csp() {
        let d = document("<p>x</p>", false);
        assert!(d.contains("default-src 'none'"));
        assert!(!d.contains("https:"));
    }
}

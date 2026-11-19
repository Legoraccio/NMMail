//! MIME parsing helpers built on `mail-parser`.

use mail_parser::{AddressList, MessageParser, MessagePart, MessageRef, PartKind};

use crate::model::{Addr, Attachment};

/// Header-level information about a message.
#[derive(Debug, Clone, Default)]
pub struct HeaderInfo {
    pub subject: String,
    pub from: Option<Addr>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub reply_to: Vec<Addr>,
    pub date: Option<i64>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    /// Heuristic from headers only (multipart/mixed or X-MS-Has-Attach).
    pub has_attachments_hint: bool,
    pub category: Option<String>,
}

/// A fully parsed message, ready for display.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub headers: HeaderInfo,
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub snippet: String,
}

fn parser() -> MessageParser {
    MessageParser::default()
}

fn addrs(list: Option<AddressList<'_>>) -> Vec<Addr> {
    list.map(|l| {
        l.mailboxes()
            .filter_map(|m| m.address().map(|a| Addr::new(m.name(), a)))
            .collect()
    })
    .unwrap_or_default()
}

fn header_info(m: MessageRef<'_>) -> HeaderInfo {
    let ct = m.content_type();
    let mixed = ct.as_ref().is_some_and(|c| {
        c.ctype().eq_ignore_ascii_case("multipart")
            && c.subtype().is_some_and(|s| s.eq_ignore_ascii_case("mixed"))
    });
    let headers = m.headers();
    let ms_attach = headers
        .get("X-MS-Has-Attach")
        .and_then(|h| {
            h.value()
                .as_text()
                .map(|t| t.trim().eq_ignore_ascii_case("yes"))
        })
        .unwrap_or(false);
    let category = headers
        .get("X-Test-Category")
        .and_then(|h| h.value().as_text().map(|t| t.trim().to_ascii_lowercase()));
    HeaderInfo {
        subject: m.subject().unwrap_or_default().trim().to_string(),
        from: addrs(m.from())
            .into_iter()
            .next()
            .or_else(|| addrs(m.sender()).into_iter().next()),
        to: addrs(m.to()),
        cc: addrs(m.cc()),
        reply_to: addrs(m.reply_to()),
        date: m
            .date()
            .filter(|d| d.is_valid())
            .map(|d| d.to_timestamp() * 1000),
        message_id: m.message_id().map(|s| s.to_string()),
        in_reply_to: m
            .in_reply_to()
            .and_then(|l| l.first().map(|s| s.to_string())),
        references: m
            .references()
            .map(|l| {
                l.iter()
                    .map(|s| format!("<{s}>"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|s| !s.is_empty()),
        has_attachments_hint: mixed || ms_attach,
        category,
    }
}

/// Parses a header block (as returned by `BODY.PEEK[HEADER.FIELDS (...)]`).
pub fn parse_headers(raw: &[u8]) -> HeaderInfo {
    // mail-parser needs the blank line terminating the header block.
    let mut buf = raw.to_vec();
    if !buf.ends_with(b"\r\n\r\n") && !buf.ends_with(b"\n\n") {
        buf.extend_from_slice(b"\r\n");
    }
    match parser().parse(&buf) {
        Some(m) => header_info(m.root()),
        None => HeaderInfo::default(),
    }
}

fn is_attachment_part(p: &MessagePart<'_>) -> bool {
    !matches!(p.kind(), PartKind::Multipart)
}

fn part_filename(p: &MessagePart<'_>, idx: usize) -> String {
    if let Some(n) = p.attachment_name().filter(|n| !n.trim().is_empty()) {
        return sanitize_filename(n);
    }
    if let PartKind::Message(nested) = p.kind()
        && let Some(s) = nested.subject()
    {
        return sanitize_filename(&format!("{s}.eml"));
    }
    let ext = p
        .content_type()
        .and_then(|c| {
            let mime = format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream"));
            mime_guess::get_mime_extensions_str(&mime).and_then(|e| e.first().copied())
        })
        .unwrap_or("bin");
    format!("allegato-{}.{}", idx + 1, ext)
}

fn part_mime(p: &MessagePart<'_>) -> String {
    p.content_type()
        .map(|c| {
            format!(
                "{}/{}",
                c.ctype().to_ascii_lowercase(),
                c.subtype().unwrap_or("octet-stream").to_ascii_lowercase()
            )
        })
        .unwrap_or_else(|| "application/octet-stream".into())
}

pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "allegato".into()
    } else {
        cleaned.chars().take(180).collect()
    }
}

/// Parses a complete RFC 5322 message.
pub fn parse_full(raw: &[u8]) -> Parsed {
    let Some(msg) = parser().parse(raw) else {
        return Parsed {
            text: Some(String::from_utf8_lossy(raw).into_owned()),
            ..Default::default()
        };
    };
    let root = msg.root();
    let headers = header_info(root);

    let html_parts: Vec<_> = root.html_body().collect();
    let text_parts: Vec<_> = root.text_body().collect();
    let html = if html_parts
        .iter()
        .any(|p| matches!(p.kind(), PartKind::Html))
    {
        let mut s = String::new();
        for p in &html_parts {
            match p.kind() {
                PartKind::Html => {
                    if let Some(t) = p.text() {
                        s.push_str(&t);
                    }
                }
                PartKind::Text => {
                    if let Some(t) = p.text() {
                        s.push_str(&crate::html::text_to_html(&t));
                    }
                }
                _ => {}
            }
        }
        Some(s)
    } else {
        None
    };
    let text = {
        let mut s = String::new();
        for p in &text_parts {
            if matches!(p.kind(), PartKind::Text)
                && let Some(t) = p.text()
            {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(&t);
            }
        }
        if s.is_empty() {
            html.as_deref().map(mail_parser::html_to_text)
        } else {
            Some(s)
        }
    };

    let mut attachments = Vec::new();
    for (idx, p) in root.attachments().enumerate() {
        if !is_attachment_part(&p) {
            continue;
        }
        let inline = p.is_inline() && p.content_id().is_some();
        attachments.push(Attachment {
            part_id: p.id(),
            filename: part_filename(&p, idx),
            mime: part_mime(&p),
            size: p.decoded_len() as u64,
            inline,
        });
    }

    let snippet = make_snippet(text.as_deref().unwrap_or(""));
    Parsed {
        headers,
        html,
        text,
        attachments,
        snippet,
    }
}

/// Inline parts addressable through `cid:` URLs: (content-id, mime, bytes).
pub fn inline_parts(raw: &[u8]) -> Vec<(String, String, Vec<u8>)> {
    let Some(msg) = parser().parse(raw) else {
        return Vec::new();
    };
    msg.parts()
        .filter_map(|p| {
            let cid = p.content_id()?;
            let mime = part_mime(&p);
            if !mime.starts_with("image/") {
                return None;
            }
            Some((
                cid.trim_matches(['<', '>']).to_string(),
                mime,
                p.decoded().into_owned(),
            ))
        })
        .collect()
}

/// Extracts one attachment by part id: (filename, mime, bytes).
pub fn attachment(raw: &[u8], part_id: u32) -> Option<(String, String, Vec<u8>)> {
    let msg = parser().parse(raw)?;
    let root = msg.root();
    let (idx, p) = root
        .attachments()
        .enumerate()
        .find(|(_, p)| p.id() == part_id)?;
    let bytes = match p.kind() {
        PartKind::Message(nested) => nested.raw().to_vec(),
        _ => p.decoded().into_owned(),
    };
    Some((part_filename(&p, idx), part_mime(&p), bytes))
}

/// Single-line preview text (max ~200 chars).
pub fn make_snippet(text: &str) -> String {
    let mut out = String::with_capacity(220);
    let mut last_space = true;
    for line in text.lines() {
        let l = line.trim();
        // Skip quoted replies and signature separators in the preview.
        if l.starts_with('>') || l == "--" {
            continue;
        }
        for c in l.chars() {
            let c = if c.is_whitespace() || c.is_control() {
                ' '
            } else {
                c
            };
            if c == ' ' {
                if last_space {
                    continue;
                }
                last_space = true;
            } else {
                last_space = false;
            }
            out.push(c);
            if out.chars().count() >= 200 {
                return out.trim().to_string();
            }
        }
        if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MSG: &[u8] = b"From: =?UTF-8?Q?Jos=C3=A9?= <jose@example.com>\r\n\
To: me@example.com, \"Anna\" <anna@example.com>\r\n\
Subject: Prova allegato\r\n\
Date: Tue, 10 Nov 2026 10:00:00 +0100\r\n\
Message-ID: <abc@example.com>\r\n\
References: <r1@example.com> <r2@example.com>\r\n\
In-Reply-To: <r2@example.com>\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
\r\n\
--b1\r\n\
Content-Type: multipart/alternative; boundary=\"b2\"\r\n\
\r\n\
--b2\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Ciao,\r\nin allegato il file.\r\n> citazione\r\n\
--b2\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Ciao,<br>in allegato <b>il file</b>.</p><img src=\"cid:img1\">\r\n\
--b2--\r\n\
--b1\r\n\
Content-Type: image/png\r\n\
Content-ID: <img1>\r\n\
Content-Disposition: inline\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--b1\r\n\
Content-Type: application/pdf; name=\"fattura.pdf\"\r\n\
Content-Disposition: attachment; filename=\"fattura.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--b1--\r\n";

    #[test]
    fn parses_full_message() {
        let p = parse_full(MSG);
        assert_eq!(p.headers.subject, "Prova allegato");
        assert_eq!(
            p.headers.from.as_ref().unwrap().name.as_deref(),
            Some("José")
        );
        assert_eq!(p.headers.to.len(), 2);
        assert_eq!(p.headers.message_id.as_deref(), Some("abc@example.com"));
        assert_eq!(p.headers.in_reply_to.as_deref(), Some("r2@example.com"));
        assert!(
            p.headers
                .references
                .as_deref()
                .unwrap()
                .contains("<r1@example.com>")
        );
        assert!(p.headers.has_attachments_hint);
        assert!(p.html.as_deref().unwrap().contains("<b>il file</b>"));
        assert!(p.text.as_deref().unwrap().contains("in allegato il file"));
        assert_eq!(p.snippet, "Ciao, in allegato il file.");
        let pdf = p
            .attachments
            .iter()
            .find(|a| a.filename == "fattura.pdf")
            .unwrap();
        assert_eq!(pdf.mime, "application/pdf");
        assert!(!pdf.inline);
        let (name, _, bytes) = attachment(MSG, pdf.part_id).unwrap();
        assert_eq!(name, "fattura.pdf");
        assert!(bytes.starts_with(b"%PDF"));
        let inl = inline_parts(MSG);
        assert_eq!(inl[0].0, "img1");
    }

    #[test]
    fn parses_header_block() {
        let h = parse_headers(b"Subject: =?utf-8?B?Q2lhbyDwn5GL?=\r\nFrom: a@b.c\r\n");
        assert_eq!(h.subject, "Ciao 👋");
        assert_eq!(h.from.unwrap().address, "a@b.c");
    }

    #[test]
    fn filename_sanitizing() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_filename("  "), "allegato");
    }
}

//! Minimal MIME support: header blocks, content types, the part tree used for
//! BODYSTRUCTURE and section fetches, address lists for ENVELOPE and the decoded text
//! used by SEARCH. Everything works on byte offsets into the raw message and never
//! panics on malformed input.

use std::ops::Range;

/// Maximum MIME nesting depth that is parsed (deeper parts are treated as leaves).
const MAX_DEPTH: usize = 16;

/// One header field of a header block.
#[derive(Debug, Clone)]
pub(crate) struct HeaderField {
    /// Field name as written (without the colon).
    pub name: String,
    /// Unfolded and trimmed value (lossy UTF-8).
    pub value: String,
    /// Byte range of the whole field (all its lines, line breaks included).
    pub raw: Range<usize>,
}

/// Index just past the next `\n` at or after `pos` (or `end`).
fn next_line(raw: &[u8], pos: usize, end: usize) -> usize {
    match raw[pos..end].iter().position(|&b| b == b'\n') {
        Some(i) => pos + i + 1,
        None => end,
    }
}

/// Strips a trailing LF or CRLF.
pub(crate) fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Parses the header block starting at `start`. Returns the fields and the offset where
/// the body starts (just after the blank line, or `end` if there is none).
pub(crate) fn parse_header_block(
    raw: &[u8],
    start: usize,
    end: usize,
) -> (Vec<HeaderField>, usize) {
    let end = end.min(raw.len());
    let mut pos = start.min(end);
    let mut fields: Vec<HeaderField> = Vec::new();
    let mut body_start = end;
    while pos < end {
        let next = next_line(raw, pos, end);
        let content = trim_eol(&raw[pos..next]);
        match content.first() {
            None => {
                body_start = next;
                break;
            }
            Some(b' ' | b'\t') => {
                if let Some(last) = fields.last_mut() {
                    last.value.push_str(&String::from_utf8_lossy(content));
                    last.raw.end = next;
                }
            }
            Some(_) => {
                if let Some(colon) = content.iter().position(|&b| b == b':') {
                    fields.push(HeaderField {
                        name: String::from_utf8_lossy(&content[..colon])
                            .trim()
                            .to_string(),
                        value: String::from_utf8_lossy(&content[colon + 1..]).into_owned(),
                        raw: pos..next,
                    });
                }
            }
        }
        pos = next;
    }
    for field in &mut fields {
        let trimmed = field.value.trim();
        if trimmed.len() != field.value.len() {
            field.value = trimmed.to_string();
        }
    }
    (fields, body_start)
}

/// First value of the named header (case-insensitive).
pub(crate) fn header_value<'a>(fields: &'a [HeaderField], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case(name))
        .map(|f| f.value.as_str())
}

/// A parsed `Content-Type` (type and subtype lowercase, parameter names lowercase).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentType {
    pub ty: String,
    pub subtype: String,
    pub params: Vec<(String, String)>,
}

impl ContentType {
    pub(crate) fn new(ty: &str, subtype: &str) -> Self {
        ContentType {
            ty: ty.to_string(),
            subtype: subtype.to_string(),
            params: Vec::new(),
        }
    }

    fn text_plain() -> Self {
        let mut ct = ContentType::new("text", "plain");
        ct.params
            .push(("charset".to_string(), "us-ascii".to_string()));
        ct
    }

    pub(crate) fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn is(&self, ty: &str, subtype: &str) -> bool {
        self.ty == ty && self.subtype == subtype
    }
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b > 0x20 && b < 0x7f && !b"()<>@,;:\\\"/[]?=".contains(&b))
}

/// Splits a structured header value at `;`, ignoring separators inside quotes.
fn split_params(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut escape = false;
    for c in value.chars() {
        if escape {
            current.push(c);
            escape = false;
            continue;
        }
        match c {
            '\\' if in_quote => {
                current.push(c);
                escape = true;
            }
            '"' => {
                in_quote = !in_quote;
                current.push(c);
            }
            ';' if !in_quote => out.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    out.push(current);
    out
}

fn unquote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut escape = false;
    for c in s.chars() {
        if escape {
            out.push(c);
            escape = false;
        } else if c == '\\' {
            escape = true;
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_param(segment: &str) -> Option<(String, String)> {
    let (name, value) = segment.split_once('=')?;
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let value = value.trim();
    let value = match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(inner) => unquote(inner),
        None => value.to_string(),
    };
    Some((name, value))
}

pub(crate) fn parse_content_type(value: &str) -> Option<ContentType> {
    let mut segments = split_params(value).into_iter();
    let main = segments.next()?;
    let (ty, subtype) = main.split_once('/')?;
    let ty = ty.trim().to_ascii_lowercase();
    let subtype = subtype.trim().to_ascii_lowercase();
    if !is_token(&ty) || !is_token(&subtype) {
        return None;
    }
    Some(ContentType {
        ty,
        subtype,
        params: segments.filter_map(|s| parse_param(&s)).collect(),
    })
}

/// A parsed `Content-Disposition`: lowercase type plus parameters.
pub(crate) type Disposition = (String, Vec<(String, String)>);

fn parse_disposition(value: &str) -> Option<Disposition> {
    let mut segments = split_params(value).into_iter();
    let ty = segments.next()?.trim().to_ascii_lowercase();
    if !is_token(&ty) {
        return None;
    }
    Some((ty, segments.filter_map(|s| parse_param(&s)).collect()))
}

/// One node of the MIME tree. The root node is the message itself.
#[derive(Debug, Clone)]
pub(crate) struct Part {
    /// The (MIME) header of this entity, terminating blank line included.
    pub header: Range<usize>,
    /// The body of this entity.
    pub body: Range<usize>,
    pub content_type: ContentType,
    pub encoding: Option<String>,
    pub id: Option<String>,
    pub description: Option<String>,
    pub disposition: Option<Disposition>,
    pub language: Option<String>,
    pub location: Option<String>,
    pub md5: Option<String>,
    pub kind: PartKind,
}

#[derive(Debug, Clone)]
pub(crate) enum PartKind {
    Leaf,
    Multipart(Vec<Part>),
    /// A `message/rfc822` part; the box holds the encapsulated message.
    Message(Box<Part>),
}

impl Part {
    /// Number of lines of the body (as reported in BODYSTRUCTURE).
    pub(crate) fn body_lines(&self, raw: &[u8]) -> usize {
        let body = raw.get(self.body.clone()).unwrap_or_default();
        let newlines = body.iter().filter(|&&b| b == b'\n').count();
        if body.is_empty() || body.ends_with(b"\n") {
            newlines
        } else {
            newlines + 1
        }
    }
}

/// Parses the MIME structure of a whole message.
pub(crate) fn parse_message(raw: &[u8]) -> Part {
    parse_entity(raw, 0, raw.len(), &ContentType::text_plain(), 0)
}

fn parse_entity(
    raw: &[u8],
    start: usize,
    end: usize,
    default_ct: &ContentType,
    depth: usize,
) -> Part {
    let (fields, body_start) = parse_header_block(raw, start, end);
    let content_type = header_value(&fields, "content-type")
        .and_then(parse_content_type)
        .unwrap_or_else(|| default_ct.clone());
    let text = |name: &str| header_value(&fields, name).map(str::to_string);
    let body = body_start..end.max(body_start);
    let kind = if depth >= MAX_DEPTH {
        PartKind::Leaf
    } else if content_type.ty == "multipart" {
        match content_type.param("boundary") {
            Some(boundary) if !boundary.is_empty() => {
                let child_default = if content_type.subtype == "digest" {
                    ContentType::new("message", "rfc822")
                } else {
                    ContentType::text_plain()
                };
                let parts: Vec<Part> = split_multipart(raw, body.clone(), boundary)
                    .into_iter()
                    .map(|r| parse_entity(raw, r.start, r.end, &child_default, depth + 1))
                    .collect();
                if parts.is_empty() {
                    PartKind::Leaf
                } else {
                    PartKind::Multipart(parts)
                }
            }
            _ => PartKind::Leaf,
        }
    } else if content_type.is("message", "rfc822") {
        PartKind::Message(Box::new(parse_entity(
            raw,
            body.start,
            body.end,
            &ContentType::text_plain(),
            depth + 1,
        )))
    } else {
        PartKind::Leaf
    };
    Part {
        header: start..body_start,
        body,
        encoding: text("content-transfer-encoding"),
        id: text("content-id"),
        description: text("content-description"),
        disposition: header_value(&fields, "content-disposition").and_then(parse_disposition),
        language: text("content-language"),
        location: text("content-location"),
        md5: text("content-md5"),
        content_type,
        kind,
    }
}

/// Byte ranges of the parts of a multipart body.
fn split_multipart(raw: &[u8], body: Range<usize>, boundary: &str) -> Vec<Range<usize>> {
    let delimiter = [b"--".as_slice(), boundary.as_bytes()].concat();
    let mut parts = Vec::new();
    let mut current: Option<usize> = None;
    let mut pos = body.start;
    while pos < body.end {
        let next = next_line(raw, pos, body.end);
        let line = trim_eol(&raw[pos..next]);
        if let Some(rest) = line.strip_prefix(delimiter.as_slice()) {
            let closing = rest.starts_with(b"--");
            if closing || rest.iter().all(|&b| b == b' ' || b == b'\t') {
                if let Some(part_start) = current {
                    // The line break before the delimiter belongs to the delimiter.
                    let mut part_end = pos;
                    if part_end > part_start && raw[part_end - 1] == b'\n' {
                        part_end -= 1;
                        if part_end > part_start && raw[part_end - 1] == b'\r' {
                            part_end -= 1;
                        }
                    }
                    parts.push(part_start..part_end);
                }
                if closing {
                    return parts;
                }
                current = Some(next);
            }
        }
        pos = next;
    }
    if let Some(part_start) = current {
        parts.push(part_start..body.end.max(part_start));
    }
    parts
}

/// Resolves a section part path (`1.2.3`) as defined by RFC 3501 section 6.4.5.
pub(crate) fn resolve_part<'a>(root: &'a Part, path: &[u32]) -> Option<&'a Part> {
    let mut current = root;
    for (i, &n) in path.iter().enumerate() {
        let index = usize::try_from(n).ok()?.checked_sub(1)?;
        current = match &current.kind {
            PartKind::Multipart(children) => children.get(index)?,
            PartKind::Message(inner) if i > 0 => match &inner.kind {
                PartKind::Multipart(children) => children.get(index)?,
                _ if index == 0 => inner,
                _ => return None,
            },
            _ if index == 0 => current,
            _ => return None,
        };
    }
    Some(current)
}

/// An entry of an address list, as needed for an IMAP ENVELOPE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AddrItem {
    Mailbox {
        name: Option<String>,
        mailbox: String,
        host: Option<String>,
    },
    GroupStart(String),
    GroupEnd,
}

/// Parses an RFC 5322 address list (display names are kept raw, encoded words included).
pub(crate) fn parse_address_list(value: &str) -> Vec<AddrItem> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut escape = false;
    let mut comment = 0usize;
    let mut angle = false;
    let mut in_group = false;
    for c in value.chars() {
        if escape {
            current.push(c);
            escape = false;
            continue;
        }
        if in_quote {
            current.push(c);
            match c {
                '\\' => escape = true,
                '"' => in_quote = false,
                _ => {}
            }
            continue;
        }
        if comment > 0 {
            current.push(c);
            match c {
                '\\' => escape = true,
                '(' => comment += 1,
                ')' => comment -= 1,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_quote = true;
                current.push(c);
            }
            '(' => {
                comment = 1;
                current.push(c);
            }
            '<' => {
                angle = true;
                current.push(c);
            }
            '>' => {
                angle = false;
                current.push(c);
            }
            ',' if !angle => {
                push_mailbox(&mut items, &current);
                current.clear();
            }
            ':' if !angle && !in_group && !current.contains('@') && !current.contains('<') => {
                items.push(AddrItem::GroupStart(clean_phrase(&current)));
                in_group = true;
                current.clear();
            }
            ';' if !angle && in_group => {
                push_mailbox(&mut items, &current);
                current.clear();
                items.push(AddrItem::GroupEnd);
                in_group = false;
            }
            _ => current.push(c),
        }
    }
    push_mailbox(&mut items, &current);
    if in_group {
        items.push(AddrItem::GroupEnd);
    }
    items
}

/// Byte index of the first `c` outside a quoted string.
fn find_unquoted(text: &str, wanted: char) -> Option<usize> {
    let mut in_quote = false;
    let mut escape = false;
    for (i, c) in text.char_indices() {
        if escape {
            escape = false;
        } else if in_quote && c == '\\' {
            escape = true;
        } else if c == '"' {
            in_quote = !in_quote;
        } else if c == wanted && !in_quote {
            return Some(i);
        }
    }
    None
}

/// Removes comments; returns the remaining text and the first comment's content.
fn strip_comments(text: &str) -> (String, Option<String>) {
    let mut out = String::new();
    let mut first_comment: Option<String> = None;
    let mut comment = String::new();
    let mut depth = 0usize;
    let mut in_quote = false;
    for c in text.chars() {
        if depth > 0 {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        if first_comment.is_none() {
                            first_comment = Some(comment.trim().to_string());
                        }
                        comment.clear();
                        continue;
                    }
                }
                _ => {}
            }
            comment.push(c);
            continue;
        }
        match c {
            '"' => {
                in_quote = !in_quote;
                out.push(c);
            }
            '(' if !in_quote => depth = 1,
            _ => out.push(c),
        }
    }
    (out, first_comment)
}

/// Trims a display name and removes the quoting.
fn clean_phrase(text: &str) -> String {
    let (text, _) = strip_comments(text);
    let mut out = String::new();
    let mut escape = false;
    let mut in_quote = false;
    for c in text.chars() {
        if escape {
            out.push(c);
            escape = false;
        } else if in_quote && c == '\\' {
            escape = true;
        } else if c == '"' {
            in_quote = !in_quote;
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_mailbox(items: &mut Vec<AddrItem>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let (name, addr) = match find_unquoted(text, '<') {
        Some(lt) => {
            let after = &text[lt + 1..];
            let addr = match after.find('>') {
                Some(gt) => &after[..gt],
                None => after,
            };
            let name = clean_phrase(&text[..lt]);
            ((!name.is_empty()).then_some(name), addr.trim().to_string())
        }
        None => {
            let (addr, comment) = strip_comments(text);
            (comment.filter(|c| !c.is_empty()), addr.trim().to_string())
        }
    };
    // Drop an obsolete source route ("@a,@b:user@host").
    let addr = match addr.rfind(':') {
        Some(i) if addr.starts_with('@') => addr[i + 1..].to_string(),
        _ => addr,
    };
    if addr.is_empty() && name.is_none() {
        return;
    }
    let (mailbox, host) = match addr.rfind('@') {
        Some(i) => (addr[..i].to_string(), Some(addr[i + 1..].to_string())),
        None => (addr, None),
    };
    let mailbox = match mailbox.strip_prefix('"').and_then(|m| m.strip_suffix('"')) {
        Some(inner) => unquote(inner),
        None => mailbox,
    };
    items.push(AddrItem::Mailbox {
        name,
        mailbox,
        host,
    });
}

/// Message-IDs (`<...>`) found in a header value, without the angle brackets.
pub(crate) fn message_ids(value: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        match after.find('>') {
            Some(end) => {
                let id = after[..end].trim();
                if !id.is_empty() {
                    ids.push(id.to_string());
                }
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    if ids.is_empty() {
        ids.extend(value.split_whitespace().map(str::to_string));
    }
    ids
}

/// Lowercase text used to evaluate SEARCH keys (decoded where possible, raw text
/// appended so both forms match).
#[derive(Debug, Default)]
pub(crate) struct SearchText {
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub body: String,
}

fn push_text(target: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(text);
}

fn push_addresses(target: &mut String, list: Option<mail_parser::AddressList<'_>>) {
    if let Some(list) = list {
        for mailbox in list.mailboxes() {
            if let Some(name) = mailbox.name() {
                push_text(target, name);
            }
            if let Some(address) = mailbox.address() {
                push_text(target, address);
            }
        }
    }
}

pub(crate) fn search_text(raw: &[u8], fields: &[HeaderField], root: &Part) -> SearchText {
    let raw_header = |name: &str| {
        fields
            .iter()
            .filter(|f| f.name.eq_ignore_ascii_case(name))
            .map(|f| f.value.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut text = SearchText {
        subject: raw_header("subject"),
        from: raw_header("from"),
        to: raw_header("to"),
        cc: raw_header("cc"),
        bcc: raw_header("bcc"),
        body: String::new(),
    };
    if let Some(message) = mail_parser::MessageParser::new().parse(raw) {
        if let Some(subject) = message.subject() {
            push_text(&mut text.subject, subject);
        }
        push_addresses(&mut text.from, message.from());
        push_addresses(&mut text.to, message.to());
        push_addresses(&mut text.cc, message.cc());
        push_addresses(&mut text.bcc, message.bcc());
        for body in message.text_bodies() {
            push_text(&mut text.body, &body);
        }
        for body in message.html_bodies() {
            push_text(&mut text.body, &body);
        }
    }
    if text.body.is_empty() {
        let body = raw.get(root.body.clone()).unwrap_or_default();
        text.body = String::from_utf8_lossy(body).into_owned();
    }
    for field in [
        &mut text.subject,
        &mut text.from,
        &mut text.to,
        &mut text.cc,
        &mut text.bcc,
        &mut text.body,
    ] {
        *field = field.to_lowercase();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTIPART: &[u8] = b"From: \"Rossi, Mario\" <mario@example.com>\r\n\
To: a@example.com, Group: b@example.com, c@example.com;\r\n\
Subject: Test\r\n\
Content-Type: multipart/mixed; boundary=\"XYZ\"\r\n\
\r\n\
preamble\r\n\
--XYZ\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hello\r\n\
--XYZ\r\n\
Content-Type: application/pdf; name=\"a.pdf\"\r\n\
Content-Disposition: attachment; filename=\"a.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0=\r\n\
--XYZ--\r\n\
epilogue\r\n";

    #[test]
    fn header_block_unfolds() {
        let raw = b"Subject: a\r\n b\r\nX: 1\r\n\r\nbody";
        let (fields, body) = parse_header_block(raw, 0, raw.len());
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].value, "a b");
        assert_eq!(&raw[body..], b"body");
        assert_eq!(&raw[fields[0].raw.clone()], b"Subject: a\r\n b\r\n");
    }

    #[test]
    fn multipart_tree() {
        let root = parse_message(MULTIPART);
        let PartKind::Multipart(parts) = &root.kind else {
            panic!("expected multipart");
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(&MULTIPART[parts[0].body.clone()], b"Hello");
        assert_eq!(&MULTIPART[parts[1].body.clone()], b"JVBERi0=");
        assert_eq!(parts[1].content_type.param("name"), Some("a.pdf"));
        assert_eq!(
            parts[1].disposition.as_ref().map(|d| d.0.as_str()),
            Some("attachment")
        );
        assert!(std::ptr::eq(resolve_part(&root, &[2]).unwrap(), &parts[1]));
        assert!(resolve_part(&root, &[3]).is_none());
    }

    #[test]
    fn addresses() {
        let items =
            parse_address_list("\"Rossi, Mario\" <mario@example.com>, x@y.z (Ics), Group: b@c.d;");
        assert_eq!(
            items,
            vec![
                AddrItem::Mailbox {
                    name: Some("Rossi, Mario".into()),
                    mailbox: "mario".into(),
                    host: Some("example.com".into())
                },
                AddrItem::Mailbox {
                    name: Some("Ics".into()),
                    mailbox: "x".into(),
                    host: Some("y.z".into())
                },
                AddrItem::GroupStart("Group".into()),
                AddrItem::Mailbox {
                    name: None,
                    mailbox: "b".into(),
                    host: Some("c.d".into())
                },
                AddrItem::GroupEnd,
            ]
        );
        assert_eq!(
            parse_address_list("undisclosed-recipients:;"),
            vec![
                AddrItem::GroupStart("undisclosed-recipients".into()),
                AddrItem::GroupEnd
            ]
        );
    }

    #[test]
    fn malformed_input_does_not_panic() {
        for raw in [
            &b""[..],
            b"\r\n",
            b"Content-Type: multipart/mixed; boundary=\r\n\r\n--\r\n",
            b"Content-Type: multipart/mixed; boundary=a\r\n\r\n--a\r\n--a\r\n--a--",
            b"Content-Type: message/rfc822\r\n\r\nContent-Type: multipart/x; boundary=b\r\n\r\n--b",
            "From: <\u{e8}@\u{e0}>, \"unterminated\r\n\r\n".as_bytes(),
        ] {
            let root = parse_message(raw);
            let _ = root.body_lines(raw);
            let _ = resolve_part(&root, &[1, 1, 1]);
            let (fields, _) = parse_header_block(raw, 0, raw.len());
            let _ = search_text(raw, &fields, &root);
            for f in &fields {
                let _ = parse_address_list(&f.value);
            }
        }
    }
}

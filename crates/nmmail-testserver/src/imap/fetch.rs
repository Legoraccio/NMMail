//! FETCH: attribute parsing and response rendering (ENVELOPE, BODYSTRUCTURE, body
//! sections, partial fetches, Gmail attributes).

use std::ops::Range;

use crate::imap::proto::{
    self, Arg, push_flag_list, push_literal, push_nstring, push_quoted, push_string,
};
use crate::mime::{self, AddrItem, Part, PartKind};
use crate::store::MessageData;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FetchAtt {
    Uid,
    Flags,
    InternalDate,
    Rfc822Size,
    Envelope,
    /// Non-extensible BODYSTRUCTURE.
    Body,
    BodyStructure,
    Rfc822,
    Rfc822Header,
    Rfc822Text,
    Section {
        peek: bool,
        section: Section,
        /// `<start.len>` (length optional).
        partial: Option<(u32, Option<u32>)>,
    },
    GmMsgId,
    GmThrId,
    GmLabels,
}

impl FetchAtt {
    /// Whether fetching this attribute sets `\Seen`.
    pub(crate) fn sets_seen(&self) -> bool {
        matches!(
            self,
            FetchAtt::Rfc822 | FetchAtt::Rfc822Text | FetchAtt::Section { peek: false, .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
    pub path: Vec<u32>,
    pub kind: SectionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SectionKind {
    Full,
    Header,
    HeaderFields(Vec<String>),
    HeaderFieldsNot(Vec<String>),
    Text,
    Mime,
}

/// Parses the attribute part of a FETCH command (a macro, one attribute or a list).
pub(crate) fn parse_fetch_atts(args: &[Arg], gmail: bool) -> Result<Vec<FetchAtt>, String> {
    let atts = match args {
        [Arg::Atom(atom)] => match atom.to_ascii_uppercase().as_str() {
            "ALL" => vec![
                FetchAtt::Flags,
                FetchAtt::InternalDate,
                FetchAtt::Rfc822Size,
                FetchAtt::Envelope,
            ],
            "FAST" => vec![
                FetchAtt::Flags,
                FetchAtt::InternalDate,
                FetchAtt::Rfc822Size,
            ],
            "FULL" => vec![
                FetchAtt::Flags,
                FetchAtt::InternalDate,
                FetchAtt::Rfc822Size,
                FetchAtt::Envelope,
                FetchAtt::Body,
            ],
            _ => vec![parse_att(atom, gmail)?],
        },
        [Arg::List(items)] if !items.is_empty() => items
            .iter()
            .map(|item| match item {
                Arg::Atom(atom) => parse_att(atom, gmail),
                _ => Err("Invalid fetch attribute".to_string()),
            })
            .collect::<Result<_, _>>()?,
        [] => return Err("Missing fetch attributes".to_string()),
        [_, _, ..] => return Err("FETCH modifiers are not supported".to_string()),
        _ => return Err("Invalid fetch attributes".to_string()),
    };
    Ok(atts)
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn parse_att(atom: &str, gmail: bool) -> Result<FetchAtt, String> {
    let att = match atom.to_ascii_uppercase().as_str() {
        "UID" => FetchAtt::Uid,
        "FLAGS" => FetchAtt::Flags,
        "INTERNALDATE" => FetchAtt::InternalDate,
        "RFC822.SIZE" => FetchAtt::Rfc822Size,
        "ENVELOPE" => FetchAtt::Envelope,
        "BODY" => FetchAtt::Body,
        "BODYSTRUCTURE" => FetchAtt::BodyStructure,
        "RFC822" => FetchAtt::Rfc822,
        "RFC822.HEADER" => FetchAtt::Rfc822Header,
        "RFC822.TEXT" => FetchAtt::Rfc822Text,
        "X-GM-MSGID" if gmail => FetchAtt::GmMsgId,
        "X-GM-THRID" if gmail => FetchAtt::GmThrId,
        "X-GM-LABELS" if gmail => FetchAtt::GmLabels,
        _ => {
            let (peek, rest) = if let Some(rest) = strip_prefix_ci(atom, "BODY.PEEK[") {
                (true, rest)
            } else if let Some(rest) = strip_prefix_ci(atom, "BODY[") {
                (false, rest)
            } else {
                return Err(format!("Unknown fetch attribute {atom}"));
            };
            let close = rest.find(']').ok_or("Missing ] in section")?;
            let section = parse_section(&rest[..close])?;
            let tail = &rest[close + 1..];
            let partial = if tail.is_empty() {
                None
            } else {
                Some(parse_partial(tail)?)
            };
            FetchAtt::Section {
                peek,
                section,
                partial,
            }
        }
    };
    Ok(att)
}

fn parse_section(spec: &str) -> Result<Section, String> {
    let mut path = Vec::new();
    let mut rest = spec;
    loop {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            break;
        }
        let n: u32 = rest[..digits].parse().map_err(|_| "Invalid section part")?;
        if n == 0 {
            return Err("Invalid section part".to_string());
        }
        path.push(n);
        rest = &rest[digits..];
        match rest.strip_prefix('.') {
            Some(r) => rest = r,
            None if rest.is_empty() => break,
            None => return Err("Invalid section".to_string()),
        }
    }
    let kind = if rest.is_empty() {
        SectionKind::Full
    } else if rest.eq_ignore_ascii_case("HEADER") {
        SectionKind::Header
    } else if rest.eq_ignore_ascii_case("TEXT") {
        SectionKind::Text
    } else if rest.eq_ignore_ascii_case("MIME") && !path.is_empty() {
        SectionKind::Mime
    } else if let Some(list) = strip_prefix_ci(rest, "HEADER.FIELDS.NOT") {
        SectionKind::HeaderFieldsNot(parse_field_names(list)?)
    } else if let Some(list) = strip_prefix_ci(rest, "HEADER.FIELDS") {
        SectionKind::HeaderFields(parse_field_names(list)?)
    } else {
        return Err(format!("Invalid section {spec}"));
    };
    Ok(Section { path, kind })
}

fn parse_field_names(list: &str) -> Result<Vec<String>, String> {
    match proto::parse_args(list.as_bytes())?.as_slice() {
        [Arg::List(items)] if !items.is_empty() => items
            .iter()
            .map(|item| {
                item.string()
                    .ok_or_else(|| "Invalid header field name".to_string())
            })
            .collect(),
        _ => Err("Invalid header field list".to_string()),
    }
}

fn parse_partial(tail: &str) -> Result<(u32, Option<u32>), String> {
    let inner = tail
        .strip_prefix('<')
        .and_then(|t| t.strip_suffix('>'))
        .ok_or("Invalid partial range")?;
    let number = |s: &str| {
        s.parse::<u32>()
            .map_err(|_| "Invalid partial range".to_string())
    };
    Ok(match inner.split_once('.') {
        Some((start, len)) => (number(start)?, Some(number(len)?)),
        None => (number(inner)?, None),
    })
}

/// What is needed to render one FETCH response.
pub(crate) struct FetchItem<'a> {
    pub seq: u32,
    pub uid: u32,
    pub flags: &'a [String],
    pub recent: bool,
    pub data: &'a MessageData,
    pub labels: &'a [String],
}

/// Renders `* seq FETCH (...)`. `force_uid` adds UID (UID FETCH), `force_flags` adds
/// FLAGS (when the fetch itself changed them).
pub(crate) fn render(
    out: &mut Vec<u8>,
    item: &FetchItem<'_>,
    atts: &[FetchAtt],
    force_uid: bool,
    force_flags: bool,
) {
    out.extend_from_slice(format!("* {} FETCH (", item.seq).as_bytes());
    let mut first = true;
    let mut separate = |out: &mut Vec<u8>| {
        if !first {
            out.push(b' ');
        }
        first = false;
    };
    if force_uid && !atts.contains(&FetchAtt::Uid) {
        separate(out);
        render_att(out, item, &FetchAtt::Uid);
    }
    for att in atts {
        separate(out);
        render_att(out, item, att);
    }
    if force_flags && !atts.contains(&FetchAtt::Flags) {
        separate(out);
        render_att(out, item, &FetchAtt::Flags);
    }
    out.extend_from_slice(b")\r\n");
}

fn slice(raw: &[u8], range: &Range<usize>) -> Vec<u8> {
    raw.get(range.clone()).unwrap_or_default().to_vec()
}

fn render_att(out: &mut Vec<u8>, item: &FetchItem<'_>, att: &FetchAtt) {
    let data = item.data;
    let raw = &data.raw;
    match att {
        FetchAtt::Uid => out.extend_from_slice(format!("UID {}", item.uid).as_bytes()),
        FetchAtt::Flags => {
            out.extend_from_slice(b"FLAGS ");
            push_flag_list(out, item.flags, item.recent);
        }
        FetchAtt::InternalDate => {
            out.extend_from_slice(b"INTERNALDATE ");
            push_quoted(out, proto::format_date_time(&data.internal_date).as_bytes());
        }
        FetchAtt::Rfc822Size => {
            out.extend_from_slice(format!("RFC822.SIZE {}", raw.len()).as_bytes())
        }
        FetchAtt::Envelope => {
            out.extend_from_slice(b"ENVELOPE ");
            push_envelope(out, raw, &data.mime);
        }
        FetchAtt::Body => {
            out.extend_from_slice(b"BODY ");
            push_body(out, raw, &data.mime, false);
        }
        FetchAtt::BodyStructure => {
            out.extend_from_slice(b"BODYSTRUCTURE ");
            push_body(out, raw, &data.mime, true);
        }
        FetchAtt::Rfc822 => {
            out.extend_from_slice(b"RFC822 ");
            push_literal(out, raw);
        }
        FetchAtt::Rfc822Header => {
            out.extend_from_slice(b"RFC822.HEADER ");
            push_literal(out, &slice(raw, &data.mime.header));
        }
        FetchAtt::Rfc822Text => {
            out.extend_from_slice(b"RFC822.TEXT ");
            push_literal(out, &slice(raw, &data.mime.body));
        }
        FetchAtt::Section {
            section, partial, ..
        } => {
            out.extend_from_slice(format!("BODY[{}]", section_spec(section)).as_bytes());
            if let Some((start, _)) = partial {
                out.extend_from_slice(format!("<{start}>").as_bytes());
            }
            out.push(b' ');
            match section_data(data, section) {
                Some(bytes) => push_literal(out, apply_partial(&bytes, *partial)),
                None => out.extend_from_slice(b"NIL"),
            }
        }
        FetchAtt::GmMsgId => {
            out.extend_from_slice(format!("X-GM-MSGID {}", data.gm_msgid).as_bytes())
        }
        FetchAtt::GmThrId => {
            out.extend_from_slice(format!("X-GM-THRID {}", data.gm_thrid).as_bytes())
        }
        FetchAtt::GmLabels => {
            out.extend_from_slice(b"X-GM-LABELS (");
            for (i, label) in item.labels.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                if label.starts_with('\\') {
                    out.extend_from_slice(label.as_bytes());
                } else {
                    push_string(out, label.as_bytes());
                }
            }
            out.push(b')');
        }
    }
}

fn apply_partial(bytes: &[u8], partial: Option<(u32, Option<u32>)>) -> &[u8] {
    match partial {
        None => bytes,
        Some((start, len)) => {
            let start = usize::try_from(start)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let end = match len {
                Some(len) => start
                    .saturating_add(usize::try_from(len).unwrap_or(usize::MAX))
                    .min(bytes.len()),
                None => bytes.len(),
            };
            &bytes[start..end]
        }
    }
}

fn is_atom_safe(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b > 0x20 && b < 0x7f && !b"(){%*\"\\]".contains(&b))
}

/// Section spec as echoed in the response (`1.2.HEADER.FIELDS (FROM TO)`).
fn section_spec(section: &Section) -> String {
    let names = |names: &[String]| {
        names
            .iter()
            .map(|n| {
                if is_atom_safe(n) {
                    n.clone()
                } else {
                    let mut quoted = Vec::new();
                    push_quoted(&mut quoted, n.as_bytes());
                    String::from_utf8_lossy(&quoted).into_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut spec = section
        .path
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".");
    let keyword = match &section.kind {
        SectionKind::Full => String::new(),
        SectionKind::Header => "HEADER".to_string(),
        SectionKind::Text => "TEXT".to_string(),
        SectionKind::Mime => "MIME".to_string(),
        SectionKind::HeaderFields(list) => format!("HEADER.FIELDS ({})", names(list)),
        SectionKind::HeaderFieldsNot(list) => format!("HEADER.FIELDS.NOT ({})", names(list)),
    };
    if !spec.is_empty() && !keyword.is_empty() {
        spec.push('.');
    }
    spec.push_str(&keyword);
    spec
}

/// Bytes of a body section, `None` if the section does not exist.
pub(crate) fn section_data(data: &MessageData, section: &Section) -> Option<Vec<u8>> {
    let raw = &data.raw;
    let root = &data.mime;
    let node = if section.path.is_empty() {
        root
    } else {
        mime::resolve_part(root, &section.path)?
    };
    // HEADER / TEXT / HEADER.FIELDS address the root message or an encapsulated one.
    let message = if section.path.is_empty() {
        Some(root)
    } else if let PartKind::Message(inner) = &node.kind {
        Some(inner.as_ref())
    } else {
        None
    };
    Some(match &section.kind {
        SectionKind::Full if section.path.is_empty() => raw.clone(),
        SectionKind::Full => slice(raw, &node.body),
        SectionKind::Mime => slice(raw, &node.header),
        SectionKind::Header => slice(raw, &message?.header),
        SectionKind::Text => slice(raw, &message?.body),
        SectionKind::HeaderFields(names) => filter_header(raw, message?, names, false),
        SectionKind::HeaderFieldsNot(names) => filter_header(raw, message?, names, true),
    })
}

/// The listed (or, with `exclude`, the unlisted) header fields plus the blank line.
fn filter_header(raw: &[u8], message: &Part, names: &[String], exclude: bool) -> Vec<u8> {
    let (fields, _) = mime::parse_header_block(raw, message.header.start, message.header.end);
    let mut out = Vec::new();
    for field in &fields {
        let listed = names.iter().any(|n| n.eq_ignore_ascii_case(&field.name));
        if listed != exclude {
            let bytes = raw.get(field.raw.clone()).unwrap_or_default();
            out.extend_from_slice(bytes);
            if !bytes.ends_with(b"\n") {
                out.extend_from_slice(b"\r\n");
            }
        }
    }
    out.extend_from_slice(b"\r\n");
    out
}

fn push_envelope(out: &mut Vec<u8>, raw: &[u8], message: &Part) {
    let (fields, _) = mime::parse_header_block(raw, message.header.start, message.header.end);
    let value = |name: &str| mime::header_value(&fields, name);
    let addresses = |name: &str| {
        value(name)
            .map(mime::parse_address_list)
            .filter(|list| !list.is_empty())
    };
    let from = addresses("from");
    let sender = addresses("sender").or_else(|| from.clone());
    let reply_to = addresses("reply-to").or_else(|| from.clone());
    out.push(b'(');
    push_nstring(out, value("date").map(str::as_bytes));
    out.push(b' ');
    push_nstring(out, value("subject").map(str::as_bytes));
    for list in [
        &from,
        &sender,
        &reply_to,
        &addresses("to"),
        &addresses("cc"),
        &addresses("bcc"),
    ] {
        out.push(b' ');
        push_address_list(out, list.as_deref());
    }
    out.push(b' ');
    push_nstring(out, value("in-reply-to").map(str::as_bytes));
    out.push(b' ');
    push_nstring(out, value("message-id").map(str::as_bytes));
    out.push(b')');
}

fn push_address_list(out: &mut Vec<u8>, list: Option<&[AddrItem]>) {
    let Some(list) = list else {
        out.extend_from_slice(b"NIL");
        return;
    };
    out.push(b'(');
    for item in list {
        match item {
            AddrItem::Mailbox {
                name,
                mailbox,
                host,
            } => {
                out.push(b'(');
                push_nstring(out, name.as_deref().map(str::as_bytes));
                out.extend_from_slice(b" NIL ");
                push_string(out, mailbox.as_bytes());
                out.push(b' ');
                push_nstring(out, host.as_deref().map(str::as_bytes));
                out.push(b')');
            }
            AddrItem::GroupStart(name) => {
                out.extend_from_slice(b"(NIL NIL ");
                push_string(out, name.as_bytes());
                out.extend_from_slice(b" NIL)");
            }
            AddrItem::GroupEnd => out.extend_from_slice(b"(NIL NIL NIL NIL)"),
        }
    }
    out.push(b')');
}

fn push_params(out: &mut Vec<u8>, params: &[(String, String)]) {
    if params.is_empty() {
        out.extend_from_slice(b"NIL");
        return;
    }
    out.push(b'(');
    for (i, (name, value)) in params.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        push_string(out, name.to_ascii_uppercase().as_bytes());
        out.push(b' ');
        push_string(out, value.as_bytes());
    }
    out.push(b')');
}

fn push_extension(out: &mut Vec<u8>, part: &Part) {
    out.push(b' ');
    match &part.disposition {
        Some((ty, params)) => {
            out.push(b'(');
            push_string(out, ty.to_ascii_uppercase().as_bytes());
            out.push(b' ');
            push_params(out, params);
            out.push(b')');
        }
        None => out.extend_from_slice(b"NIL"),
    }
    out.push(b' ');
    push_nstring(out, part.language.as_deref().map(str::as_bytes));
    out.push(b' ');
    push_nstring(out, part.location.as_deref().map(str::as_bytes));
}

/// BODY / BODYSTRUCTURE of a part (`extended` adds the extension data).
fn push_body(out: &mut Vec<u8>, raw: &[u8], part: &Part, extended: bool) {
    let content_type = &part.content_type;
    out.push(b'(');
    if let PartKind::Multipart(children) = &part.kind {
        for child in children {
            push_body(out, raw, child, extended);
        }
        out.push(b' ');
        push_string(out, content_type.subtype.to_ascii_uppercase().as_bytes());
        if extended {
            out.push(b' ');
            push_params(out, &content_type.params);
            push_extension(out, part);
        }
        out.push(b')');
        return;
    }
    push_string(out, content_type.ty.to_ascii_uppercase().as_bytes());
    out.push(b' ');
    push_string(out, content_type.subtype.to_ascii_uppercase().as_bytes());
    out.push(b' ');
    push_params(out, &content_type.params);
    out.push(b' ');
    push_nstring(out, part.id.as_deref().map(str::as_bytes));
    out.push(b' ');
    push_nstring(out, part.description.as_deref().map(str::as_bytes));
    out.push(b' ');
    let encoding = part
        .encoding
        .as_deref()
        .map(|e| e.trim().to_ascii_uppercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "7BIT".to_string());
    push_string(out, encoding.as_bytes());
    out.extend_from_slice(format!(" {}", part.body.len()).as_bytes());
    if let PartKind::Message(inner) = &part.kind {
        out.push(b' ');
        push_envelope(out, raw, inner);
        out.push(b' ');
        push_body(out, raw, inner, extended);
        out.extend_from_slice(format!(" {}", part.body_lines(raw)).as_bytes());
    } else if content_type.ty == "text" {
        out.extend_from_slice(format!(" {}", part.body_lines(raw)).as_bytes());
    }
    if extended {
        out.push(b' ');
        push_nstring(out, part.md5.as_deref().map(str::as_bytes));
        push_extension(out, part);
    }
    out.push(b')');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_attributes() {
        let atts = parse_fetch_atts(
            &[Arg::List(vec![
                Arg::Atom("UID".into()),
                Arg::Atom("body.peek[header.fields (From To)]<5.10>".into()),
                Arg::Atom("BODY[1.2.MIME]".into()),
            ])],
            false,
        )
        .unwrap();
        assert_eq!(
            atts[1],
            FetchAtt::Section {
                peek: true,
                section: Section {
                    path: vec![],
                    kind: SectionKind::HeaderFields(vec!["From".into(), "To".into()]),
                },
                partial: Some((5, Some(10))),
            }
        );
        assert_eq!(
            section_spec(&Section {
                path: vec![1, 2],
                kind: SectionKind::Mime
            }),
            "1.2.MIME"
        );
        assert!(parse_fetch_atts(&[Arg::Atom("X-GM-MSGID".into())], false).is_err());
        assert!(parse_fetch_atts(&[Arg::Atom("BODY[0]".into())], false).is_err());
        assert!(parse_fetch_atts(&[Arg::Atom("BODY[HEADER.FIELDS]".into())], false).is_err());
        assert!(parse_fetch_atts(&[Arg::Atom("BODY[]<a>".into())], false).is_err());
        assert_eq!(
            parse_fetch_atts(&[Arg::Atom("fast".into())], false)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn partial_ranges() {
        assert_eq!(apply_partial(b"abcdef", Some((2, Some(3)))), b"cde");
        assert_eq!(apply_partial(b"abcdef", Some((4, Some(100)))), b"ef");
        assert_eq!(apply_partial(b"abcdef", Some((10, None))), b"");
        assert_eq!(apply_partial(b"abcdef", None), b"abcdef");
    }
}

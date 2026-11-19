//! SEARCH: key parsing and evaluation (RFC 3501 keys plus Gmail `X-GM-RAW`,
//! `X-GM-MSGID` and `X-GM-THRID`).

use chrono::NaiveDate;

use crate::imap::proto::{self, Arg, SeqSet};
use crate::mime;
use crate::store::{MessageData, has_flag};

/// Maximum nesting of NOT / OR / parenthesized keys.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Key {
    All,
    /// System flag set (`true`) or not set (`false`).
    Flag(&'static str, bool),
    Keyword(String, bool),
    New,
    Old,
    Recent,
    Subject(String),
    From(String),
    To(String),
    Cc(String),
    Bcc(String),
    Body(String),
    Text(String),
    Header(String, String),
    Before(NaiveDate),
    On(NaiveDate),
    Since(NaiveDate),
    SentBefore(NaiveDate),
    SentOn(NaiveDate),
    SentSince(NaiveDate),
    Larger(u64),
    Smaller(u64),
    Uid(SeqSet),
    Seq(SeqSet),
    Not(Box<Key>),
    Or(Box<Key>, Box<Key>),
    And(Vec<Key>),
    GmRaw(String),
    GmMsgId(u64),
    GmThrId(u64),
}

/// Parses the arguments of SEARCH (an optional `CHARSET x` prefix is accepted).
pub(crate) fn parse_search(args: &[Arg], gmail: bool) -> Result<Key, String> {
    let mut i = 0;
    if args.first().is_some_and(|a| a.is_atom("CHARSET")) {
        i = 2;
    }
    let mut keys = Vec::new();
    while i < args.len() {
        keys.push(parse_key(args, &mut i, gmail, 0)?);
    }
    if keys.is_empty() {
        return Err("Missing search criteria".to_string());
    }
    Ok(Key::And(keys))
}

fn next_string(args: &[Arg], i: &mut usize) -> Result<String, String> {
    let value = args
        .get(*i)
        .and_then(Arg::string)
        .ok_or_else(|| "Missing search argument".to_string())?;
    *i += 1;
    Ok(value)
}

fn next_date(args: &[Arg], i: &mut usize) -> Result<NaiveDate, String> {
    let value = next_string(args, i)?;
    proto::parse_date(&value).ok_or_else(|| format!("Invalid date {value}"))
}

fn next_number(args: &[Arg], i: &mut usize) -> Result<u64, String> {
    let value = next_string(args, i)?;
    value.parse().map_err(|_| format!("Invalid number {value}"))
}

fn parse_key(args: &[Arg], i: &mut usize, gmail: bool, depth: usize) -> Result<Key, String> {
    if depth > MAX_DEPTH {
        return Err("Search keys nested too deeply".to_string());
    }
    let arg = args.get(*i).ok_or("Missing search key")?;
    *i += 1;
    let atom = match arg {
        Arg::List(items) => {
            let mut j = 0;
            let mut keys = Vec::new();
            while j < items.len() {
                keys.push(parse_key(items, &mut j, gmail, depth + 1)?);
            }
            if keys.is_empty() {
                return Err("Empty search list".to_string());
            }
            return Ok(Key::And(keys));
        }
        Arg::Str(_) => return Err("Unexpected string in search".to_string()),
        Arg::Atom(atom) => atom,
    };
    let lower = |s: String| s.to_lowercase();
    let key = match atom.to_ascii_uppercase().as_str() {
        "ALL" => Key::All,
        "ANSWERED" => Key::Flag("\\Answered", true),
        "UNANSWERED" => Key::Flag("\\Answered", false),
        "DELETED" => Key::Flag("\\Deleted", true),
        "UNDELETED" => Key::Flag("\\Deleted", false),
        "DRAFT" => Key::Flag("\\Draft", true),
        "UNDRAFT" => Key::Flag("\\Draft", false),
        "FLAGGED" => Key::Flag("\\Flagged", true),
        "UNFLAGGED" => Key::Flag("\\Flagged", false),
        "SEEN" => Key::Flag("\\Seen", true),
        "UNSEEN" => Key::Flag("\\Seen", false),
        "NEW" => Key::New,
        "OLD" => Key::Old,
        "RECENT" => Key::Recent,
        "KEYWORD" => Key::Keyword(next_string(args, i)?, true),
        "UNKEYWORD" => Key::Keyword(next_string(args, i)?, false),
        "SUBJECT" => Key::Subject(lower(next_string(args, i)?)),
        "FROM" => Key::From(lower(next_string(args, i)?)),
        "TO" => Key::To(lower(next_string(args, i)?)),
        "CC" => Key::Cc(lower(next_string(args, i)?)),
        "BCC" => Key::Bcc(lower(next_string(args, i)?)),
        "BODY" => Key::Body(lower(next_string(args, i)?)),
        "TEXT" => Key::Text(lower(next_string(args, i)?)),
        "HEADER" => {
            let name = next_string(args, i)?;
            Key::Header(name, lower(next_string(args, i)?))
        }
        "BEFORE" => Key::Before(next_date(args, i)?),
        "ON" => Key::On(next_date(args, i)?),
        "SINCE" => Key::Since(next_date(args, i)?),
        "SENTBEFORE" => Key::SentBefore(next_date(args, i)?),
        "SENTON" => Key::SentOn(next_date(args, i)?),
        "SENTSINCE" => Key::SentSince(next_date(args, i)?),
        "LARGER" => Key::Larger(next_number(args, i)?),
        "SMALLER" => Key::Smaller(next_number(args, i)?),
        "UID" => {
            let set = next_string(args, i)?;
            Key::Uid(SeqSet::parse(&set).ok_or_else(|| format!("Invalid UID set {set}"))?)
        }
        "NOT" => Key::Not(Box::new(parse_key(args, i, gmail, depth + 1)?)),
        "OR" => {
            let a = parse_key(args, i, gmail, depth + 1)?;
            let b = parse_key(args, i, gmail, depth + 1)?;
            Key::Or(Box::new(a), Box::new(b))
        }
        "X-GM-RAW" if gmail => Key::GmRaw(next_string(args, i)?),
        "X-GM-MSGID" if gmail => Key::GmMsgId(next_number(args, i)?),
        "X-GM-THRID" if gmail => Key::GmThrId(next_number(args, i)?),
        _ => match SeqSet::parse(atom) {
            Some(set) => Key::Seq(set),
            None => return Err(format!("Unknown search key {atom}")),
        },
    };
    Ok(key)
}

/// A message being tested against a search key.
pub(crate) struct Candidate<'a> {
    pub seq: u32,
    pub uid: u32,
    pub flags: &'a [String],
    pub recent: bool,
    pub data: &'a MessageData,
}

/// Values of `*` for sequence numbers and UIDs.
pub(crate) struct Limits {
    pub max_seq: u32,
    pub max_uid: u32,
}

pub(crate) fn matches(key: &Key, c: &Candidate<'_>, limits: &Limits) -> bool {
    let date = c.data.internal_date.date_naive();
    let search = &c.data.search;
    match key {
        Key::All => true,
        Key::Flag(flag, set) => has_flag(c.flags, flag) == *set,
        Key::Keyword(flag, set) => has_flag(c.flags, flag) == *set,
        Key::New => c.recent && !has_flag(c.flags, "\\Seen"),
        Key::Old => !c.recent,
        Key::Recent => c.recent,
        Key::Subject(s) => search.subject.contains(s.as_str()),
        Key::From(s) => search.from.contains(s.as_str()),
        Key::To(s) => search.to.contains(s.as_str()),
        Key::Cc(s) => search.cc.contains(s.as_str()),
        Key::Bcc(s) => search.bcc.contains(s.as_str()),
        Key::Body(s) => search.body.contains(s.as_str()),
        Key::Text(s) => {
            search.body.contains(s.as_str()) || header_text(c.data).contains(s.as_str())
        }
        Key::Header(name, value) => {
            let (fields, _) = mime::parse_header_block(
                &c.data.raw,
                c.data.mime.header.start,
                c.data.mime.header.end,
            );
            fields
                .iter()
                .filter(|f| f.name.eq_ignore_ascii_case(name))
                .any(|f| f.value.to_lowercase().contains(value.as_str()))
        }
        Key::Before(d) => date < *d,
        Key::On(d) => date == *d,
        Key::Since(d) => date >= *d,
        Key::SentBefore(d) => c.data.sent_date.is_some_and(|s| s < *d),
        Key::SentOn(d) => c.data.sent_date.is_some_and(|s| s == *d),
        Key::SentSince(d) => c.data.sent_date.is_some_and(|s| s >= *d),
        Key::Larger(n) => c.data.raw.len() as u64 > *n,
        Key::Smaller(n) => (c.data.raw.len() as u64) < *n,
        Key::Uid(set) => set.contains(c.uid, limits.max_uid),
        Key::Seq(set) => set.contains(c.seq, limits.max_seq),
        Key::Not(inner) => !matches(inner, c, limits),
        Key::Or(a, b) => matches(a, c, limits) || matches(b, c, limits),
        Key::And(keys) => keys.iter().all(|k| matches(k, c, limits)),
        Key::GmRaw(query) => gm_raw(query, c),
        Key::GmMsgId(id) => c.data.gm_msgid == *id,
        Key::GmThrId(id) => c.data.gm_thrid == *id,
    }
}

fn header_text(data: &MessageData) -> String {
    let header = data.raw.get(data.mime.header.clone()).unwrap_or_default();
    String::from_utf8_lossy(header).to_lowercase()
        + "\n"
        + &data.search.subject
        + "\n"
        + &data.search.from
        + "\n"
        + &data.search.to
        + "\n"
        + &data.search.cc
}

/// Gmail search emulation: `category:<name>` terms match the `X-Test-Category` header
/// (`category:primary` also matches messages without it), `-term` negates, any other
/// term is a case-insensitive substring of the raw message. All terms must match.
fn gm_raw(query: &str, c: &Candidate<'_>) -> bool {
    let mut raw_lower: Option<String> = None;
    for term in query.split_whitespace() {
        let term = term.trim_matches('"').to_lowercase();
        let (negated, term) = match term.strip_prefix('-') {
            Some(rest) if !rest.is_empty() => (true, rest.to_string()),
            _ => (false, term),
        };
        if term.is_empty() {
            continue;
        }
        let matched = if let Some(category) = term.strip_prefix("category:") {
            match &c.data.category {
                Some(value) => value == category,
                None => category == "primary",
            }
        } else {
            raw_lower
                .get_or_insert_with(|| String::from_utf8_lossy(&c.data.raw).to_lowercase())
                .contains(term.as_str())
        };
        if matched == negated {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap::proto::parse_command;

    fn parse(line: &str, gmail: bool) -> Result<Key, String> {
        let cmd =
            parse_command(format!("a SEARCH {line}").as_bytes()).map_err(|e| format!("{e:?}"))?;
        parse_search(&cmd.args, gmail)
    }

    #[test]
    fn parses_keys() {
        assert!(parse("ALL", false).is_ok());
        assert!(parse("CHARSET UTF-8 SUBJECT \"ciao mondo\"", false).is_ok());
        assert!(parse("OR (FROM a SEEN) NOT UID 1:*", false).is_ok());
        assert!(
            parse(
                "1:5,7 SINCE 1-Feb-2024 HEADER X-Test-Category promotions",
                false
            )
            .is_ok()
        );
        assert!(parse("X-GM-RAW \"category:primary\"", true).is_ok());
        assert!(parse("X-GM-RAW \"category:primary\"", false).is_err());
        assert!(parse("SINCE yesterday", false).is_err());
        assert!(parse("OR SEEN", false).is_err());
        assert!(parse("", false).is_err());
        let deep = "NOT ".repeat(200) + "ALL";
        assert!(parse(&deep, false).is_err());
    }
}

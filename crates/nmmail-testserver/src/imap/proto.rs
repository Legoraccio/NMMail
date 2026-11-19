//! IMAP syntax: command tokenizer, sequence sets, dates and response encoding helpers.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeZone};

/// Maximum nesting of parenthesized lists in a command.
const MAX_LIST_DEPTH: usize = 32;

/// One command argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Arg {
    /// An atom (bracketed sections such as `BODY[HEADER.FIELDS (A B)]<0.10>` included).
    Atom(String),
    /// A quoted string or a literal.
    Str(Vec<u8>),
    /// A parenthesized list.
    List(Vec<Arg>),
}

impl Arg {
    pub(crate) fn atom(&self) -> Option<&str> {
        match self {
            Arg::Atom(a) => Some(a),
            _ => None,
        }
    }

    /// The value of an `astring` (atom or string) as lossy UTF-8.
    pub(crate) fn string(&self) -> Option<String> {
        match self {
            Arg::Atom(a) => Some(a.clone()),
            Arg::Str(s) => Some(String::from_utf8_lossy(s).into_owned()),
            Arg::List(_) => None,
        }
    }

    pub(crate) fn is_atom(&self, value: &str) -> bool {
        self.atom().is_some_and(|a| a.eq_ignore_ascii_case(value))
    }
}

/// A parsed command line.
#[derive(Debug, Clone)]
pub(crate) struct Command {
    pub tag: String,
    /// Uppercase command name.
    pub name: String,
    pub args: Vec<Arg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandError {
    /// The line does not start with a valid tag.
    NoTag,
    /// The tag is valid but the rest is not.
    Syntax { tag: String, message: String },
}

fn is_tag_char(b: u8) -> bool {
    b > 0x20 && b < 0x7f && !matches!(b, b'(' | b')' | b'{' | b'%' | b'*' | b'"' | b'\\' | b'+')
}

fn is_atom_char(b: u8) -> bool {
    b > 0x20 && b != 0x7f && !matches!(b, b'(' | b')' | b'"')
}

/// Parses a full command (literals inlined as `{n}\r\n<bytes>`).
pub(crate) fn parse_command(buf: &[u8]) -> Result<Command, CommandError> {
    let tag_len = buf.iter().take_while(|&&b| is_tag_char(b)).count();
    if tag_len == 0 {
        return Err(CommandError::NoTag);
    }
    let tag = String::from_utf8_lossy(&buf[..tag_len]).into_owned();
    let syntax = |message: &str| CommandError::Syntax {
        tag: tag.clone(),
        message: message.to_string(),
    };
    let rest = &buf[tag_len..];
    let Some(rest) = rest.strip_prefix(b" ") else {
        return Err(if rest.is_empty() {
            syntax("Missing command")
        } else {
            CommandError::NoTag
        });
    };
    let name_len = rest
        .iter()
        .take_while(|&&b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
        .count();
    if name_len == 0 {
        return Err(syntax("Missing command"));
    }
    let name = String::from_utf8_lossy(&rest[..name_len]).to_ascii_uppercase();
    let mut lexer = Lexer {
        buf: &rest[name_len..],
        pos: 0,
    };
    if !lexer.at_end() && !lexer.eat(b' ') {
        return Err(syntax("Invalid command name"));
    }
    let args = lexer.parse_args(0).map_err(|m| syntax(&m))?;
    Ok(Command {
        tag: tag.clone(),
        name,
        args,
    })
}

/// Parses a standalone argument list (e.g. the field list of `HEADER.FIELDS`).
pub(crate) fn parse_args(buf: &[u8]) -> Result<Vec<Arg>, String> {
    Lexer { buf, pos: 0 }.parse_args(0)
}

struct Lexer<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Parses arguments until the end of input (depth 0) or a closing parenthesis.
    fn parse_args(&mut self, depth: usize) -> Result<Vec<Arg>, String> {
        let mut out = Vec::new();
        loop {
            while matches!(self.peek(), Some(b' ' | b'\r' | b'\n' | b'\t')) {
                self.pos += 1;
            }
            match self.peek() {
                None if depth == 0 => return Ok(out),
                None => return Err("Unterminated list".to_string()),
                Some(b')') if depth > 0 => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b')') => return Err("Unexpected )".to_string()),
                Some(b'(') => {
                    if depth >= MAX_LIST_DEPTH {
                        return Err("Lists nested too deeply".to_string());
                    }
                    self.pos += 1;
                    out.push(Arg::List(self.parse_args(depth + 1)?));
                }
                Some(b'"') => out.push(Arg::Str(self.parse_quoted()?)),
                Some(b'{') => match self.parse_literal()? {
                    Some(literal) => out.push(Arg::Str(literal)),
                    None => out.push(Arg::Atom(self.parse_atom())),
                },
                Some(_) => {
                    let atom = self.parse_atom();
                    if atom.is_empty() {
                        return Err("Invalid character".to_string());
                    }
                    out.push(Arg::Atom(atom));
                }
            }
        }
    }

    fn parse_quoted(&mut self) -> Result<Vec<u8>, String> {
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'\r' | b'\n') => return Err("Unterminated quoted string".to_string()),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(c) => {
                            out.push(c);
                            self.pos += 1;
                        }
                        None => return Err("Unterminated quoted string".to_string()),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// `{n}` or `{n+}` followed by CRLF and n bytes. Returns `None` (without consuming
    /// anything) if the text at the cursor is not a literal header.
    fn parse_literal(&mut self) -> Result<Option<Vec<u8>>, String> {
        let rest = &self.buf[self.pos..];
        let Some(close) = rest.iter().position(|&b| b == b'}') else {
            return Ok(None);
        };
        let inner = &rest[1..close];
        let digits = inner.strip_suffix(b"+").unwrap_or(inner);
        if digits.is_empty() || digits.len() > 10 || !digits.iter().all(u8::is_ascii_digit) {
            return Ok(None);
        }
        let Some(len) = std::str::from_utf8(digits)
            .ok()
            .and_then(|d| d.parse::<usize>().ok())
        else {
            return Ok(None);
        };
        let after = &rest[close + 1..];
        let data_start = if after.starts_with(b"\r\n") {
            2
        } else if after.starts_with(b"\n") {
            1
        } else {
            return Err("Literal must be followed by CRLF".to_string());
        };
        let data = after
            .get(data_start..data_start + len)
            .ok_or_else(|| "Truncated literal".to_string())?;
        self.pos += close + 1 + data_start + len;
        Ok(Some(data.to_vec()))
    }

    fn parse_atom(&mut self) -> String {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'[' {
                // Section specs may contain spaces and parentheses: take up to ']'.
                match self.buf[self.pos..].iter().position(|&c| c == b']') {
                    Some(close) => self.pos += close + 1,
                    None => self.pos = self.buf.len(),
                }
                continue;
            }
            if !is_atom_char(b) {
                break;
            }
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.buf[start..self.pos]).into_owned()
    }
}

/// If `line` ends with a literal header (`{n}` or `{n+}`), returns `(n, non_synchronizing)`.
pub(crate) fn literal_suffix(line: &[u8]) -> Option<(usize, bool)> {
    let without_brace = line.strip_suffix(b"}")?;
    let open = without_brace.iter().rposition(|&b| b == b'{')?;
    let inner = &without_brace[open + 1..];
    let (digits, non_sync) = match inner.strip_suffix(b"+") {
        Some(d) => (d, true),
        None => (inner, false),
    };
    if digits.is_empty() || digits.len() > 10 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let len = std::str::from_utf8(digits).ok()?.parse().ok()?;
    Some((len, non_sync))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeqNum {
    Num(u32),
    Star,
}

impl SeqNum {
    fn resolve(self, star: u32) -> u32 {
        match self {
            SeqNum::Num(n) => n,
            SeqNum::Star => star,
        }
    }
}

/// A sequence set (`1`, `1:5`, `3:*`, `*`, `1,4,7:9`, reversed ranges allowed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeqSet(Vec<(SeqNum, SeqNum)>);

impl SeqSet {
    pub(crate) fn parse(s: &str) -> Option<SeqSet> {
        fn number(s: &str) -> Option<SeqNum> {
            if s == "*" {
                Some(SeqNum::Star)
            } else if !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit()) {
                s.parse().ok().map(SeqNum::Num)
            } else {
                None
            }
        }
        if s.is_empty() {
            return None;
        }
        let mut ranges = Vec::new();
        for item in s.split(',') {
            let range = match item.split_once(':') {
                Some((a, b)) => (number(a)?, number(b)?),
                None => {
                    let n = number(item)?;
                    (n, n)
                }
            };
            ranges.push(range);
        }
        Some(SeqSet(ranges))
    }

    /// Whether `n` is in the set, `*` standing for `star`.
    pub(crate) fn contains(&self, n: u32, star: u32) -> bool {
        self.0.iter().any(|&(a, b)| {
            let (a, b) = (a.resolve(star), b.resolve(star));
            let (low, high) = if a <= b { (a, b) } else { (b, a) };
            (low..=high).contains(&n)
        })
    }
}

/// Formats ascending UIDs as a compact set (`1:3,7`).
pub(crate) fn format_uid_set(uids: &[u32]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < uids.len() {
        let start = uids[i];
        let mut end = start;
        while i + 1 < uids.len() && uids[i + 1] == end.wrapping_add(1) {
            i += 1;
            end = uids[i];
        }
        if !out.is_empty() {
            out.push(',');
        }
        if start == end {
            out.push_str(&start.to_string());
        } else {
            out.push_str(&format!("{start}:{end}"));
        }
        i += 1;
    }
    out
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// `1-Feb-1994` (also with a leading space or zero).
pub(crate) fn parse_date(s: &str) -> Option<NaiveDate> {
    let mut parts = s.trim().splitn(3, '-');
    let day: u32 = parts.next()?.trim().parse().ok()?;
    let month = parts.next()?.to_ascii_lowercase();
    let month = MONTHS.iter().position(|m| *m == month)? + 1;
    let year: i32 = parts.next()?.trim().parse().ok()?;
    NaiveDate::from_ymd_opt(year, u32::try_from(month).ok()?, day)
}

/// `17-Jul-1996 02:44:25 -0700` (day may be space padded).
pub(crate) fn parse_date_time(s: &str) -> Option<DateTime<FixedOffset>> {
    let mut parts = s.split_whitespace();
    let date = parse_date(parts.next()?)?;
    let time = NaiveTime::parse_from_str(parts.next()?, "%H:%M:%S").ok()?;
    let zone = parts.next()?;
    if parts.next().is_some() || zone.len() != 5 || !zone.is_ascii() {
        return None;
    }
    let sign = match &zone[..1] {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let hours: i32 = zone[1..3].parse().ok()?;
    let minutes: i32 = zone[3..5].parse().ok()?;
    let offset = FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))?;
    offset.from_local_datetime(&date.and_time(time)).single()
}

/// INTERNALDATE format: `17-Jul-1996 02:44:25 -0700`.
pub(crate) fn format_date_time(d: &DateTime<FixedOffset>) -> String {
    d.format("%d-%b-%Y %H:%M:%S %z").to_string()
}

pub(crate) fn push_quoted(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &b in s {
        if b == b'"' || b == b'\\' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b'"');
}

pub(crate) fn push_literal(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(format!("{{{}}}\r\n", s.len()).as_bytes());
    out.extend_from_slice(s);
}

/// A string: quoted when it is short printable ASCII, a literal otherwise.
pub(crate) fn push_string(out: &mut Vec<u8>, s: &[u8]) {
    if s.len() <= 1024 && s.iter().all(|&b| (0x20..0x7f).contains(&b)) {
        push_quoted(out, s);
    } else {
        push_literal(out, s);
    }
}

pub(crate) fn push_nstring(out: &mut Vec<u8>, s: Option<&[u8]>) {
    match s {
        Some(s) => push_string(out, s),
        None => out.extend_from_slice(b"NIL"),
    }
}

/// `(\Seen \Flagged $Keyword \Recent)`.
pub(crate) fn push_flag_list(out: &mut Vec<u8>, flags: &[String], recent: bool) {
    out.push(b'(');
    let mut first = true;
    for flag in flags
        .iter()
        .map(String::as_str)
        .chain(recent.then_some("\\Recent"))
    {
        if !first {
            out.push(b' ');
        }
        first = false;
        out.extend_from_slice(flag.as_bytes());
    }
    out.push(b')');
}

/// Mailbox names and other astrings in responses are always quoted (or literal).
pub(crate) fn push_astring(out: &mut Vec<u8>, s: &str) {
    push_string(out, s.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_commands() {
        let cmd = parse_command(b"a1 UID FETCH 1:* (UID BODY.PEEK[HEADER.FIELDS (FROM TO)]<0.10>)")
            .unwrap();
        assert_eq!(cmd.tag, "a1");
        assert_eq!(cmd.name, "UID");
        assert_eq!(
            cmd.args,
            vec![
                Arg::Atom("FETCH".into()),
                Arg::Atom("1:*".into()),
                Arg::List(vec![
                    Arg::Atom("UID".into()),
                    Arg::Atom("BODY.PEEK[HEADER.FIELDS (FROM TO)]<0.10>".into())
                ])
            ]
        );
        let cmd = parse_command(b"x LOGIN \"a\\\"b\" {3}\r\npwd").unwrap();
        assert_eq!(
            cmd.args,
            vec![Arg::Str(b"a\"b".to_vec()), Arg::Str(b"pwd".to_vec())]
        );
        let cmd = parse_command(b"x APPEND INBOX (\\Seen) {2+}\r\nhi").unwrap();
        assert_eq!(cmd.args.len(), 3);
        assert!(matches!(
            parse_command(b"NOOP"),
            Err(CommandError::Syntax { .. })
        ));
        assert!(matches!(parse_command(b"+ x"), Err(CommandError::NoTag)));
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        for line in [
            &b""[..],
            b"*",
            b"a",
            b"a ",
            b"a (",
            b"a X ((((",
            b"a X \"abc",
            b"a X {5}\r\nab",
            b"a X {99999999999999}",
            b"a X )",
            b"a X [",
            b"\xff\xfe X",
            b"a X \xc3",
        ] {
            let _ = parse_command(line);
        }
        assert!(matches!(
            parse_command(b"a X (b"),
            Err(CommandError::Syntax { .. })
        ));
    }

    #[test]
    fn literal_suffixes() {
        assert_eq!(literal_suffix(b"a APPEND INBOX {12}"), Some((12, false)));
        assert_eq!(literal_suffix(b"a APPEND INBOX {12+}"), Some((12, true)));
        assert_eq!(literal_suffix(b"a LOGIN \"{12}\""), None);
        assert_eq!(literal_suffix(b"{}"), None);
    }

    #[test]
    fn sequence_sets() {
        let set = SeqSet::parse("1,4,7:9,12:*").unwrap();
        assert!(set.contains(1, 20) && set.contains(8, 20) && set.contains(20, 20));
        assert!(!set.contains(5, 20) && !set.contains(10, 20));
        let reversed = SeqSet::parse("5:2").unwrap();
        assert!(reversed.contains(3, 10));
        assert!(
            SeqSet::parse("5:").is_none()
                && SeqSet::parse("a").is_none()
                && SeqSet::parse("").is_none()
        );
        // "n:*" with n beyond the last UID still matches the last message (RFC 3501).
        assert!(SeqSet::parse("100:*").unwrap().contains(10, 10));
        assert_eq!(format_uid_set(&[1, 2, 3, 5, 7, 8]), "1:3,5,7:8");
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_date("1-Feb-1994"),
            NaiveDate::from_ymd_opt(1994, 2, 1)
        );
        assert_eq!(
            parse_date("01-feb-1994"),
            NaiveDate::from_ymd_opt(1994, 2, 1)
        );
        assert_eq!(parse_date("31-Feb-1994"), None);
        let dt = parse_date_time(" 7-Jul-1996 02:44:25 -0700").unwrap();
        assert_eq!(format_date_time(&dt), "07-Jul-1996 02:44:25 -0700");
        assert!(parse_date_time("07-Jul-1996 02:44:25").is_none());
    }
}

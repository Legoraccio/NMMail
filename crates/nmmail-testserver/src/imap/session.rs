//! One IMAP connection: state machine, mailbox change propagation and command handlers.
//!
//! Each session keeps its own view of the selected mailbox (sequence number -> UID and
//! the flags last reported). After every change in the store the view is diffed against
//! the mailbox and the differences are reported as untagged EXPUNGE / FETCH / EXISTS /
//! RECENT responses: immediately while idling, otherwise before the tagged response of
//! the next command (EXPUNGE is withheld during FETCH, STORE and SEARCH, RFC 3501 7.4.1).

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tracing::{debug, info};

use crate::conn::{self, ConnIo, ConnKind};
use crate::imap::fetch::{self, FetchAtt, FetchItem};
use crate::imap::proto::{self, Arg, Command, CommandError, SeqSet, push_astring, push_flag_list};
use crate::imap::search::{self, Candidate, Limits};
use crate::store::{MessageData, SYSTEM_FLAGS, StoreError, has_flag, normalize_flag, sort_flags};
use crate::{Shared, lock};

/// Maximum length of a command line (literals excluded).
const MAX_LINE: usize = 1024 * 1024;
/// Maximum size of one literal.
const MAX_LITERAL: usize = 64 * 1024 * 1024;
/// Maximum size of a whole command (line plus literals).
const MAX_COMMAND: usize = MAX_LITERAL + MAX_LINE;

/// The error challenge Gmail sends when XOAUTH2 fails.
const XOAUTH2_ERROR: &str =
    r#"{"status":"400","schemes":"Bearer","scope":"https://mail.google.com/"}"#;

/// Commands that exist (used to tell "unknown" from "not authenticated").
const KNOWN_COMMANDS: &[&str] = &[
    "SELECT",
    "EXAMINE",
    "CREATE",
    "DELETE",
    "RENAME",
    "SUBSCRIBE",
    "UNSUBSCRIBE",
    "LIST",
    "LSUB",
    "STATUS",
    "APPEND",
    "NAMESPACE",
    "CHECK",
    "CLOSE",
    "UNSELECT",
    "EXPUNGE",
    "SEARCH",
    "FETCH",
    "STORE",
    "COPY",
    "MOVE",
    "UID",
];

pub(crate) async fn run(stream: TcpStream, peer: SocketAddr, shared: Arc<Shared>) {
    let (guard, ctl) = conn::register(&shared, ConnKind::Imap);
    let mut session = Session {
        changes: shared.changes.subscribe(),
        shared,
        io: ConnIo::new(stream, ctl),
        user: None,
        selected: None,
    };
    debug!(%peer, "IMAP connection opened");
    match session.serve().await {
        Ok(()) => debug!(%peer, "IMAP connection closed"),
        Err(err) if conn::is_killed_error(&err) => {
            debug!(%peer, "IMAP connection dropped by the test server");
            session.io.abort();
        }
        Err(err) => debug!(%peer, "IMAP connection closed: {err}"),
    }
    drop(guard);
}

enum Flow {
    Continue,
    Close,
}

enum SyncOutcome {
    Ok,
    /// The session must be closed with `* BYE <reason>`.
    Bye(&'static str),
}

/// Outcome of a non-interactive command.
struct Reply {
    status: &'static str,
    code: Option<String>,
    text: String,
    close: bool,
    /// Whether pending EXPUNGE responses may be sent before the tagged response.
    allow_expunge: bool,
}

impl Reply {
    fn new(status: &'static str, text: impl Into<String>) -> Reply {
        Reply {
            status,
            code: None,
            text: text.into(),
            close: false,
            allow_expunge: true,
        }
    }

    fn ok(text: impl Into<String>) -> Reply {
        Reply::new("OK", text)
    }

    fn no(text: impl Into<String>) -> Reply {
        Reply::new("NO", text)
    }

    fn bad(text: impl Into<String>) -> Reply {
        Reply::new("BAD", text)
    }

    fn with_code(mut self, code: impl Into<String>) -> Reply {
        self.code = Some(code.into());
        self
    }
}

/// A message collected under the store lock for a FETCH response.
struct Fetched {
    seq: u32,
    uid: u32,
    flags: Vec<String>,
    data: Arc<MessageData>,
    /// `\Seen` was set by this fetch.
    seen_set: bool,
    labels: Vec<String>,
}

/// One message as known by the session.
struct ViewEntry {
    uid: u32,
    /// Flags last reported to the client.
    flags: Vec<String>,
    recent: bool,
}

struct Selected {
    /// Canonical mailbox name.
    mailbox: String,
    read_only: bool,
    uid_validity: u32,
    /// Index + 1 = sequence number.
    view: Vec<ViewEntry>,
    /// Highest UID ever added to the view.
    max_uid: u32,
}

impl Selected {
    /// `(sequence number, uid)` of the view entries matched by a set.
    fn targets(&self, set: &SeqSet, uid_mode: bool) -> Vec<(u32, u32)> {
        let max_seq = u32::try_from(self.view.len()).unwrap_or(u32::MAX);
        let max_uid = self.view.last().map_or(0, |e| e.uid);
        self.view
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                let seq = u32::try_from(i + 1).unwrap_or(u32::MAX);
                let hit = if uid_mode {
                    set.contains(entry.uid, max_uid)
                } else {
                    set.contains(seq, max_seq)
                };
                hit.then_some((seq, entry.uid))
            })
            .collect()
    }

    fn entry_mut(&mut self, seq: u32, uid: u32) -> Option<&mut ViewEntry> {
        let index = usize::try_from(seq).ok()?.checked_sub(1)?;
        self.view.get_mut(index).filter(|e| e.uid == uid)
    }
}

struct Session {
    shared: Arc<Shared>,
    io: ConnIo,
    changes: watch::Receiver<u64>,
    /// Key of the authenticated user.
    user: Option<String>,
    selected: Option<Selected>,
}

/// First line of a command, for the log (bounded length).
fn first_line(buf: &[u8]) -> String {
    let end = buf
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end])
        .chars()
        .take(2000)
        .collect()
}

/// Commands whose arguments refer to messages: report new messages and flag changes
/// before executing them so `*` and UID ranges cover everything in the mailbox.
fn wants_presync(cmd: &Command) -> bool {
    matches!(
        cmd.name.as_str(),
        "FETCH" | "STORE" | "SEARCH" | "COPY" | "MOVE" | "UID"
    )
}

fn parse_plain(decoded: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8_lossy(decoded);
    let mut parts = text.split('\0');
    let authzid = parts.next()?;
    let authcid = parts.next()?;
    let password = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let user = if authcid.is_empty() { authzid } else { authcid };
    Some((user.to_string(), password.to_string()))
}

/// `user=<email>\x01auth=Bearer <token>\x01\x01`.
pub(crate) fn parse_xoauth2(decoded: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8_lossy(decoded);
    let mut user = None;
    let mut token = None;
    for field in text.split('\x01') {
        if let Some(value) = field.strip_prefix("user=") {
            user = Some(value.trim().to_string());
        } else if let Some(value) = field.strip_prefix("auth=") {
            let value = value.trim();
            let bearer = value.get(..7).filter(|p| p.eq_ignore_ascii_case("bearer "));
            if bearer.is_some() {
                token = Some(value[7..].trim().to_string());
            }
        }
    }
    Some((user?, token?))
}

/// LIST pattern matching: `*` matches anything, `%` anything but the delimiter.
fn list_match(pattern: &str, name: &str) -> bool {
    let pattern = match pattern.get(..5) {
        Some(head) if head.eq_ignore_ascii_case("INBOX") => format!("INBOX{}", &pattern[5..]),
        _ => pattern.to_string(),
    };
    let (pattern, name) = (pattern.as_bytes(), name.as_bytes());
    // previous[j]: the pattern consumed so far matches name[..j].
    let mut previous = vec![false; name.len() + 1];
    previous[0] = true;
    for &p in pattern {
        let mut current = vec![false; name.len() + 1];
        current[0] = previous[0] && (p == b'*' || p == b'%');
        for (j, &c) in name.iter().enumerate() {
            current[j + 1] = match p {
                b'*' => previous[j + 1] || current[j],
                b'%' => previous[j + 1] || (current[j] && c != b'/'),
                _ => previous[j] && c == p,
            };
        }
        previous = current;
    }
    previous[name.len()]
}

impl Session {
    fn capabilities(&self) -> String {
        let mut caps = String::from(
            "IMAP4rev1 LITERAL+ SASL-IR ID IDLE NAMESPACE UIDPLUS MOVE SPECIAL-USE UNSELECT CHILDREN AUTH=PLAIN",
        );
        if self.shared.xoauth2_token.is_some() {
            caps.push_str(" AUTH=XOAUTH2");
        }
        if self.shared.gmail_mode {
            caps.push_str(" X-GM-EXT-1");
        }
        caps
    }

    async fn serve(&mut self) -> io::Result<()> {
        let greeting = format!(
            "* OK [CAPABILITY {}] NM Mail test server ready\r\n",
            self.capabilities()
        );
        self.io.write_all(greeting.as_bytes()).await?;
        loop {
            let buf = match self.read_command().await {
                Ok(buf) => buf,
                Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                    let _ = self
                        .io
                        .write_all(format!("* BYE {err}\r\n").as_bytes())
                        .await;
                    return Ok(());
                }
                Err(err) => return Err(err),
            };
            if buf.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if let Flow::Close = self.handle(&buf).await? {
                return Ok(());
            }
        }
    }

    /// Reads one command, answering synchronizing literals with a continuation.
    async fn read_command(&mut self) -> io::Result<Vec<u8>> {
        let mut command = Vec::new();
        loop {
            let line = self.io.read_line(MAX_LINE).await?;
            let literal = proto::literal_suffix(&line);
            command.extend_from_slice(&line);
            let Some((len, non_sync)) = literal else {
                return Ok(command);
            };
            if len > MAX_LITERAL || command.len().saturating_add(len) > MAX_COMMAND {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Literal too large",
                ));
            }
            if !non_sync {
                self.io.write_all(b"+ Ready for literal\r\n").await?;
            }
            command.extend_from_slice(b"\r\n");
            let data = self.io.read_exact_bytes(len).await?;
            command.extend_from_slice(&data);
        }
    }

    fn log_command(&self, cmd: &Command, buf: &[u8]) {
        let first_arg = || cmd.args.first().and_then(Arg::string).unwrap_or_default();
        let entry = match cmd.name.as_str() {
            "LOGIN" => format!("{} LOGIN {} ***", cmd.tag, first_arg()),
            "AUTHENTICATE" if cmd.args.len() > 1 => {
                format!("{} AUTHENTICATE {} ***", cmd.tag, first_arg())
            }
            _ => first_line(buf),
        };
        debug!("C: {entry}");
        self.shared.log_command(entry);
    }

    /// Logs a line that could not be parsed, masking anything that may be a secret.
    fn log_raw(&self, buf: &[u8]) {
        let line = first_line(buf);
        let words: Vec<&str> = line.split_whitespace().collect();
        let sensitive = words.get(1).is_some_and(|w| {
            w.eq_ignore_ascii_case("LOGIN") || w.eq_ignore_ascii_case("AUTHENTICATE")
        });
        let entry = if sensitive && words.len() > 2 {
            format!("{} {} ***", words[0], words[1])
        } else {
            line
        };
        debug!("C: {entry}");
        self.shared.log_command(entry);
    }

    async fn handle(&mut self, buf: &[u8]) -> io::Result<Flow> {
        let cmd = match proto::parse_command(buf) {
            Ok(cmd) => cmd,
            Err(CommandError::NoTag) => {
                self.log_raw(buf);
                self.io.write_all(b"* BAD Invalid tag\r\n").await?;
                return Ok(Flow::Continue);
            }
            Err(CommandError::Syntax { tag, message }) => {
                self.log_raw(buf);
                self.io
                    .write_all(format!("{tag} BAD {message}\r\n").as_bytes())
                    .await?;
                return Ok(Flow::Continue);
            }
        };
        self.log_command(&cmd, buf);
        match cmd.name.as_str() {
            "AUTHENTICATE" => return self.authenticate(&cmd).await,
            "IDLE" => return self.idle(&cmd).await,
            _ => {}
        }
        let mut out = Vec::new();
        if self.selected.is_some() && wants_presync(&cmd) {
            if let SyncOutcome::Bye(reason) = self.sync(&mut out, false) {
                return self.bye(out, reason).await;
            }
        }
        let reply = self.execute(&cmd, &mut out);
        if self.selected.is_some() {
            if let SyncOutcome::Bye(reason) = self.sync(&mut out, reply.allow_expunge) {
                return self.bye(out, reason).await;
            }
        }
        let tagged = match &reply.code {
            Some(code) => format!("{} {} [{code}] {}", cmd.tag, reply.status, reply.text),
            None => format!("{} {} {}", cmd.tag, reply.status, reply.text),
        };
        debug!("S: {tagged} ({} bytes of untagged data)", out.len());
        out.extend_from_slice(tagged.as_bytes());
        out.extend_from_slice(b"\r\n");
        self.io.write_all(&out).await?;
        Ok(if reply.close {
            Flow::Close
        } else {
            Flow::Continue
        })
    }

    async fn bye(&mut self, mut out: Vec<u8>, reason: &str) -> io::Result<Flow> {
        out.extend_from_slice(format!("* BYE {reason}\r\n").as_bytes());
        self.io.write_all(&out).await?;
        Ok(Flow::Close)
    }

    async fn tagged(
        &mut self,
        tag: &str,
        status: &str,
        code: Option<&str>,
        text: &str,
    ) -> io::Result<Flow> {
        let line = match code {
            Some(code) => format!("{tag} {status} [{code}] {text}\r\n"),
            None => format!("{tag} {status} {text}\r\n"),
        };
        self.io.write_all(line.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    /// Reports the differences between the session's view and the selected mailbox.
    fn sync(&mut self, out: &mut Vec<u8>, allow_expunge: bool) -> SyncOutcome {
        let (Some(user), Some(sel)) = (self.user.as_deref(), self.selected.as_mut()) else {
            return SyncOutcome::Ok;
        };
        let mut store = lock(&self.shared.store);
        let Some(mailbox) = store
            .mailbox_mut(user, &sel.mailbox)
            .filter(|m| m.selectable)
        else {
            return SyncOutcome::Bye("Selected mailbox was deleted, have to disconnect");
        };
        if mailbox.uid_validity != sel.uid_validity {
            return SyncOutcome::Bye("Mailbox UIDVALIDITY changed, have to disconnect");
        }
        if allow_expunge {
            let mut i = 0;
            while i < sel.view.len() {
                if mailbox.get(sel.view[i].uid).is_none() {
                    out.extend_from_slice(format!("* {} EXPUNGE\r\n", i + 1).as_bytes());
                    sel.view.remove(i);
                } else {
                    i += 1;
                }
            }
        }
        for (i, entry) in sel.view.iter_mut().enumerate() {
            let Some(message) = mailbox.get(entry.uid).filter(|m| m.flags != entry.flags) else {
                continue;
            };
            entry.flags.clone_from(&message.flags);
            out.extend_from_slice(
                format!("* {} FETCH (UID {} FLAGS ", i + 1, entry.uid).as_bytes(),
            );
            push_flag_list(out, &entry.flags, entry.recent);
            out.extend_from_slice(b")\r\n");
        }
        let start = mailbox.messages.partition_point(|m| m.uid <= sel.max_uid);
        if start < mailbox.messages.len() {
            for message in &mut mailbox.messages[start..] {
                let recent = !sel.read_only && std::mem::take(&mut message.recent);
                sel.view.push(ViewEntry {
                    uid: message.uid,
                    flags: message.flags.clone(),
                    recent,
                });
                sel.max_uid = message.uid;
            }
            let recent = sel.view.iter().filter(|e| e.recent).count();
            out.extend_from_slice(
                format!("* {} EXISTS\r\n* {recent} RECENT\r\n", sel.view.len()).as_bytes(),
            );
        }
        SyncOutcome::Ok
    }

    fn execute(&mut self, cmd: &Command, out: &mut Vec<u8>) -> Reply {
        let args = cmd.args.as_slice();
        match cmd.name.as_str() {
            "CAPABILITY" => {
                out.extend_from_slice(
                    format!("* CAPABILITY {}\r\n", self.capabilities()).as_bytes(),
                );
                return Reply::ok("CAPABILITY completed");
            }
            "NOOP" => return Reply::ok("NOOP completed"),
            "LOGOUT" => {
                self.selected = None;
                out.extend_from_slice(b"* BYE NM Mail test server logging out\r\n");
                return Reply {
                    close: true,
                    ..Reply::ok("LOGOUT completed")
                };
            }
            "ID" => {
                out.extend_from_slice(
                    format!(
                        "* ID (\"name\" \"nmmail-testserver\" \"vendor\" \"NM Mail\" \"version\" \"{}\")\r\n",
                        env!("CARGO_PKG_VERSION")
                    )
                    .as_bytes(),
                );
                return Reply::ok("ID completed");
            }
            "STARTTLS" => return Reply::bad("STARTTLS is not supported by the test server"),
            _ => {}
        }
        let Some(user) = self.user.clone() else {
            return match cmd.name.as_str() {
                "LOGIN" => self.login(args),
                name if KNOWN_COMMANDS.contains(&name) => Reply::bad("Not authenticated"),
                name => Reply::bad(format!("Unknown command {name}")),
            };
        };
        match cmd.name.as_str() {
            "LOGIN" => Reply::bad("Already authenticated"),
            "SELECT" => self.select(&user, args, false, out),
            "EXAMINE" => self.select(&user, args, true, out),
            "CREATE" => self.create(&user, args),
            "DELETE" => self.delete(&user, args),
            "RENAME" => self.rename(&user, args),
            "SUBSCRIBE" | "UNSUBSCRIBE" => Reply::ok(format!("{} completed", cmd.name)),
            "LIST" => self.list(&user, args, "LIST", out),
            "LSUB" => self.list(&user, args, "LSUB", out),
            "STATUS" => self.status(&user, args, out),
            "APPEND" => self.append(&user, args),
            "NAMESPACE" => {
                out.extend_from_slice(b"* NAMESPACE ((\"\" \"/\")) NIL NIL\r\n");
                Reply::ok("NAMESPACE completed")
            }
            name if KNOWN_COMMANDS.contains(&name) => {
                if self.selected.is_none() {
                    return Reply::bad("No mailbox selected");
                }
                self.selected_command(&user, cmd, out)
            }
            name => Reply::bad(format!("Unknown command {name}")),
        }
    }

    fn selected_command(&mut self, user: &str, cmd: &Command, out: &mut Vec<u8>) -> Reply {
        let (name, args, uid_mode) = if cmd.name == "UID" {
            match cmd.args.split_first() {
                Some((Arg::Atom(sub), rest)) => (sub.to_ascii_uppercase(), rest, true),
                _ => return Reply::bad("Missing UID command"),
            }
        } else {
            (cmd.name.clone(), cmd.args.as_slice(), false)
        };
        let reply = match (name.as_str(), uid_mode) {
            ("CHECK", false) => Reply::ok("CHECK completed"),
            ("CLOSE", false) => self.close(user, true),
            ("UNSELECT", false) => self.close(user, false),
            ("EXPUNGE", _) => self.expunge(user, args, uid_mode),
            ("SEARCH", _) => self.search(user, args, uid_mode, out),
            ("FETCH", _) => self.fetch(user, args, uid_mode, out),
            ("STORE", _) => self.store(user, args, uid_mode, out),
            ("COPY", _) => self.copy(user, args, uid_mode, false, out),
            ("MOVE", _) => self.copy(user, args, uid_mode, true, out),
            _ if uid_mode => return Reply::bad(format!("Unknown command UID {name}")),
            _ => return Reply::bad(format!("Unknown command {name}")),
        };
        // RFC 3501 7.4.1: no EXPUNGE responses during FETCH, STORE and SEARCH.
        let sequence_command = !uid_mode && matches!(name.as_str(), "FETCH" | "STORE" | "SEARCH");
        Reply {
            allow_expunge: reply.allow_expunge && !sequence_command,
            ..reply
        }
    }

    fn login(&mut self, args: &[Arg]) -> Reply {
        let [user, password] = args else {
            return Reply::bad("LOGIN expects a user name and a password");
        };
        let (Some(user), Some(password)) = (user.string(), password.string()) else {
            return Reply::bad("LOGIN expects a user name and a password");
        };
        match self.shared.check_password(&user, &password) {
            Some(key) => {
                info!(user = %key, "IMAP login");
                self.user = Some(key);
                Reply::ok("Logged in").with_code(format!("CAPABILITY {}", self.capabilities()))
            }
            None => Reply::no("Invalid credentials (Failure)").with_code("AUTHENTICATIONFAILED"),
        }
    }

    async fn authenticate(&mut self, cmd: &Command) -> io::Result<Flow> {
        let tag = cmd.tag.as_str();
        if self.user.is_some() {
            return self.tagged(tag, "BAD", None, "Already authenticated").await;
        }
        let mechanism = cmd
            .args
            .first()
            .and_then(Arg::atom)
            .map(str::to_ascii_uppercase)
            .unwrap_or_default();
        let xoauth2 = match mechanism.as_str() {
            "PLAIN" => false,
            "XOAUTH2" if self.shared.xoauth2_token.is_some() => true,
            _ => {
                return self
                    .tagged(tag, "NO", None, "Unsupported authentication mechanism")
                    .await;
            }
        };
        let response = match cmd.args.get(1).and_then(Arg::string) {
            Some(initial) => initial,
            None => {
                self.io.write_all(b"+ \r\n").await?;
                let line = self.io.read_line(MAX_LINE).await?;
                String::from_utf8_lossy(&line).into_owned()
            }
        };
        let response = response.trim();
        if response == "*" {
            return self
                .tagged(tag, "BAD", None, "AUTHENTICATE cancelled")
                .await;
        }
        let decoded = if response == "=" {
            Vec::new()
        } else {
            match BASE64.decode(response) {
                Ok(decoded) => decoded,
                Err(_) => return self.tagged(tag, "BAD", None, "Invalid base64 data").await,
            }
        };
        let user = if xoauth2 {
            parse_xoauth2(&decoded).and_then(|(user, token)| self.shared.check_token(&user, &token))
        } else {
            parse_plain(&decoded)
                .and_then(|(user, password)| self.shared.check_password(&user, &password))
        };
        match user {
            Some(key) => {
                info!(user = %key, mechanism = %mechanism, "IMAP authenticated");
                self.user = Some(key);
                let code = format!("CAPABILITY {}", self.capabilities());
                self.tagged(tag, "OK", Some(&code), "Authentication successful")
                    .await
            }
            None => {
                if xoauth2 {
                    // Like Gmail: an error challenge, then NO once the client answers.
                    let challenge = BASE64.encode(XOAUTH2_ERROR);
                    self.io
                        .write_all(format!("+ {challenge}\r\n").as_bytes())
                        .await?;
                    self.io.read_line(MAX_LINE).await?;
                }
                self.tagged(
                    tag,
                    "NO",
                    Some("AUTHENTICATIONFAILED"),
                    "Invalid credentials (Failure)",
                )
                .await
            }
        }
    }

    async fn idle(&mut self, cmd: &Command) -> io::Result<Flow> {
        let tag = cmd.tag.clone();
        if self.user.is_none() {
            return self.tagged(&tag, "BAD", None, "Not authenticated").await;
        }
        self.io.write_all(b"+ idling\r\n").await?;
        loop {
            self.changes.mark_unchanged();
            let mut out = Vec::new();
            if let SyncOutcome::Bye(reason) = self.sync(&mut out, true) {
                return self.bye(out, reason).await;
            }
            if !out.is_empty() {
                self.io.write_all(&out).await?;
            }
            tokio::select! {
                line = self.io.read_line(MAX_LINE) => {
                    let line = line?;
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    debug!("C: {text}");
                    self.shared.log_command(text.chars().take(2000).collect());
                    let mut out = Vec::new();
                    if let SyncOutcome::Bye(reason) = self.sync(&mut out, true) {
                        return self.bye(out, reason).await;
                    }
                    if text.eq_ignore_ascii_case("DONE") {
                        out.extend_from_slice(format!("{tag} OK IDLE terminated\r\n").as_bytes());
                    } else {
                        out.extend_from_slice(format!("{tag} BAD Expected DONE\r\n").as_bytes());
                    }
                    self.io.write_all(&out).await?;
                    return Ok(Flow::Continue);
                }
                res = self.changes.changed() => {
                    if res.is_err() {
                        return Err(conn::killed_error());
                    }
                }
            }
        }
    }

    fn select(&mut self, user: &str, args: &[Arg], read_only: bool, out: &mut Vec<u8>) -> Reply {
        self.selected = None;
        let Some(name) = args.first().and_then(Arg::string) else {
            return Reply::bad("Missing mailbox name");
        };
        let mut store = lock(&self.shared.store);
        let Some(mailbox) = store.mailbox_mut(user, &name) else {
            return Reply::no("Mailbox doesn't exist").with_code("NONEXISTENT");
        };
        if !mailbox.selectable {
            return Reply::no("Mailbox is not selectable");
        }
        let view: Vec<ViewEntry> = mailbox
            .messages
            .iter_mut()
            .map(|m| ViewEntry {
                uid: m.uid,
                flags: m.flags.clone(),
                recent: !read_only && std::mem::take(&mut m.recent),
            })
            .collect();
        let mut keywords: Vec<String> = mailbox
            .messages
            .iter()
            .flat_map(|m| m.flags.iter())
            .filter(|f| !f.starts_with('\\'))
            .cloned()
            .collect();
        sort_flags(&mut keywords);
        let selected = Selected {
            mailbox: mailbox.name.clone(),
            read_only,
            uid_validity: mailbox.uid_validity,
            max_uid: mailbox.messages.last().map_or(0, |m| m.uid),
            view,
        };
        let uid_next = mailbox.uid_next;
        drop(store);

        let mut flags = SYSTEM_FLAGS.join(" ");
        for keyword in &keywords {
            flags.push(' ');
            flags.push_str(keyword);
        }
        let recent = selected.view.iter().filter(|e| e.recent).count();
        let mut text = format!(
            "* FLAGS ({flags})\r\n* {} EXISTS\r\n* {recent} RECENT\r\n",
            selected.view.len()
        );
        if let Some(i) = selected
            .view
            .iter()
            .position(|e| !has_flag(&e.flags, "\\Seen"))
        {
            text.push_str(&format!("* OK [UNSEEN {}] First unseen\r\n", i + 1));
        }
        text.push_str(&format!(
            "* OK [UIDVALIDITY {}] UIDs valid\r\n* OK [UIDNEXT {uid_next}] Predicted next UID\r\n",
            selected.uid_validity
        ));
        if read_only {
            text.push_str("* OK [PERMANENTFLAGS ()] Read-only mailbox\r\n");
        } else {
            text.push_str(&format!(
                "* OK [PERMANENTFLAGS ({flags} \\*)] Flags permitted\r\n"
            ));
        }
        out.extend_from_slice(text.as_bytes());
        self.selected = Some(selected);
        if read_only {
            Reply::ok("EXAMINE completed").with_code("READ-ONLY")
        } else {
            Reply::ok("SELECT completed").with_code("READ-WRITE")
        }
    }

    fn create(&mut self, user: &str, args: &[Arg]) -> Reply {
        let Some(name) = args.first().and_then(Arg::string) else {
            return Reply::bad("Missing mailbox name");
        };
        let result = lock(&self.shared.store).create_mailbox(user, &name);
        match result {
            Ok(()) => Reply::ok("CREATE completed"),
            Err(StoreError::AlreadyExists) => {
                Reply::no("Mailbox already exists").with_code("ALREADYEXISTS")
            }
            Err(err) => Reply::no(format!("Cannot create mailbox: {err}")),
        }
    }

    fn delete(&mut self, user: &str, args: &[Arg]) -> Reply {
        let Some(name) = args.first().and_then(Arg::string) else {
            return Reply::bad("Missing mailbox name");
        };
        if self.selected.as_ref().is_some_and(|s| s.mailbox == name) {
            self.selected = None;
        }
        let result = lock(&self.shared.store).delete_mailbox(user, &name);
        match result {
            Ok(()) => {
                self.shared.notify();
                Reply::ok("DELETE completed")
            }
            Err(StoreError::NoSuchMailbox) => {
                Reply::no("Mailbox doesn't exist").with_code("NONEXISTENT")
            }
            Err(err) => Reply::no(format!("Cannot delete mailbox: {err}")),
        }
    }

    fn rename(&mut self, user: &str, args: &[Arg]) -> Reply {
        let [from, to] = args else {
            return Reply::bad("RENAME expects two mailbox names");
        };
        let (Some(from), Some(to)) = (from.string(), to.string()) else {
            return Reply::bad("RENAME expects two mailbox names");
        };
        let result = lock(&self.shared.store).rename_mailbox(user, &from, &to);
        match result {
            Ok(()) => {
                if let Some(sel) = self.selected.as_mut() {
                    let to = to.trim_end_matches('/');
                    if sel.mailbox == from {
                        sel.mailbox = to.to_string();
                    } else if let Some(rest) = sel.mailbox.strip_prefix(&format!("{from}/")) {
                        sel.mailbox = format!("{to}/{rest}");
                    }
                }
                self.shared.notify();
                Reply::ok("RENAME completed")
            }
            Err(StoreError::NoSuchMailbox) => {
                Reply::no("Mailbox doesn't exist").with_code("NONEXISTENT")
            }
            Err(StoreError::AlreadyExists) => {
                Reply::no("Mailbox already exists").with_code("ALREADYEXISTS")
            }
            Err(err) => Reply::no(format!("Cannot rename mailbox: {err}")),
        }
    }

    fn list(&mut self, user: &str, args: &[Arg], verb: &str, out: &mut Vec<u8>) -> Reply {
        // LIST [(selection options)] reference pattern(s) [RETURN (...)]
        let mut i = 0;
        let mut special_only = false;
        if let Some(Arg::List(options)) = args.first() {
            special_only = options.iter().any(|o| o.is_atom("SPECIAL-USE"));
            i = 1;
        }
        let Some(reference) = args.get(i).and_then(Arg::string) else {
            return Reply::bad("Missing reference name");
        };
        let patterns: Vec<String> = match args.get(i + 1) {
            Some(Arg::List(items)) => items.iter().filter_map(Arg::string).collect(),
            Some(arg) => arg.string().into_iter().collect(),
            None => return Reply::bad("Missing mailbox pattern"),
        };
        if patterns.len() == 1 && patterns[0].is_empty() {
            out.extend_from_slice(format!("* {verb} (\\Noselect) \"/\" \"\"\r\n").as_bytes());
            return Reply::ok(format!("{verb} completed"));
        }
        let store = lock(&self.shared.store);
        let Some(account) = store.user(user) else {
            return Reply::no("Unknown user");
        };
        for mailbox in &account.mailboxes {
            if special_only && mailbox.special_use.is_none() {
                continue;
            }
            if !patterns
                .iter()
                .any(|p| list_match(&format!("{reference}{p}"), &mailbox.name))
            {
                continue;
            }
            let mut attributes = vec![if account.has_children(&mailbox.name) {
                "\\HasChildren"
            } else {
                "\\HasNoChildren"
            }];
            if !mailbox.selectable {
                attributes.push("\\Noselect");
            }
            if let Some(special_use) = mailbox.special_use {
                attributes.push(special_use.attribute());
            }
            out.extend_from_slice(format!("* {verb} ({}) \"/\" ", attributes.join(" ")).as_bytes());
            push_astring(out, &mailbox.name);
            out.extend_from_slice(b"\r\n");
        }
        Reply::ok(format!("{verb} completed"))
    }

    fn status(&mut self, user: &str, args: &[Arg], out: &mut Vec<u8>) -> Reply {
        let (Some(name), Some(Arg::List(items))) =
            (args.first().and_then(Arg::string), args.get(1))
        else {
            return Reply::bad("STATUS expects a mailbox name and a list of items");
        };
        let store = lock(&self.shared.store);
        let Some(mailbox) = store.mailbox(user, &name) else {
            return Reply::no("Mailbox doesn't exist").with_code("NONEXISTENT");
        };
        if !mailbox.selectable {
            return Reply::no("Mailbox is not selectable");
        }
        let mut values = Vec::new();
        for item in items {
            let Some(atom) = item.atom() else {
                return Reply::bad("Invalid STATUS item");
            };
            let item = atom.to_ascii_uppercase();
            let value = match item.as_str() {
                "MESSAGES" => mailbox.messages.len(),
                "RECENT" => mailbox.recent_count(),
                "UIDNEXT" => mailbox.uid_next as usize,
                "UIDVALIDITY" => mailbox.uid_validity as usize,
                "UNSEEN" => mailbox.unseen_count(),
                _ => return Reply::bad(format!("Unsupported STATUS item {atom}")),
            };
            values.push(format!("{item} {value}"));
        }
        drop(store);
        out.extend_from_slice(b"* STATUS ");
        push_astring(out, &name);
        out.extend_from_slice(format!(" ({})\r\n", values.join(" ")).as_bytes());
        Reply::ok("STATUS completed")
    }

    fn append(&mut self, user: &str, args: &[Arg]) -> Reply {
        let Some((name, mut rest)) = args.split_first() else {
            return Reply::bad("APPEND expects a mailbox name and a message");
        };
        let Some(name) = name.string() else {
            return Reply::bad("Invalid mailbox name");
        };
        let mut flags = Vec::new();
        if let Some((Arg::List(items), tail)) = rest.split_first() {
            for item in items {
                let Some(atom) = item.atom() else {
                    return Reply::bad("Invalid flag");
                };
                flags.extend(normalize_flag(atom));
            }
            rest = tail;
        }
        sort_flags(&mut flags);
        let mut internal_date = None;
        if let [Arg::Str(date), _] = rest {
            match proto::parse_date_time(&String::from_utf8_lossy(date)) {
                Some(date) => internal_date = Some(date),
                None => return Reply::bad("Invalid date-time"),
            }
            rest = &rest[1..];
        }
        let [Arg::Str(message)] = rest else {
            return Reply::bad("APPEND expects a message literal");
        };
        let result = lock(&self.shared.store).append(
            user,
            &name,
            message.clone(),
            &flags,
            internal_date,
            true,
        );
        match result {
            Ok((validity, uid)) => {
                self.shared.notify();
                Reply::ok("APPEND completed").with_code(format!("APPENDUID {validity} {uid}"))
            }
            Err(StoreError::NoSuchMailbox) => {
                Reply::no("Mailbox doesn't exist").with_code("TRYCREATE")
            }
            Err(err) => Reply::no(format!("Cannot append: {err}")),
        }
    }

    fn close(&mut self, user: &str, expunge: bool) -> Reply {
        let Some(sel) = self.selected.take() else {
            return Reply::bad("No mailbox selected");
        };
        if expunge && !sel.read_only {
            let removed = {
                let mut store = lock(&self.shared.store);
                let uids: Vec<u32> = store
                    .mailbox(user, &sel.mailbox)
                    .map(|m| {
                        m.messages
                            .iter()
                            .filter(|msg| has_flag(&msg.flags, "\\Deleted"))
                            .map(|msg| msg.uid)
                            .collect()
                    })
                    .unwrap_or_default();
                store.remove_uids(user, &sel.mailbox, &uids)
            };
            if removed > 0 {
                self.shared.notify();
            }
        }
        Reply::ok(if expunge {
            "CLOSE completed"
        } else {
            "UNSELECT completed"
        })
    }

    fn expunge(&mut self, user: &str, args: &[Arg], uid_mode: bool) -> Reply {
        let uid_set = match (uid_mode, args) {
            (false, []) => None,
            (true, [set]) => match set.atom().and_then(SeqSet::parse) {
                Some(set) => Some(set),
                None => return Reply::bad("Invalid UID set"),
            },
            _ => return Reply::bad("Invalid EXPUNGE arguments"),
        };
        let Some(sel) = self.selected.as_ref() else {
            return Reply::bad("No mailbox selected");
        };
        if sel.read_only {
            return Reply::no("Mailbox is read-only");
        }
        let max_uid = sel.view.last().map_or(0, |e| e.uid);
        let removed = {
            let mut store = lock(&self.shared.store);
            let uids: Vec<u32> = store
                .mailbox(user, &sel.mailbox)
                .map(|m| {
                    m.messages
                        .iter()
                        .filter(|msg| has_flag(&msg.flags, "\\Deleted"))
                        .filter(|msg| {
                            uid_set
                                .as_ref()
                                .is_none_or(|s| s.contains(msg.uid, max_uid))
                        })
                        .map(|msg| msg.uid)
                        .collect()
                })
                .unwrap_or_default();
            store.remove_uids(user, &sel.mailbox, &uids)
        };
        if removed > 0 {
            self.shared.notify();
        }
        Reply::ok("EXPUNGE completed")
    }

    fn search(&mut self, user: &str, args: &[Arg], uid_mode: bool, out: &mut Vec<u8>) -> Reply {
        let key = match search::parse_search(args, self.shared.gmail_mode) {
            Ok(key) => key,
            Err(err) => return Reply::bad(err),
        };
        let Some(sel) = self.selected.as_ref() else {
            return Reply::bad("No mailbox selected");
        };
        let limits = Limits {
            max_seq: u32::try_from(sel.view.len()).unwrap_or(u32::MAX),
            max_uid: sel.view.last().map_or(0, |e| e.uid),
        };
        let mut hits = Vec::new();
        {
            let store = lock(&self.shared.store);
            let Some(mailbox) = store.mailbox(user, &sel.mailbox) else {
                return Reply::no("Selected mailbox no longer exists");
            };
            for (i, entry) in sel.view.iter().enumerate() {
                let Some(message) = mailbox.get(entry.uid) else {
                    continue;
                };
                let seq = u32::try_from(i + 1).unwrap_or(u32::MAX);
                let candidate = Candidate {
                    seq,
                    uid: entry.uid,
                    flags: &message.flags,
                    recent: entry.recent,
                    data: &message.data,
                };
                if search::matches(&key, &candidate, &limits) {
                    hits.push(if uid_mode { entry.uid } else { seq });
                }
            }
        }
        out.extend_from_slice(b"* SEARCH");
        for hit in hits {
            out.extend_from_slice(format!(" {hit}").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        Reply::ok("SEARCH completed")
    }

    fn fetch(&mut self, user: &str, args: &[Arg], uid_mode: bool, out: &mut Vec<u8>) -> Reply {
        let Some((set, rest)) = args.split_first() else {
            return Reply::bad("FETCH expects a sequence set and attributes");
        };
        let Some(set) = set.atom().and_then(SeqSet::parse) else {
            return Reply::bad("Invalid sequence set");
        };
        let atts = match fetch::parse_fetch_atts(rest, self.shared.gmail_mode) {
            Ok(atts) => atts,
            Err(err) => return Reply::bad(err),
        };
        let Some(sel) = self.selected.as_mut() else {
            return Reply::bad("No mailbox selected");
        };
        let sets_seen = !sel.read_only && atts.iter().any(FetchAtt::sets_seen);
        let wants_labels = atts.contains(&FetchAtt::GmLabels);
        let reports_flags = atts.contains(&FetchAtt::Flags);
        let targets = sel.targets(&set, uid_mode);

        let mut items: Vec<Fetched> = Vec::new();
        {
            let mut store = lock(&self.shared.store);
            let Some(mailbox) = store.mailbox_mut(user, &sel.mailbox) else {
                return Reply::no("Selected mailbox no longer exists");
            };
            for (seq, uid) in targets {
                // Messages expunged by another session are skipped until reported.
                let Some(message) = mailbox.get_mut(uid) else {
                    continue;
                };
                let mut seen_set = false;
                if sets_seen && !has_flag(&message.flags, "\\Seen") {
                    message.flags.push("\\Seen".to_string());
                    sort_flags(&mut message.flags);
                    seen_set = true;
                }
                items.push(Fetched {
                    seq,
                    uid,
                    flags: message.flags.clone(),
                    data: message.data.clone(),
                    seen_set,
                    labels: Vec::new(),
                });
            }
            if wants_labels {
                for item in &mut items {
                    item.labels = store.gm_labels(user, item.data.gm_msgid, &item.flags);
                }
            }
        }
        if items.iter().any(|item| item.seen_set) {
            self.shared.notify();
        }
        for item in &items {
            let recent = match sel.entry_mut(item.seq, item.uid) {
                Some(entry) => {
                    if reports_flags || item.seen_set {
                        entry.flags.clone_from(&item.flags);
                    }
                    entry.recent
                }
                None => false,
            };
            let view = FetchItem {
                seq: item.seq,
                uid: item.uid,
                flags: &item.flags,
                recent,
                data: &item.data,
                labels: &item.labels,
            };
            fetch::render(out, &view, &atts, uid_mode, item.seen_set);
        }
        Reply::ok("FETCH completed")
    }

    fn store(&mut self, user: &str, args: &[Arg], uid_mode: bool, out: &mut Vec<u8>) -> Reply {
        enum Op {
            Replace,
            Add,
            Remove,
        }
        let [set, item, values @ ..] = args else {
            return Reply::bad("STORE expects a sequence set, an item and flags");
        };
        let Some(set) = set.atom().and_then(SeqSet::parse) else {
            return Reply::bad("Invalid sequence set");
        };
        let Some(item) = item.atom() else {
            return Reply::bad("Invalid STORE item");
        };
        let (op, silent) = match item.to_ascii_uppercase().as_str() {
            "FLAGS" => (Op::Replace, false),
            "FLAGS.SILENT" => (Op::Replace, true),
            "+FLAGS" => (Op::Add, false),
            "+FLAGS.SILENT" => (Op::Add, true),
            "-FLAGS" => (Op::Remove, false),
            "-FLAGS.SILENT" => (Op::Remove, true),
            _ => return Reply::bad(format!("Unsupported STORE item {item}")),
        };
        let values: Vec<&Arg> = match values {
            [] => return Reply::bad("STORE expects a flag list"),
            [Arg::List(items)] => items.iter().collect(),
            _ => values.iter().collect(),
        };
        let mut flags = Vec::new();
        for value in values {
            let Some(atom) = value.atom() else {
                return Reply::bad("Invalid flag");
            };
            // \Recent and unknown system flags cannot be stored: ignored.
            flags.extend(normalize_flag(atom));
        }
        let Some(sel) = self.selected.as_mut() else {
            return Reply::bad("No mailbox selected");
        };
        if sel.read_only {
            return Reply::no("Mailbox is read-only");
        }
        let targets = sel.targets(&set, uid_mode);
        let mut updated = Vec::new();
        let mut changed = false;
        {
            let mut store = lock(&self.shared.store);
            let Some(mailbox) = store.mailbox_mut(user, &sel.mailbox) else {
                return Reply::no("Selected mailbox no longer exists");
            };
            for (seq, uid) in targets {
                let Some(message) = mailbox.get_mut(uid) else {
                    continue;
                };
                let mut new_flags = match op {
                    Op::Replace => flags.clone(),
                    Op::Add => message.flags.iter().chain(&flags).cloned().collect(),
                    Op::Remove => message
                        .flags
                        .iter()
                        .filter(|f| !has_flag(&flags, f))
                        .cloned()
                        .collect(),
                };
                sort_flags(&mut new_flags);
                if new_flags != message.flags {
                    message.flags.clone_from(&new_flags);
                    changed = true;
                }
                updated.push((seq, uid, new_flags));
            }
        }
        if changed {
            self.shared.notify();
        }
        for (seq, uid, new_flags) in updated {
            let recent = match sel.entry_mut(seq, uid) {
                Some(entry) => {
                    entry.flags.clone_from(&new_flags);
                    entry.recent
                }
                None => false,
            };
            if !silent {
                out.extend_from_slice(format!("* {seq} FETCH (").as_bytes());
                if uid_mode {
                    out.extend_from_slice(format!("UID {uid} ").as_bytes());
                }
                out.extend_from_slice(b"FLAGS ");
                push_flag_list(out, &new_flags, recent);
                out.extend_from_slice(b")\r\n");
            }
        }
        Reply::ok("STORE completed")
    }

    fn copy(
        &mut self,
        user: &str,
        args: &[Arg],
        uid_mode: bool,
        is_move: bool,
        out: &mut Vec<u8>,
    ) -> Reply {
        let verb = if is_move { "MOVE" } else { "COPY" };
        let [set, destination] = args else {
            return Reply::bad(format!("{verb} expects a sequence set and a mailbox"));
        };
        let Some(set) = set.atom().and_then(SeqSet::parse) else {
            return Reply::bad("Invalid sequence set");
        };
        let Some(destination) = destination.string() else {
            return Reply::bad("Invalid mailbox name");
        };
        let Some(sel) = self.selected.as_ref() else {
            return Reply::bad("No mailbox selected");
        };
        if is_move && sel.read_only {
            return Reply::no("Mailbox is read-only");
        }
        let uids: Vec<u32> = sel
            .targets(&set, uid_mode)
            .into_iter()
            .map(|(_, uid)| uid)
            .collect();
        let result = {
            let mut store = lock(&self.shared.store);
            store
                .copy(user, &sel.mailbox, &uids, &destination)
                .inspect(|(_, pairs)| {
                    if is_move {
                        let moved: Vec<u32> = pairs.iter().map(|p| p.0).collect();
                        store.remove_uids(user, &sel.mailbox, &moved);
                    }
                })
        };
        match result {
            Ok((_, pairs)) if pairs.is_empty() => {
                Reply::ok(format!("{verb} completed (no messages)"))
            }
            Ok((validity, pairs)) => {
                self.shared.notify();
                let source: Vec<u32> = pairs.iter().map(|p| p.0).collect();
                let target: Vec<u32> = pairs.iter().map(|p| p.1).collect();
                let code = format!(
                    "COPYUID {validity} {} {}",
                    proto::format_uid_set(&source),
                    proto::format_uid_set(&target)
                );
                if is_move {
                    out.extend_from_slice(format!("* OK [{code}] Moved\r\n").as_bytes());
                    Reply::ok("MOVE completed")
                } else {
                    Reply::ok("COPY completed").with_code(code)
                }
            }
            Err(StoreError::NoSuchMailbox) => {
                Reply::no("Destination mailbox doesn't exist").with_code("TRYCREATE")
            }
            Err(err) => Reply::no(format!("Cannot {verb}: {err}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_patterns() {
        assert!(list_match("*", "INBOX"));
        assert!(list_match("*", "[Gmail]/All Mail"));
        assert!(list_match("%", "INBOX"));
        assert!(!list_match("%", "[Gmail]/All Mail"));
        assert!(list_match("[Gmail]/%", "[Gmail]/All Mail"));
        assert!(list_match("inbox", "INBOX"));
        assert!(list_match("S*t", "Sent"));
        assert!(!list_match("Sent", "Sent/Old"));
        assert!(!list_match("", "INBOX"));
    }

    #[test]
    fn sasl_payloads() {
        assert_eq!(
            parse_xoauth2(b"user=a@b.c\x01auth=Bearer tok\x01\x01"),
            Some(("a@b.c".into(), "tok".into()))
        );
        assert_eq!(parse_xoauth2(b"user=a@b.c\x01auth=Basic tok\x01\x01"), None);
        assert_eq!(
            parse_plain(b"\0a@b.c\0pw"),
            Some(("a@b.c".into(), "pw".into()))
        );
        assert_eq!(
            parse_plain(b"a@b.c\0\0pw"),
            Some(("a@b.c".into(), "pw".into()))
        );
        assert_eq!(parse_plain(b"garbage"), None);
    }
}

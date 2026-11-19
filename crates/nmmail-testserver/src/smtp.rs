//! SMTP server: EHLO/HELO, AUTH PLAIN/LOGIN/XOAUTH2, MAIL, RCPT, DATA, RSET, NOOP, QUIT.
//!
//! Accepted messages are delivered to the INBOX of every recipient that is a known user
//! (unknown recipients are accepted and discarded) and a `\Seen` copy is appended to the
//! Sent mailbox of the authenticated user, as Gmail and Outlook do server-side.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio::net::TcpStream;
use tracing::{debug, info};

use crate::conn::{self, ConnIo, ConnKind};
use crate::imap::parse_xoauth2;
use crate::store::{SpecialUse, normalize_flags};
use crate::{Shared, lock};

const HOSTNAME: &str = "nmmail.local";
/// Command lines (RFC 5321 allows 512 bytes; be lenient).
const MAX_LINE: usize = 64 * 1024;
/// Message lines inside DATA (RFC 5321 allows 1000 bytes; be lenient).
const MAX_DATA_LINE: usize = 1024 * 1024;
const MAX_MESSAGE: usize = 50 * 1024 * 1024;
const MAX_RECIPIENTS: usize = 100;
const XOAUTH2_ERROR: &str =
    r#"{"status":"400","schemes":"Bearer","scope":"https://mail.google.com/"}"#;

pub(crate) async fn handle_connection(stream: TcpStream, peer: SocketAddr, shared: Arc<Shared>) {
    let (guard, ctl) = conn::register(&shared, ConnKind::Smtp);
    let mut session = SmtpSession {
        shared,
        io: ConnIo::new(stream, ctl),
        user: None,
        mail_from: None,
        recipients: Vec::new(),
    };
    debug!(%peer, "SMTP connection opened");
    match session.serve().await {
        Ok(()) => debug!(%peer, "SMTP connection closed"),
        Err(err) if conn::is_killed_error(&err) => {
            debug!(%peer, "SMTP connection dropped by the test server");
            session.io.abort();
        }
        Err(err) => debug!(%peer, "SMTP connection closed: {err}"),
    }
    drop(guard);
}

struct SmtpSession {
    shared: Arc<Shared>,
    io: ConnIo,
    /// Key of the authenticated user.
    user: Option<String>,
    mail_from: Option<String>,
    recipients: Vec<String>,
}

/// Extracts the address of `FROM:<a@b> PARAMS` / `TO:<a@b>`.
fn parse_path<'a>(args: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let head = args.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = args[prefix.len()..].trim_start();
    if let Some(inner) = rest.strip_prefix('<') {
        let end = inner.find('>')?;
        Some((inner[..end].trim(), inner[end + 1..].trim()))
    } else {
        let (address, params) = rest.split_once(' ').unwrap_or((rest, ""));
        Some((address.trim(), params.trim()))
    }
}

impl SmtpSession {
    async fn reply(&mut self, text: &str) -> io::Result<()> {
        self.io.write_all(format!("{text}\r\n").as_bytes()).await
    }

    async fn read_text_line(&mut self) -> io::Result<String> {
        let line = self.io.read_line(MAX_LINE).await?;
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    async fn serve(&mut self) -> io::Result<()> {
        self.reply(&format!("220 {HOSTNAME} ESMTP NM Mail test server ready"))
            .await?;
        loop {
            let line = match self.read_text_line().await {
                Ok(line) => line,
                Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                    let _ = self.reply("500 5.5.2 Line too long").await;
                    return Ok(());
                }
                Err(err) => return Err(err),
            };
            let (verb, args) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
            let verb = verb.to_ascii_uppercase();
            let args = args.trim().to_string();
            debug!(
                "SMTP C: {}",
                if verb == "AUTH" {
                    "AUTH ***"
                } else {
                    line.trim()
                }
            );
            match verb.as_str() {
                "EHLO" => {
                    self.reset();
                    let mechanisms = if self.shared.xoauth2_token.is_some() {
                        "PLAIN LOGIN XOAUTH2"
                    } else {
                        "PLAIN LOGIN"
                    };
                    let text = format!(
                        "250-{HOSTNAME} Hello {args}\r\n250-SIZE {MAX_MESSAGE}\r\n250-8BITMIME\r\n\
                         250-PIPELINING\r\n250-ENHANCEDSTATUSCODES\r\n250 AUTH {mechanisms}"
                    );
                    self.reply(&text).await?;
                }
                "HELO" => {
                    self.reset();
                    self.reply(&format!("250 {HOSTNAME}")).await?;
                }
                "AUTH" => self.auth(&args).await?,
                "MAIL" => self.mail(&args).await?,
                "RCPT" => self.rcpt(&args).await?,
                "DATA" => self.data().await?,
                "RSET" => {
                    self.reset();
                    self.reply("250 2.0.0 OK").await?;
                }
                "NOOP" => self.reply("250 2.0.0 OK").await?,
                "VRFY" => self.reply("252 2.1.5 Cannot VRFY user").await?,
                "QUIT" => {
                    self.reply("221 2.0.0 Bye").await?;
                    return Ok(());
                }
                "STARTTLS" => self.reply("502 5.5.1 STARTTLS not supported").await?,
                _ => self.reply("500 5.5.2 Unrecognized command").await?,
            }
        }
    }

    fn reset(&mut self) {
        self.mail_from = None;
        self.recipients.clear();
    }

    async fn auth(&mut self, args: &str) -> io::Result<()> {
        if self.user.is_some() {
            return self.reply("503 5.5.1 Already authenticated").await;
        }
        let (mechanism, initial) = args.split_once(' ').unwrap_or((args, ""));
        let mechanism = mechanism.to_ascii_uppercase();
        let initial = initial.trim();
        let user = match mechanism.as_str() {
            "PLAIN" => {
                let Some(data) = self.sasl_response(initial, "").await? else {
                    return Ok(());
                };
                let text = String::from_utf8_lossy(&data).into_owned();
                let parts: Vec<&str> = text.split('\0').collect();
                match parts.as_slice() {
                    [authzid, authcid, password] => {
                        let user = if authcid.is_empty() { authzid } else { authcid };
                        self.shared.check_password(user, password)
                    }
                    _ => None,
                }
            }
            "LOGIN" => {
                let Some(user) = self.sasl_response(initial, "VXNlcm5hbWU6").await? else {
                    return Ok(());
                };
                let Some(password) = self.sasl_response("", "UGFzc3dvcmQ6").await? else {
                    return Ok(());
                };
                self.shared.check_password(
                    &String::from_utf8_lossy(&user),
                    &String::from_utf8_lossy(&password),
                )
            }
            "XOAUTH2" if self.shared.xoauth2_token.is_some() => {
                let Some(data) = self.sasl_response(initial, "").await? else {
                    return Ok(());
                };
                let user = parse_xoauth2(&data)
                    .and_then(|(user, token)| self.shared.check_token(&user, &token));
                if user.is_none() {
                    // Like Gmail: an error challenge, then 535 once the client answers.
                    self.reply(&format!("334 {}", BASE64.encode(XOAUTH2_ERROR)))
                        .await?;
                    self.read_text_line().await?;
                }
                user
            }
            _ => {
                return self
                    .reply("504 5.5.4 Unrecognized authentication type")
                    .await;
            }
        };
        match user {
            Some(key) => {
                info!(user = %key, mechanism = %mechanism, "SMTP authenticated");
                self.user = Some(key);
                self.reply("235 2.7.0 Authentication successful").await
            }
            None => {
                self.reply("535 5.7.8 Username and Password not accepted")
                    .await
            }
        }
    }

    /// Returns the decoded SASL response: the initial response if present, otherwise
    /// the answer to a `334 <challenge>`. `None` if the exchange was cancelled or
    /// invalid (the error reply has been sent).
    async fn sasl_response(
        &mut self,
        initial: &str,
        challenge: &str,
    ) -> io::Result<Option<Vec<u8>>> {
        let response = if initial.is_empty() {
            self.reply(&format!("334 {challenge}")).await?;
            self.read_text_line().await?.trim().to_string()
        } else {
            initial.to_string()
        };
        if response == "*" {
            self.reply("501 5.0.0 Authentication cancelled").await?;
            return Ok(None);
        }
        if response == "=" {
            return Ok(Some(Vec::new()));
        }
        match BASE64.decode(&response) {
            Ok(data) => Ok(Some(data)),
            Err(_) => {
                self.reply("501 5.5.2 Invalid base64 data").await?;
                Ok(None)
            }
        }
    }

    async fn mail(&mut self, args: &str) -> io::Result<()> {
        if self.user.is_none() {
            return self.reply("530 5.7.0 Authentication required").await;
        }
        if self.mail_from.is_some() {
            return self.reply("503 5.5.1 Sender already specified").await;
        }
        let Some((address, params)) = parse_path(args, "FROM:") else {
            return self.reply("501 5.5.4 Syntax: MAIL FROM:<address>").await;
        };
        let size = params
            .split_whitespace()
            .find_map(|p| {
                p.get(..5)
                    .filter(|k| k.eq_ignore_ascii_case("SIZE="))
                    .map(|_| &p[5..])
            })
            .and_then(|v| v.parse::<usize>().ok());
        if size.is_some_and(|s| s > MAX_MESSAGE) {
            return self
                .reply("552 5.3.4 Message size exceeds fixed limit")
                .await;
        }
        self.mail_from = Some(address.to_string());
        self.reply("250 2.1.0 OK").await
    }

    async fn rcpt(&mut self, args: &str) -> io::Result<()> {
        if self.mail_from.is_none() {
            return self.reply("503 5.5.1 Need MAIL command").await;
        }
        let Some((address, _)) = parse_path(args, "TO:") else {
            return self.reply("501 5.5.4 Syntax: RCPT TO:<address>").await;
        };
        if address.is_empty() {
            return self.reply("501 5.1.3 Invalid recipient").await;
        }
        if self.recipients.len() >= MAX_RECIPIENTS {
            return self.reply("452 4.5.3 Too many recipients").await;
        }
        self.recipients.push(address.to_string());
        self.reply("250 2.1.5 OK").await
    }

    async fn data(&mut self) -> io::Result<()> {
        if self.mail_from.is_none() {
            return self.reply("503 5.5.1 Need MAIL command").await;
        }
        if self.recipients.is_empty() {
            return self.reply("503 5.5.1 Need RCPT command").await;
        }
        self.reply("354 Go ahead, end data with <CR><LF>.<CR><LF>")
            .await?;
        let mut message = Vec::new();
        let mut too_big = false;
        loop {
            let line = self.io.read_line(MAX_DATA_LINE).await?;
            if line == b"." {
                break;
            }
            // Dot-unstuffing.
            let line = line.strip_prefix(b".").unwrap_or(&line);
            if message.len() + line.len() + 2 > MAX_MESSAGE {
                too_big = true;
                continue;
            }
            message.extend_from_slice(line);
            message.extend_from_slice(b"\r\n");
        }
        if too_big {
            self.reset();
            return self
                .reply("552 5.3.4 Message size exceeds fixed limit")
                .await;
        }
        let delivered = self.deliver(message);
        self.reset();
        self.reply(&format!(
            "250 2.0.0 OK: delivered to {delivered} local mailbox(es)"
        ))
        .await
    }

    /// Stores the message for local recipients and in the sender's Sent mailbox.
    fn deliver(&self, message: Vec<u8>) -> usize {
        let mut delivered = 0;
        {
            let mut store = lock(&self.shared.store);
            for recipient in &self.recipients {
                if store.user_key(recipient).is_some()
                    && store
                        .append(recipient, "INBOX", message.clone(), &[], None, true)
                        .is_ok()
                {
                    delivered += 1;
                }
            }
            if let Some(user) = &self.user {
                let sent = store
                    .user(user)
                    .and_then(|u| u.special(SpecialUse::Sent))
                    .map(|m| m.name.clone());
                if let Some(sent) = sent {
                    let seen = normalize_flags(["\\Seen"]);
                    if store
                        .append(user, &sent, message, &seen, None, true)
                        .is_ok()
                    {
                        delivered += 1;
                    }
                }
            }
        }
        info!(
            from = self.mail_from.as_deref().unwrap_or_default(),
            recipients = self.recipients.len(),
            "SMTP message accepted"
        );
        self.shared.notify();
        delivered
    }
}

//! In-memory IMAP + SMTP test server for NM Mail.
//!
//! The server speaks plain TCP (no TLS) and keeps every mailbox in memory. It is used
//! by the engine's integration tests through the library API below and, through the
//! `nmmail-testserver` binary, for manual development of the app before real
//! Gmail / Outlook OAuth credentials exist.
//!
//! IMAP: IMAP4rev1 subset plus IDLE, MOVE, UIDPLUS, SPECIAL-USE, ID, LITERAL+, SASL-IR,
//! UNSELECT, NAMESPACE and (in Gmail mode) X-GM-EXT-1. Every change to a mailbox (made by
//! an IMAP session, the SMTP server or the library API) is pushed to the other sessions
//! that have that mailbox selected: immediately while they are in IDLE, otherwise before
//! the tagged response of their next command.
//!
//! SMTP: EHLO/HELO, AUTH PLAIN/LOGIN/XOAUTH2, MAIL, RCPT, DATA, RSET, NOOP, QUIT. Accepted
//! messages are delivered to the INBOX of every recipient that is a known user and a
//! `\Seen` copy is stored in the sender's Sent mailbox, like Gmail and Outlook do.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use nmmail_testserver::{Config, TestServer};
//!
//! let server = TestServer::start(Config::default()).await?;
//! let uid = server.deliver("test@nmmail.local", "INBOX", b"Subject: hi\r\n\r\nhello\r\n");
//! assert_eq!(uid, 1);
//! server.shutdown().await;
//! # Ok(()) }
//! ```

mod conn;
mod imap;
mod mime;
pub mod seed;
mod smtp;
mod store;

pub use chrono;

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Context as _;
use chrono::{DateTime, FixedOffset};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::conn::{ConnKind, Registry};
use crate::store::Store;

/// Maximum number of entries kept in the IMAP command log.
const MAX_LOG_ENTRIES: usize = 10_000;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Accounts as `(email, password)` pairs. Emails are matched case-insensitively.
    pub users: Vec<(String, String)>,
    /// Bearer token accepted by `AUTHENTICATE XOAUTH2` (IMAP and SMTP) for every user.
    /// `None` disables XOAUTH2 (and stops advertising it over IMAP).
    pub xoauth2_token: Option<String>,
    /// IMAP listen address. Port 0 picks a free port (see [`TestServer::imap_addr`]).
    pub imap_addr: SocketAddr,
    /// SMTP listen address. Port 0 picks a free port (see [`TestServer::smtp_addr`]).
    pub smtp_addr: SocketAddr,
    /// Emulate Gmail: `[Gmail]/...` special mailboxes, `X-GM-EXT-1` (X-GM-MSGID,
    /// X-GM-THRID, X-GM-LABELS, `X-GM-RAW` search).
    pub gmail_mode: bool,
}

impl Default for Config {
    fn default() -> Self {
        let localhost = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        Config {
            users: vec![("test@nmmail.local".to_string(), "test".to_string())],
            xoauth2_token: Some("test-token".to_string()),
            imap_addr: localhost,
            smtp_addr: localhost,
            gmail_mode: false,
        }
    }
}

/// A message as stored by the server (see [`TestServer::messages`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    /// UID inside its mailbox.
    pub uid: u32,
    /// Stored flags (system flags such as `\Seen` and keywords; never `\Recent`).
    pub flags: Vec<String>,
    /// The raw RFC 5322 message, byte for byte.
    pub raw: Vec<u8>,
    /// IMAP INTERNALDATE (second precision).
    pub internal_date: DateTime<FixedOffset>,
}

/// State shared by the API handle and every connection task.
pub(crate) struct Shared {
    pub(crate) store: Mutex<Store>,
    /// Bumped after every mailbox change; sessions wake up on it and diff their view.
    pub(crate) changes: watch::Sender<u64>,
    pub(crate) conns: Mutex<Registry>,
    log: Mutex<VecDeque<String>>,
    pub(crate) xoauth2_token: Option<String>,
    pub(crate) gmail_mode: bool,
}

impl Shared {
    /// Wakes every session so it can report pending mailbox changes.
    pub(crate) fn notify(&self) {
        self.changes.send_modify(|v| *v = v.wrapping_add(1));
    }

    pub(crate) fn log_command(&self, entry: String) {
        let mut log = lock(&self.log);
        if log.len() >= MAX_LOG_ENTRIES {
            log.pop_front();
        }
        log.push_back(entry);
    }

    /// Returns the user key when `email` / `password` are valid.
    pub(crate) fn check_password(&self, email: &str, password: &str) -> Option<String> {
        lock(&self.store).check_password(email, password)
    }

    /// Returns the user key when `token` is the configured XOAUTH2 token and `email` exists.
    pub(crate) fn check_token(&self, email: &str, token: &str) -> Option<String> {
        let expected = self.xoauth2_token.as_deref()?;
        if token != expected {
            return None;
        }
        lock(&self.store).user_key(email)
    }
}

/// Locks a mutex, ignoring poisoning (a panicking connection task must not take the
/// whole server down).
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running in-memory IMAP + SMTP server.
///
/// Dropping the handle stops the listeners and closes every connection; prefer
/// [`TestServer::shutdown`] in async code to wait for that to complete.
pub struct TestServer {
    shared: Arc<Shared>,
    imap_addr: SocketAddr,
    smtp_addr: SocketAddr,
    shutdown_tx: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl TestServer {
    /// Binds the IMAP and SMTP listeners and starts serving.
    pub async fn start(config: Config) -> anyhow::Result<TestServer> {
        let imap_listener = TcpListener::bind(config.imap_addr)
            .await
            .with_context(|| format!("cannot bind IMAP listener on {}", config.imap_addr))?;
        let smtp_listener = TcpListener::bind(config.smtp_addr)
            .await
            .with_context(|| format!("cannot bind SMTP listener on {}", config.smtp_addr))?;
        let imap_addr = imap_listener.local_addr()?;
        let smtp_addr = smtp_listener.local_addr()?;

        let shared = Arc::new(Shared {
            store: Mutex::new(Store::new(&config.users, config.gmail_mode)),
            changes: watch::channel(0u64).0,
            conns: Mutex::new(Registry::default()),
            log: Mutex::new(VecDeque::new()),
            xoauth2_token: config.xoauth2_token.clone(),
            gmail_mode: config.gmail_mode,
        });
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(conn::accept_loop(
                imap_listener,
                shared.clone(),
                shutdown_rx.clone(),
                imap::handle_connection,
            )),
            tokio::spawn(conn::accept_loop(
                smtp_listener,
                shared.clone(),
                shutdown_rx,
                smtp::handle_connection,
            )),
        ];
        tracing::info!(%imap_addr, %smtp_addr, gmail_mode = config.gmail_mode, "test server started");
        Ok(TestServer {
            shared,
            imap_addr,
            smtp_addr,
            shutdown_tx,
            tasks,
        })
    }

    /// Address the IMAP listener is bound to.
    pub fn imap_addr(&self) -> SocketAddr {
        self.imap_addr
    }

    /// Address the SMTP listener is bound to.
    pub fn smtp_addr(&self) -> SocketAddr {
        self.smtp_addr
    }

    /// Appends `raw` to `mailbox` of `user` (no flags, current time as INTERNALDATE)
    /// and returns its UID. Sessions idling on that mailbox are notified immediately.
    ///
    /// # Panics
    /// If the user or the mailbox does not exist.
    pub fn deliver(&self, user: &str, mailbox: &str, raw: &[u8]) -> u32 {
        self.deliver_with_flags(user, mailbox, raw, &[], None)
    }

    /// Like [`TestServer::deliver`] with explicit flags (e.g. `"\\Seen"`, `"$Forwarded"`)
    /// and INTERNALDATE (`None` = now).
    ///
    /// # Panics
    /// If the user or the mailbox does not exist.
    pub fn deliver_with_flags(
        &self,
        user: &str,
        mailbox: &str,
        raw: &[u8],
        flags: &[&str],
        internal_date: Option<DateTime<FixedOffset>>,
    ) -> u32 {
        let flags = store::normalize_flags(flags.iter().copied());
        let result = lock(&self.shared.store).append(
            user,
            mailbox,
            raw.to_vec(),
            &flags,
            internal_date,
            true,
        );
        match result {
            Ok((_, uid)) => {
                self.shared.notify();
                uid
            }
            Err(err) => panic!("nmmail-testserver: cannot deliver to {user} / {mailbox}: {err}"),
        }
    }

    /// Replaces the flags of a message, like another client would (sessions are
    /// notified). Unknown users, mailboxes or UIDs are ignored.
    pub fn set_flags(&self, user: &str, mailbox: &str, uid: u32, flags: &[&str]) {
        let flags = store::normalize_flags(flags.iter().copied());
        let changed = lock(&self.shared.store).replace_flags(user, mailbox, uid, flags);
        if changed {
            self.shared.notify();
        }
    }

    /// Permanently removes a message, like another client would with `UID EXPUNGE`
    /// (sessions are notified). Unknown users, mailboxes or UIDs are ignored.
    pub fn expunge_uid(&self, user: &str, mailbox: &str, uid: u32) {
        let removed = lock(&self.shared.store).remove_uids(user, mailbox, &[uid]);
        if removed > 0 {
            self.shared.notify();
        }
    }

    /// Snapshot of a mailbox (empty if the user or mailbox does not exist).
    pub fn messages(&self, user: &str, mailbox: &str) -> Vec<StoredMessage> {
        lock(&self.shared.store).snapshot(user, mailbox)
    }

    /// Bumps UIDVALIDITY of a mailbox and renumbers its messages from UID 1. Sessions
    /// that have the mailbox selected are disconnected with `* BYE`, as real servers do.
    pub fn reset_uidvalidity(&self, user: &str, mailbox: &str) {
        let changed = lock(&self.shared.store).reset_uidvalidity(user, mailbox);
        if changed {
            self.shared.notify();
        }
    }

    /// Current UIDVALIDITY of a mailbox (`None` if it does not exist).
    pub fn uid_validity(&self, user: &str, mailbox: &str) -> Option<u32> {
        lock(&self.shared.store).uid_validity(user, mailbox)
    }

    /// Creates a mailbox (and missing parents). Returns `false` if it already exists or
    /// the user is unknown.
    pub fn create_mailbox(&self, user: &str, mailbox: &str) -> bool {
        lock(&self.shared.store)
            .create_mailbox(user, mailbox)
            .is_ok()
    }

    /// Abruptly closes (TCP reset) every open IMAP and SMTP connection, simulating a
    /// network loss. New connections keep working.
    pub fn drop_all_connections(&self) {
        lock(&self.shared.conns).kill_all();
    }

    /// `true`: every currently open connection stops reading and writing but stays
    /// open (a half-open TCP connection); connections opened later are not affected.
    /// `false`: resumes the frozen connections.
    pub fn freeze_connections(&self, frozen: bool) {
        lock(&self.shared.conns).freeze_all(frozen);
    }

    /// Number of currently open IMAP connections.
    pub fn imap_connection_count(&self) -> usize {
        lock(&self.shared.conns).count(ConnKind::Imap)
    }

    /// Received IMAP command lines (`tag COMMAND args`, first line only for commands
    /// with literals; passwords and SASL payloads replaced by `***`). `DONE` lines that
    /// end an IDLE are logged as `DONE`.
    pub fn command_log(&self) -> Vec<String> {
        lock(&self.shared.log).iter().cloned().collect()
    }

    /// Empties the command log.
    pub fn clear_command_log(&self) {
        lock(&self.shared.log).clear();
    }

    /// Fills `user`'s INBOX and Sent mailboxes with realistic Italian demo messages
    /// (see [`seed`]). Returns the number of messages stored.
    pub fn seed_demo_messages(&self, user: &str) -> usize {
        let count = seed::seed_user(&mut lock(&self.shared.store), user);
        self.shared.notify();
        count
    }

    /// Stops the listeners and closes every connection.
    pub async fn shutdown(mut self) {
        self.signal_shutdown();
        for task in std::mem::take(&mut self.tasks) {
            let _ = task.await;
        }
    }

    fn signal_shutdown(&self) {
        self.shutdown_tx.send_replace(true);
        lock(&self.shared.conns).kill_all();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.signal_shutdown();
        for task in &self.tasks {
            task.abort();
        }
    }
}

// Engine tests share the server between tasks (e.g. behind an `Arc`).
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TestServer>();
    assert_send_sync::<Config>();
    assert_send_sync::<StoredMessage>();
};

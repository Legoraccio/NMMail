//! SQLite storage (WAL, FTS5). The daemon is the only writer.
//!
//! All calls are short and synchronous; the connection lives behind a mutex
//! that is never held across an `.await`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::error::not_found;
use crate::model::*;
use crate::settings::Settings;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS accounts(
  id INTEGER PRIMARY KEY,
  provider TEXT NOT NULL,
  email TEXT NOT NULL,
  display_name TEXT NOT NULL,
  username TEXT NOT NULL,
  auth TEXT NOT NULL,
  imap_host TEXT NOT NULL, imap_port INTEGER NOT NULL, imap_security TEXT NOT NULL,
  smtp_host TEXT NOT NULL, smtp_port INTEGER NOT NULL, smtp_security TEXT NOT NULL,
  notify INTEGER NOT NULL DEFAULT 1,
  notify_filter TEXT NOT NULL DEFAULT 'all',
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS folders(
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  display_name TEXT NOT NULL,
  delimiter TEXT,
  role TEXT,
  parent_id INTEGER,
  depth INTEGER NOT NULL DEFAULT 0,
  selectable INTEGER NOT NULL DEFAULT 1,
  uidvalidity INTEGER,
  uidnext INTEGER,
  highest_uid INTEGER NOT NULL DEFAULT 0,
  highest_modseq INTEGER,
  oldest_date INTEGER,
  last_sync INTEGER,
  UNIQUE(account_id, name)
);
CREATE TABLE IF NOT EXISTS messages(
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  uid INTEGER NOT NULL,
  message_id TEXT,
  in_reply_to TEXT,
  refs TEXT,
  gm_msgid INTEGER,
  gm_thrid INTEGER,
  subject TEXT NOT NULL DEFAULT '',
  from_name TEXT,
  from_addr TEXT,
  to_json TEXT NOT NULL DEFAULT '[]',
  cc_json TEXT NOT NULL DEFAULT '[]',
  reply_to_json TEXT NOT NULL DEFAULT '[]',
  date INTEGER NOT NULL,
  size INTEGER NOT NULL DEFAULT 0,
  flags INTEGER NOT NULL DEFAULT 0,
  snippet TEXT NOT NULL DEFAULT '',
  has_attachments INTEGER NOT NULL DEFAULT 0,
  category TEXT,
  UNIQUE(folder_id, uid)
);
CREATE INDEX IF NOT EXISTS messages_folder_date ON messages(folder_id, date DESC, id DESC);
CREATE INDEX IF NOT EXISTS messages_account_date ON messages(account_id, date DESC);
CREATE INDEX IF NOT EXISTS messages_msgid ON messages(message_id);
CREATE TABLE IF NOT EXISTS bodies(
  message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
  raw BLOB NOT NULL,
  fetched_at INTEGER NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  subject, sender, recipients, body,
  content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS messages_fts_ad AFTER DELETE ON messages BEGIN
  DELETE FROM messages_fts WHERE rowid = old.id;
END;
CREATE TABLE IF NOT EXISTS contacts(
  address TEXT PRIMARY KEY COLLATE NOCASE,
  name TEXT,
  last_used INTEGER NOT NULL,
  uses INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS outbox(
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  raw BLOB NOT NULL,
  sender TEXT NOT NULL,
  recipients TEXT NOT NULL,
  reply_to_message INTEGER,
  created_at INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT
);
"#;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

/// Input for creating an account.
#[derive(Debug, Clone)]
pub struct NewAccount {
    pub provider: Provider,
    pub email: String,
    pub display_name: String,
    pub username: String,
    pub auth: AuthKind,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
}

/// Folder as seen on the server (from LIST).
#[derive(Debug, Clone)]
pub struct RemoteFolder {
    pub name: String,
    pub delimiter: Option<String>,
    pub role: Option<FolderRole>,
    pub selectable: bool,
}

/// Folder including sync bookkeeping.
#[derive(Debug, Clone)]
pub struct FolderState {
    pub id: FolderId,
    pub account_id: AccountId,
    pub name: String,
    pub role: Option<FolderRole>,
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
    pub highest_uid: u32,
    pub highest_modseq: Option<u64>,
    pub oldest_date: Option<i64>,
    pub last_sync: Option<i64>,
}

/// Header data for a message to insert or refresh.
#[derive(Debug, Clone, Default)]
pub struct NewMessage {
    pub uid: u32,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub gm_msgid: Option<u64>,
    pub gm_thrid: Option<u64>,
    pub subject: String,
    pub from: Option<Addr>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub reply_to: Vec<Addr>,
    pub date: i64,
    pub size: u32,
    pub flags: i64,
    pub snippet: String,
    pub has_attachments: bool,
}

/// Everything needed to act on a stored message.
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub summary: MessageSummary,
    pub uid: u32,
    pub flags: i64,
    pub folder_name: String,
    pub folder_role: Option<FolderRole>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub cc: Vec<Addr>,
    pub reply_to: Vec<Addr>,
}

#[derive(Debug, Clone)]
pub struct OutboxItem {
    pub id: i64,
    pub account_id: AccountId,
    pub raw: Vec<u8>,
    pub sender: String,
    pub recipients: Vec<String>,
    pub reply_to_message: Option<MessageId>,
    pub attempts: u32,
}

/// Which messages to list.
#[derive(Debug, Clone, Copy)]
pub enum ListScope {
    Folder(FolderId),
    UnifiedInbox,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;
             PRAGMA cache_size=-2000;
             PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(Store {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---------------------------------------------------------------- settings

    pub fn load_settings(&self) -> Result<Settings> {
        let raw: Option<String> = self
            .db()
            .query_row("SELECT value FROM settings WHERE key='settings'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(raw
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .map(|mut s| {
                // Build-time OAuth defaults fill in blanks.
                let d = Settings::default();
                if s.google_client_id.is_empty() {
                    s.google_client_id = d.google_client_id;
                    s.google_client_secret = d.google_client_secret;
                }
                if s.microsoft_client_id.is_empty() {
                    s.microsoft_client_id = d.microsoft_client_id;
                }
                s
            })
            .unwrap_or_default())
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        self.db().execute(
            "INSERT INTO settings(key, value) VALUES('settings', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [serde_json::to_string(s)?],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- accounts

    pub fn insert_account(&self, a: &NewAccount) -> Result<AccountConfig> {
        let db = self.db();
        db.execute(
            "INSERT INTO accounts(provider, email, display_name, username, auth,
                imap_host, imap_port, imap_security, smtp_host, smtp_port, smtp_security, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                a.provider.as_str(),
                a.email,
                a.display_name,
                a.username,
                a.auth.as_str(),
                a.imap.host,
                a.imap.port,
                a.imap.security.as_str(),
                a.smtp.host,
                a.smtp.port,
                a.smtp.security.as_str(),
                now_ms()
            ],
        )?;
        let id = db.last_insert_rowid();
        drop(db);
        self.account(id)
    }

    pub fn update_account_prefs(
        &self,
        id: AccountId,
        display_name: Option<&str>,
        notify: Option<bool>,
        notify_filter: Option<NotifyFilter>,
    ) -> Result<AccountConfig> {
        let db = self.db();
        if let Some(n) = display_name {
            db.execute(
                "UPDATE accounts SET display_name=?1 WHERE id=?2",
                params![n, id],
            )?;
        }
        if let Some(n) = notify {
            db.execute("UPDATE accounts SET notify=?1 WHERE id=?2", params![n, id])?;
        }
        if let Some(f) = notify_filter {
            db.execute(
                "UPDATE accounts SET notify_filter=?1 WHERE id=?2",
                params![f.as_str(), id],
            )?;
        }
        drop(db);
        self.account(id)
    }

    /// Replaces how an account connects (used when the user signs in again).
    pub fn update_account_connection(
        &self,
        id: AccountId,
        username: &str,
        auth: AuthKind,
        imap: &ServerConfig,
        smtp: &ServerConfig,
    ) -> Result<()> {
        self.db().execute(
            "UPDATE accounts SET username=?1, auth=?2, imap_host=?3, imap_port=?4, imap_security=?5,
                smtp_host=?6, smtp_port=?7, smtp_security=?8 WHERE id=?9",
            params![
                username,
                auth.as_str(),
                imap.host,
                imap.port,
                imap.security.as_str(),
                smtp.host,
                smtp.port,
                smtp.security.as_str(),
                id
            ],
        )?;
        Ok(())
    }

    pub fn accounts(&self) -> Result<Vec<AccountConfig>> {
        let db = self.db();
        let mut st = db.prepare("SELECT * FROM accounts ORDER BY id")?;
        let rows = st.query_map([], account_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn account(&self, id: AccountId) -> Result<AccountConfig> {
        self.db()
            .query_row("SELECT * FROM accounts WHERE id=?1", [id], account_from_row)
            .optional()?
            .ok_or_else(|| not_found(format!("account {id}")))
    }

    pub fn account_by_email(&self, email: &str) -> Result<Option<AccountConfig>> {
        Ok(self
            .db()
            .query_row(
                "SELECT * FROM accounts WHERE email=?1 COLLATE NOCASE",
                [email],
                account_from_row,
            )
            .optional()?)
    }

    pub fn delete_account(&self, id: AccountId) -> Result<()> {
        self.db()
            .execute("DELETE FROM accounts WHERE id=?1", [id])?;
        Ok(())
    }

    // ----------------------------------------------------------------- folders

    /// Replaces the folder list of an account with what the server reported,
    /// keeping ids (and sync state) of folders that still exist.
    pub fn sync_folders(&self, account_id: AccountId, remote: &[RemoteFolder]) -> Result<bool> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut existing: HashMap<String, (FolderId, Option<String>, bool)> = HashMap::new();
        {
            let mut st =
                tx.prepare("SELECT id, name, role, selectable FROM folders WHERE account_id=?1")?;
            let rows = st.query_map([account_id], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    (r.get(0)?, r.get(2)?, r.get::<_, bool>(3)?),
                ))
            })?;
            for row in rows {
                let (name, v) = row?;
                existing.insert(name, v);
            }
        }
        let mut changed = false;
        let mut seen = HashSet::new();
        let mut ids: HashMap<String, FolderId> = HashMap::new();
        for f in remote {
            seen.insert(f.name.clone());
            let role = f.role.map(|r| r.as_str().to_string());
            let display = display_name_for(f);
            match existing.get(&f.name) {
                Some((id, old_role, old_sel)) => {
                    if *old_role != role || *old_sel != f.selectable {
                        tx.execute(
                            "UPDATE folders SET role=?1, selectable=?2, display_name=?3, delimiter=?4 WHERE id=?5",
                            params![role, f.selectable, display, f.delimiter, id],
                        )?;
                        changed = true;
                    }
                    ids.insert(f.name.clone(), *id);
                }
                None => {
                    tx.execute(
                        "INSERT INTO folders(account_id, name, display_name, delimiter, role, selectable)
                         VALUES(?1,?2,?3,?4,?5,?6)",
                        params![account_id, f.name, display, f.delimiter, role, f.selectable],
                    )?;
                    ids.insert(f.name.clone(), tx.last_insert_rowid());
                    changed = true;
                }
            }
        }
        for (name, (id, _, _)) in &existing {
            if !seen.contains(name) {
                tx.execute("DELETE FROM folders WHERE id=?1", [id])?;
                changed = true;
            }
        }
        // Hierarchy (parent/depth) from delimiters.
        for f in remote {
            let Some(delim) = f.delimiter.as_deref().filter(|d| !d.is_empty()) else {
                continue;
            };
            let parts: Vec<&str> = f.name.split(delim).collect();
            let depth = parts.len().saturating_sub(1) as i64;
            let parent = if parts.len() > 1 {
                ids.get(&parts[..parts.len() - 1].join(delim)).copied()
            } else {
                None
            };
            tx.execute(
                "UPDATE folders SET depth=?1, parent_id=?2 WHERE id=?3",
                params![
                    if parent.is_some() { depth } else { 0 },
                    parent,
                    ids[&f.name]
                ],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn folders(&self, account_id: Option<AccountId>) -> Result<Vec<Folder>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT f.id, f.account_id, f.name, f.display_name, f.role, f.parent_id, f.depth, f.selectable,
                    COALESCE(c.unread, 0), COALESCE(c.total, 0)
             FROM folders f
             LEFT JOIN (SELECT folder_id, SUM((flags & 1) = 0) AS unread, COUNT(*) AS total
                        FROM messages GROUP BY folder_id) c ON c.folder_id = f.id
             WHERE (?1 IS NULL OR f.account_id = ?1)",
        )?;
        let rows = st.query_map([account_id], |r| {
            Ok(Folder {
                id: r.get(0)?,
                account_id: r.get(1)?,
                name: r.get(2)?,
                display_name: r.get(3)?,
                role: r
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| FolderRole::parse(&s)),
                parent_id: r.get(5)?,
                depth: r.get(6)?,
                selectable: r.get(7)?,
                unread: r.get(8)?,
                total: r.get(9)?,
            })
        })?;
        let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        sort_folders(&mut out);
        Ok(out)
    }

    pub fn folder_state(&self, id: FolderId) -> Result<FolderState> {
        self.db()
            .query_row(
                "SELECT * FROM folders WHERE id=?1",
                [id],
                folder_state_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found(format!("folder {id}")))
    }

    pub fn folder_by_role(
        &self,
        account_id: AccountId,
        role: FolderRole,
    ) -> Result<Option<FolderState>> {
        Ok(self
            .db()
            .query_row(
                "SELECT * FROM folders WHERE account_id=?1 AND role=?2 ORDER BY id LIMIT 1",
                params![account_id, role.as_str()],
                folder_state_from_row,
            )
            .optional()?)
    }

    pub fn folder_by_name(&self, account_id: AccountId, name: &str) -> Result<Option<FolderState>> {
        Ok(self
            .db()
            .query_row(
                "SELECT * FROM folders WHERE account_id=?1 AND name=?2",
                params![account_id, name],
                folder_state_from_row,
            )
            .optional()?)
    }

    /// Ensures a folder row exists (used for INBOX before the first LIST).
    pub fn ensure_folder(
        &self,
        account_id: AccountId,
        name: &str,
        role: Option<FolderRole>,
    ) -> Result<FolderState> {
        if let Some(f) = self.folder_by_name(account_id, name)? {
            return Ok(f);
        }
        let display = display_name_for(&RemoteFolder {
            name: name.to_string(),
            delimiter: None,
            role,
            selectable: true,
        });
        self.db().execute(
            "INSERT OR IGNORE INTO folders(account_id, name, display_name, role) VALUES(?1,?2,?3,?4)",
            params![account_id, name, display, role.map(|r| r.as_str())],
        )?;
        self.folder_by_name(account_id, name)?
            .ok_or_else(|| not_found(name.to_string()))
    }

    pub fn update_folder_sync(
        &self,
        id: FolderId,
        uidvalidity: u32,
        uidnext: Option<u32>,
        highest_uid: u32,
        highest_modseq: Option<u64>,
        oldest_date: Option<i64>,
    ) -> Result<()> {
        self.db().execute(
            "UPDATE folders SET uidvalidity=?1, uidnext=?2, highest_uid=MAX(highest_uid, ?3),
                highest_modseq=COALESCE(?4, highest_modseq),
                oldest_date=COALESCE(?5, oldest_date), last_sync=?6 WHERE id=?7",
            params![
                uidvalidity,
                uidnext,
                highest_uid,
                highest_modseq.map(|m| m as i64),
                oldest_date,
                now_ms(),
                id
            ],
        )?;
        Ok(())
    }

    /// UIDVALIDITY changed: forget everything cached for the folder.
    pub fn reset_folder(&self, id: FolderId) -> Result<()> {
        let db = self.db();
        db.execute("DELETE FROM messages WHERE folder_id=?1", [id])?;
        db.execute(
            "UPDATE folders SET uidvalidity=NULL, uidnext=NULL, highest_uid=0, highest_modseq=NULL,
                oldest_date=NULL, last_sync=NULL WHERE id=?1",
            [id],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- messages

    /// Inserts new messages / refreshes flags of known ones.
    /// Returns ids of messages that were newly inserted.
    pub fn upsert_messages(
        &self,
        account_id: AccountId,
        folder_id: FolderId,
        msgs: &[NewMessage],
    ) -> Result<Vec<MessageId>> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut inserted = Vec::new();
        {
            let mut find =
                tx.prepare_cached("SELECT id, flags FROM messages WHERE folder_id=?1 AND uid=?2")?;
            let mut upd = tx.prepare_cached("UPDATE messages SET flags=?1 WHERE id=?2")?;
            let mut ins = tx.prepare_cached(
                "INSERT INTO messages(account_id, folder_id, uid, message_id, in_reply_to, refs,
                    gm_msgid, gm_thrid, subject, from_name, from_addr, to_json, cc_json, reply_to_json,
                    date, size, flags, snippet, has_attachments)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            )?;
            let mut fts = tx.prepare_cached(
                "INSERT INTO messages_fts(rowid, subject, sender, recipients, body) VALUES(?1,?2,?3,?4,?5)",
            )?;
            for m in msgs {
                let found: Option<(i64, i64)> = find
                    .query_row(params![folder_id, m.uid], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                if let Some((id, flags)) = found {
                    if flags != m.flags {
                        upd.execute(params![m.flags, id])?;
                    }
                    continue;
                }
                ins.execute(params![
                    account_id,
                    folder_id,
                    m.uid,
                    m.message_id,
                    m.in_reply_to,
                    m.references,
                    m.gm_msgid.map(|v| v as i64),
                    m.gm_thrid.map(|v| v as i64),
                    m.subject,
                    m.from.as_ref().and_then(|a| a.name.clone()),
                    m.from.as_ref().map(|a| a.address.clone()),
                    serde_json::to_string(&m.to)?,
                    serde_json::to_string(&m.cc)?,
                    serde_json::to_string(&m.reply_to)?,
                    m.date,
                    m.size,
                    m.flags,
                    m.snippet,
                    m.has_attachments,
                ])?;
                let id = tx.last_insert_rowid();
                fts.execute(params![
                    id,
                    m.subject,
                    m.from
                        .as_ref()
                        .map(|a| format!("{} {}", a.name.as_deref().unwrap_or(""), a.address))
                        .unwrap_or_default(),
                    addr_text(m.to.iter().chain(m.cc.iter())),
                    m.snippet,
                ])?;
                inserted.push(id);
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Applies server flags for (uid, flags) pairs. Returns how many changed.
    pub fn apply_flags(&self, folder_id: FolderId, flags: &[(u32, i64)]) -> Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut changed = 0;
        {
            let mut st = tx.prepare_cached(
                "UPDATE messages SET flags=?1 WHERE folder_id=?2 AND uid=?3 AND flags<>?1",
            )?;
            for (uid, f) in flags {
                changed += st.execute(params![f, folder_id, uid])?;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// All cached UIDs of a folder with their flags.
    pub fn folder_uids(&self, folder_id: FolderId) -> Result<Vec<(u32, i64)>> {
        let db = self.db();
        let mut st =
            db.prepare("SELECT uid, flags FROM messages WHERE folder_id=?1 ORDER BY uid")?;
        let rows = st.query_map([folder_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete_uids(&self, folder_id: FolderId, uids: &[u32]) -> Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let mut n = 0;
        {
            let mut st = tx.prepare_cached("DELETE FROM messages WHERE folder_id=?1 AND uid=?2")?;
            for uid in uids {
                n += st.execute(params![folder_id, uid])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn delete_messages(&self, ids: &[MessageId]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            let mut st = tx.prepare_cached("DELETE FROM messages WHERE id=?1")?;
            for id in ids {
                st.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_messages(
        &self,
        scope: ListScope,
        before: Option<(i64, MessageId)>,
        limit: u32,
        unread_only: bool,
    ) -> Result<Vec<MessageSummary>> {
        let (bd, bi) = before.unwrap_or((i64::MAX, i64::MAX));
        let unread = if unread_only { 1 } else { 0 };
        let db = self.db();
        let sql_where = match scope {
            ListScope::Folder(_) => "m.folder_id = ?1",
            ListScope::UnifiedInbox => {
                "m.folder_id IN (SELECT id FROM folders WHERE role='inbox') AND ?1 IS NOT NULL"
            }
        };
        let sql = format!(
            "SELECT {SUMMARY_COLS} FROM messages m
             WHERE {sql_where} AND (m.date < ?2 OR (m.date = ?2 AND m.id < ?3))
               AND (?4 = 0 OR (m.flags & 1) = 0)
             ORDER BY m.date DESC, m.id DESC LIMIT ?5"
        );
        let folder = match scope {
            ListScope::Folder(f) => f,
            ListScope::UnifiedInbox => 0,
        };
        let mut st = db.prepare(&sql)?;
        let rows = st.query_map(params![folder, bd, bi, unread, limit], summary_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn summaries(&self, ids: &[MessageId]) -> Result<Vec<MessageSummary>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let db = self.db();
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT {SUMMARY_COLS} FROM messages m WHERE m.id IN ({placeholders}) ORDER BY m.date DESC, m.id DESC"
        );
        let mut st = db.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(ids.iter()), summary_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn message_row(&self, id: MessageId) -> Result<MessageRow> {
        let db = self.db();
        let sql = format!(
            "SELECT {SUMMARY_COLS}, m.uid, m.flags, f.name, f.role, m.message_id, m.in_reply_to, m.refs,
                    m.cc_json, m.reply_to_json
             FROM messages m JOIN folders f ON f.id = m.folder_id WHERE m.id=?1"
        );
        db.query_row(&sql, [id], |r| {
            Ok(MessageRow {
                summary: summary_from_row(r)?,
                uid: r.get(SUMMARY_N)?,
                flags: r.get(SUMMARY_N + 1)?,
                folder_name: r.get(SUMMARY_N + 2)?,
                folder_role: r
                    .get::<_, Option<String>>(SUMMARY_N + 3)?
                    .and_then(|s| FolderRole::parse(&s)),
                message_id: r.get(SUMMARY_N + 4)?,
                in_reply_to: r.get(SUMMARY_N + 5)?,
                references: r.get(SUMMARY_N + 6)?,
                cc: parse_addrs(&r.get::<_, String>(SUMMARY_N + 7)?),
                reply_to: parse_addrs(&r.get::<_, String>(SUMMARY_N + 8)?),
            })
        })
        .optional()?
        .ok_or_else(|| not_found(format!("message {id}")))
    }

    /// Optimistic local flag update; returns the new flags.
    pub fn set_flags(&self, id: MessageId, set: i64, clear: i64) -> Result<i64> {
        let db = self.db();
        db.execute(
            "UPDATE messages SET flags = (flags | ?1) & ~?2 WHERE id=?3",
            params![set, clear, id],
        )?;
        Ok(db.query_row("SELECT flags FROM messages WHERE id=?1", [id], |r| r.get(0))?)
    }

    pub fn store_body(
        &self,
        id: MessageId,
        raw: &[u8],
        snippet: &str,
        body_text: &str,
        has_attachments: bool,
    ) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO bodies(message_id, raw, fetched_at) VALUES(?1,?2,?3)
             ON CONFLICT(message_id) DO UPDATE SET raw=excluded.raw, fetched_at=excluded.fetched_at",
            params![id, raw, now_ms()],
        )?;
        tx.execute(
            "UPDATE messages SET snippet=?1, has_attachments=?2 WHERE id=?3",
            params![snippet, has_attachments, id],
        )?;
        let fts_row: Option<FtsSource> = tx
            .query_row(
                "SELECT subject, from_name, from_addr, to_json, cc_json FROM messages WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        if let Some((subject, from_name, from_addr, to, cc)) = fts_row {
            tx.execute("DELETE FROM messages_fts WHERE rowid=?1", [id])?;
            let recips: Vec<Addr> = parse_addrs(&to)
                .into_iter()
                .chain(parse_addrs(&cc))
                .collect();
            let mut body = body_text.to_string();
            truncate_chars(&mut body, 20_000);
            tx.execute(
                "INSERT INTO messages_fts(rowid, subject, sender, recipients, body) VALUES(?1,?2,?3,?4,?5)",
                params![
                    id,
                    subject,
                    format!(
                        "{} {}",
                        from_name.unwrap_or_default(),
                        from_addr.unwrap_or_default()
                    ),
                    addr_text(recips.iter()),
                    body
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn body(&self, id: MessageId) -> Result<Option<Vec<u8>>> {
        Ok(self
            .db()
            .query_row("SELECT raw FROM bodies WHERE message_id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Messages lacking a cached body (for background prefetch), newest first.
    pub fn messages_without_body(
        &self,
        folder_id: FolderId,
        limit: u32,
        max_size: u32,
    ) -> Result<Vec<(MessageId, u32)>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT m.id, m.uid FROM messages m LEFT JOIN bodies b ON b.message_id = m.id
             WHERE m.folder_id=?1 AND b.message_id IS NULL AND m.size <= ?2
             ORDER BY m.date DESC LIMIT ?3",
        )?;
        let rows = st.query_map(params![folder_id, max_size, limit], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Full-text search over the local cache.
    pub fn search(
        &self,
        query: &str,
        account_id: Option<AccountId>,
        limit: u32,
    ) -> Result<Vec<MessageSummary>> {
        let Some(fts) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let db = self.db();
        let sql = format!(
            "SELECT {SUMMARY_COLS} FROM messages m
             JOIN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1) s ON s.rowid = m.id
             WHERE (?2 IS NULL OR m.account_id = ?2)
             ORDER BY m.date DESC, m.id DESC LIMIT ?3"
        );
        let mut st = db.prepare(&sql)?;
        let rows = st.query_map(params![fts, account_id, limit * 2], summary_from_row)?;
        // The same mail can live in several folders (e.g. Gmail labels): dedupe.
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for r in rows {
            let m = r?;
            let key = (m.account_id, m.subject.clone(), m.date, m.from.clone());
            if seen.insert(key) {
                out.push(m);
            }
            if out.len() >= limit as usize {
                break;
            }
        }
        Ok(out)
    }

    /// Unread count of each account's INBOX.
    pub fn inbox_unread(&self) -> Result<HashMap<AccountId, u32>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT m.account_id, COUNT(*) FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE f.role='inbox' AND (m.flags & 1) = 0 GROUP BY m.account_id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }

    /// Drops cached messages older than the cache window (flagged ones are kept)
    /// and returns how many were removed.
    pub fn prune(&self, cache_days: u32) -> Result<usize> {
        if cache_days == 0 {
            return Ok(0);
        }
        let cutoff = now_ms() - cache_days as i64 * 86_400_000;
        let db = self.db();
        let n = db.execute(
            "DELETE FROM messages WHERE date < ?1 AND (flags & 2) = 0",
            [cutoff],
        )?;
        db.execute(
            "UPDATE folders SET oldest_date = MAX(COALESCE(oldest_date, 0), ?1) WHERE oldest_date IS NOT NULL",
            [cutoff],
        )?;
        Ok(n)
    }

    // ---------------------------------------------------------------- contacts

    pub fn touch_contacts(&self, addrs: &[Addr]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO contacts(address, name, last_used) VALUES(?1, ?2, ?3)
                 ON CONFLICT(address) DO UPDATE SET uses = uses + 1, last_used = excluded.last_used,
                    name = COALESCE(excluded.name, contacts.name)",
            )?;
            for a in addrs {
                if a.address.contains('@') {
                    st.execute(params![a.address.to_lowercase(), a.name, now_ms()])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Address suggestions: explicit contacts first, then senders seen in mail.
    pub fn suggest_contacts(&self, prefix: &str, limit: u32) -> Result<Vec<Addr>> {
        let p = prefix.trim().to_lowercase();
        if p.is_empty() {
            return Ok(Vec::new());
        }
        let like = format!("{}%", p.replace(['%', '_'], ""));
        let word = format!("% {}", like);
        let db = self.db();
        let mut out: Vec<Addr> = Vec::new();
        let mut seen = HashSet::new();
        let mut st = db.prepare(
            "SELECT address, name FROM contacts
             WHERE address LIKE ?1 OR name LIKE ?1 OR name LIKE ?2
             ORDER BY uses DESC, last_used DESC LIMIT ?3",
        )?;
        for r in st.query_map(params![like, word, limit], |r| {
            Ok(Addr {
                address: r.get(0)?,
                name: r.get(1)?,
            })
        })? {
            let a = r?;
            if seen.insert(a.address.to_lowercase()) {
                out.push(a);
            }
        }
        if out.len() < limit as usize {
            let mut st = db.prepare(
                "SELECT from_addr, from_name, COUNT(*) c FROM messages
                 WHERE from_addr IS NOT NULL AND (from_addr LIKE ?1 OR from_name LIKE ?1 OR from_name LIKE ?2)
                 GROUP BY lower(from_addr) ORDER BY c DESC LIMIT ?3",
            )?;
            for r in st.query_map(params![like, word, limit], |r| {
                Ok(Addr {
                    address: r.get(0)?,
                    name: r.get(1)?,
                })
            })? {
                let a = r?;
                if out.len() < limit as usize && seen.insert(a.address.to_lowercase()) {
                    out.push(a);
                }
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ outbox

    pub fn outbox_insert(
        &self,
        account_id: AccountId,
        raw: &[u8],
        sender: &str,
        recipients: &[String],
        reply_to_message: Option<MessageId>,
    ) -> Result<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO outbox(account_id, raw, sender, recipients, reply_to_message, created_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                account_id,
                raw,
                sender,
                serde_json::to_string(recipients)?,
                reply_to_message,
                now_ms()
            ],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn outbox(&self) -> Result<Vec<OutboxItem>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT id, account_id, raw, sender, recipients, reply_to_message, attempts FROM outbox ORDER BY id",
        )?;
        let rows = st.query_map([], |r| {
            Ok(OutboxItem {
                id: r.get(0)?,
                account_id: r.get(1)?,
                raw: r.get(2)?,
                sender: r.get(3)?,
                recipients: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                reply_to_message: r.get(5)?,
                attempts: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn outbox_done(&self, id: i64) -> Result<()> {
        self.db().execute("DELETE FROM outbox WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn outbox_failed(&self, id: i64, error: &str) -> Result<()> {
        self.db().execute(
            "UPDATE outbox SET attempts = attempts + 1, last_error=?1 WHERE id=?2",
            params![error, id],
        )?;
        Ok(())
    }
}

// -------------------------------------------------------------------- helpers

/// (subject, from_name, from_addr, to_json, cc_json) of a message, for FTS rows.
type FtsSource = (String, Option<String>, Option<String>, String, String);

const SUMMARY_COLS: &str =
    "m.id, m.account_id, m.folder_id, m.subject, m.from_name, m.from_addr, m.to_json,
    m.date, m.snippet, m.flags, m.has_attachments, m.size";
const SUMMARY_N: usize = 12;

fn summary_from_row(r: &Row<'_>) -> rusqlite::Result<MessageSummary> {
    let flags: i64 = r.get(9)?;
    let from_addr: Option<String> = r.get(5)?;
    Ok(MessageSummary {
        id: r.get(0)?,
        account_id: r.get(1)?,
        folder_id: r.get(2)?,
        subject: r.get(3)?,
        from: from_addr.map(|address| Addr {
            name: r.get::<_, Option<String>>(4).ok().flatten(),
            address,
        }),
        to: parse_addrs(&r.get::<_, String>(6)?),
        date: r.get(7)?,
        snippet: r.get(8)?,
        seen: flags & flags::SEEN != 0,
        flagged: flags & flags::FLAGGED != 0,
        answered: flags & flags::ANSWERED != 0,
        has_attachments: r.get(10)?,
        size: r.get(11)?,
    })
}

fn account_from_row(r: &Row<'_>) -> rusqlite::Result<AccountConfig> {
    let get_s = |name: &str| r.get::<_, String>(name);
    Ok(AccountConfig {
        id: r.get("id")?,
        provider: Provider::parse(&get_s("provider")?).unwrap_or(Provider::Imap),
        email: get_s("email")?,
        display_name: get_s("display_name")?,
        username: get_s("username")?,
        auth: AuthKind::parse(&get_s("auth")?).unwrap_or(AuthKind::Password),
        imap: ServerConfig {
            host: get_s("imap_host")?,
            port: r.get("imap_port")?,
            security: Security::parse(&get_s("imap_security")?).unwrap_or(Security::Tls),
        },
        smtp: ServerConfig {
            host: get_s("smtp_host")?,
            port: r.get("smtp_port")?,
            security: Security::parse(&get_s("smtp_security")?).unwrap_or(Security::Tls),
        },
        notify: r.get("notify")?,
        notify_filter: NotifyFilter::parse(&get_s("notify_filter")?),
    })
}

fn folder_state_from_row(r: &Row<'_>) -> rusqlite::Result<FolderState> {
    Ok(FolderState {
        id: r.get("id")?,
        account_id: r.get("account_id")?,
        name: r.get("name")?,
        role: r
            .get::<_, Option<String>>("role")?
            .and_then(|s| FolderRole::parse(&s)),
        uidvalidity: r.get("uidvalidity")?,
        uidnext: r.get("uidnext")?,
        highest_uid: r.get("highest_uid")?,
        highest_modseq: r.get::<_, Option<i64>>("highest_modseq")?.map(|v| v as u64),
        oldest_date: r.get("oldest_date")?,
        last_sync: r.get("last_sync")?,
    })
}

fn parse_addrs(json: &str) -> Vec<Addr> {
    serde_json::from_str(json).unwrap_or_default()
}

fn addr_text<'a>(addrs: impl Iterator<Item = &'a Addr>) -> String {
    let mut s = String::new();
    for a in addrs {
        if let Some(n) = &a.name {
            s.push_str(n);
            s.push(' ');
        }
        s.push_str(&a.address);
        s.push(' ');
    }
    s
}

fn truncate_chars(s: &mut String, max: usize) {
    if let Some((idx, _)) = s.char_indices().nth(max) {
        s.truncate(idx);
    }
}

/// Builds a safe FTS5 query: every word becomes a quoted prefix term (AND).
pub fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | ':' | '*'))
        .map(|t| t.trim_matches('"'))
        .filter(|t| !t.is_empty())
        .take(12)
        .map(|t| format!("\"{}\"*", t.replace('"', "")))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

/// Localized-ish display name for a folder.
fn display_name_for(f: &RemoteFolder) -> String {
    match f.role {
        Some(FolderRole::Inbox) => return "Posta in arrivo".into(),
        Some(FolderRole::Sent) => return "Posta inviata".into(),
        Some(FolderRole::Drafts) => return "Bozze".into(),
        Some(FolderRole::Trash) => return "Cestino".into(),
        Some(FolderRole::Spam) => return "Posta indesiderata".into(),
        Some(FolderRole::Archive) => return "Archivio".into(),
        Some(FolderRole::All) => return "Tutti i messaggi".into(),
        Some(FolderRole::Flagged) => return "Speciali".into(),
        Some(FolderRole::Important) => return "Importanti".into(),
        None => {}
    }
    let leaf = match f.delimiter.as_deref().filter(|d| !d.is_empty()) {
        Some(d) => f.name.rsplit(d).next().unwrap_or(&f.name),
        None => &f.name,
    };
    crate::imap::utf7::decode(leaf)
}

fn sort_folders(folders: &mut [Folder]) {
    // Role folders first in a fixed order, then the rest alphabetically by full path.
    folders.sort_by(|a, b| {
        (
            a.account_id,
            FolderRole::order(a.role),
            a.name.to_lowercase(),
        )
            .cmp(&(
                b.account_id,
                FolderRole::order(b.role),
                b.name.to_lowercase(),
            ))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(store: &Store) -> AccountConfig {
        store
            .insert_account(&NewAccount {
                provider: Provider::Imap,
                email: "me@example.com".into(),
                display_name: "Me".into(),
                username: "me@example.com".into(),
                auth: AuthKind::Password,
                imap: ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 1143,
                    security: Security::None,
                },
                smtp: ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 1025,
                    security: Security::None,
                },
            })
            .unwrap()
    }

    fn msg(uid: u32, subject: &str, date: i64, flags: i64) -> NewMessage {
        NewMessage {
            uid,
            subject: subject.into(),
            from: Some(Addr::new(Some("Mario Rossi"), "mario@example.com")),
            to: vec![Addr::new(None, "me@example.com")],
            date,
            flags,
            ..Default::default()
        }
    }

    #[test]
    fn accounts_folders_messages_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let a = account(&s);
        assert_eq!(s.accounts().unwrap().len(), 1);
        s.sync_folders(
            a.id,
            &[
                RemoteFolder {
                    name: "INBOX".into(),
                    delimiter: Some("/".into()),
                    role: Some(FolderRole::Inbox),
                    selectable: true,
                },
                RemoteFolder {
                    name: "Work".into(),
                    delimiter: Some("/".into()),
                    role: None,
                    selectable: true,
                },
                RemoteFolder {
                    name: "Work/Clients".into(),
                    delimiter: Some("/".into()),
                    role: None,
                    selectable: true,
                },
            ],
        )
        .unwrap();
        let folders = s.folders(Some(a.id)).unwrap();
        assert_eq!(folders[0].role, Some(FolderRole::Inbox));
        let clients = folders.iter().find(|f| f.name == "Work/Clients").unwrap();
        assert_eq!(clients.depth, 1);
        assert_eq!(clients.display_name, "Clients");
        let inbox = s.folder_by_role(a.id, FolderRole::Inbox).unwrap().unwrap();

        let ids = s
            .upsert_messages(
                a.id,
                inbox.id,
                &[
                    msg(1, "Fattura marzo", 1000, 0),
                    msg(2, "Riunione domani", 2000, flags::SEEN),
                ],
            )
            .unwrap();
        assert_eq!(ids.len(), 2);
        // Re-upsert only updates flags.
        let again = s
            .upsert_messages(
                a.id,
                inbox.id,
                &[msg(1, "Fattura marzo", 1000, flags::SEEN)],
            )
            .unwrap();
        assert!(again.is_empty());

        let list = s
            .list_messages(ListScope::UnifiedInbox, None, 50, false)
            .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].subject, "Riunione domani");
        assert!(list[1].seen);

        let page = s
            .list_messages(
                ListScope::Folder(inbox.id),
                Some((list[0].date, list[0].id)),
                50,
                false,
            )
            .unwrap();
        assert_eq!(page.len(), 1);

        let hits = s.search("fattu", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        s.store_body(ids[1], b"raw", "snippet", "ordine del giorno budget", false)
            .unwrap();
        assert_eq!(s.search("budget", None, 10).unwrap().len(), 1);
        assert_eq!(s.search("riunione", None, 10).unwrap().len(), 1);

        assert_eq!(
            s.set_flags(ids[0], flags::FLAGGED, flags::SEEN).unwrap(),
            flags::FLAGGED
        );
        assert_eq!(s.inbox_unread().unwrap()[&a.id], 1);

        s.delete_uids(inbox.id, &[1]).unwrap();
        assert!(s.search("fattura", None, 10).unwrap().is_empty());

        s.touch_contacts(&[Addr::new(Some("Giulia Bianchi"), "giulia@example.com")])
            .unwrap();
        assert_eq!(
            s.suggest_contacts("giu", 5).unwrap()[0].address,
            "giulia@example.com"
        );
        assert_eq!(s.suggest_contacts("mario", 5).unwrap().len(), 1);

        s.delete_account(a.id).unwrap();
        assert!(s.folders(None).unwrap().is_empty());
    }

    #[test]
    fn fts_query_is_sanitized() {
        assert_eq!(fts_query("  ").as_deref(), None);
        assert_eq!(
            fts_query("ciao \"mondo\" OR*").as_deref(),
            Some("\"ciao\"* \"mondo\"* \"OR\"*")
        );
    }
}

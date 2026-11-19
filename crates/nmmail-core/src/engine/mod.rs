//! The engine: owns accounts, sync tasks, storage and events.
//!
//! Every account runs two tasks:
//! * a **watcher** holding a dedicated IMAP connection in IDLE on INBOX
//!   (push), never used for anything else;
//! * a **worker** that lazily opens a second connection for user actions
//!   (bodies, flags, moves, other folders) and closes it when unused.

mod account;
pub mod platform;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::{Notify, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::auth::{AuthManager, oauth, secrets::SecretStore};
use crate::compose::{self, Mode, Prepared, SendRequest};
use crate::error::{ErrorCode, bad_request, code_of, coded, not_found};
use crate::events::Event;
use crate::imap::conn::{self, Credentials};
use crate::mime;
use crate::model::*;
use crate::settings::Settings;
use crate::store::{ListScope, NewAccount, Store};
use account::{Job, JobOutput, WorkerMsg};
pub use platform::{Notification, NotifyItem, Platform};

const BODY_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone)]
struct Status {
    state: AccountState,
    error: Option<String>,
    last_sync: Option<i64>,
}

struct AccountRuntime {
    status: Status,
    worker: mpsc::UnboundedSender<WorkerMsg>,
    wake: Arc<Notify>,
    tasks: Vec<JoinHandle<()>>,
}

pub(crate) struct Inner {
    store: Store,
    settings: Arc<RwLock<Settings>>,
    auth: Arc<AuthManager>,
    events: broadcast::Sender<Event>,
    platform: Arc<dyn Platform>,
    accounts: Mutex<HashMap<AccountId, AccountRuntime>>,
    /// Bumped on resume from sleep: drop connections and reconnect now.
    reconnect: watch::Sender<u64>,
    /// Bumped on network changes / unlock: verify connections are alive.
    health: watch::Sender<u64>,
    outbox_kick: Notify,
    auth_cancel: Mutex<Option<Arc<Notify>>>,
    /// Messages with a visible toast (to retract them once read elsewhere).
    notified: Mutex<HashMap<MessageId, AccountId>>,
    ui_clients: AtomicUsize,
    started: Instant,
    last_unread: Mutex<Option<(u32, Option<String>)>>,
}

/// Cheap, cloneable handle to the engine.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

/// Parameters of `accounts.addImap`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImapAccountParams {
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub username: Option<String>,
    pub password: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_security: Security,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: Security,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    pub outbox_id: i64,
    pub queued: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonStatus {
    pub version: &'static str,
    pub pid: u32,
    pub uptime_sec: u64,
    pub memory_bytes: u64,
    pub accounts: Vec<Account>,
}

impl Engine {
    /// Starts the engine (must run inside a tokio runtime).
    pub fn start(store: Store, secrets: Arc<dyn SecretStore>, platform: Arc<dyn Platform>) -> Result<Engine> {
        let settings = Arc::new(RwLock::new(store.load_settings()?));
        let auth = Arc::new(AuthManager::new(secrets, settings.clone()));
        let (events, _) = broadcast::channel(256);
        let inner = Arc::new(Inner {
            store,
            settings,
            auth,
            events,
            platform,
            accounts: Mutex::new(HashMap::new()),
            reconnect: watch::channel(0).0,
            health: watch::channel(0).0,
            outbox_kick: Notify::new(),
            auth_cancel: Mutex::new(None),
            notified: Mutex::new(HashMap::new()),
            ui_clients: AtomicUsize::new(0),
            started: Instant::now(),
            last_unread: Mutex::new(None),
        });
        let engine = Engine { inner };
        for a in engine.inner.store.accounts()? {
            engine.inner.start_account(a.id);
        }
        tokio::spawn(account::outbox_task(engine.inner.clone()));
        tokio::spawn(maintenance_task(engine.inner.clone()));
        tokio::spawn(clock_jump_task(engine.inner.clone()));
        engine.inner.refresh_unread();
        Ok(engine)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings.read().unwrap().clone()
    }

    // ------------------------------------------------------------ OS signals

    /// The machine resumed from sleep: reconnect everything immediately.
    pub fn on_resume(&self) {
        info!("resume detected: reconnecting");
        self.inner.reconnect.send_modify(|v| *v += 1);
        self.inner.outbox_kick.notify_one();
    }

    /// Network configuration changed (or session unlocked): verify connections.
    pub fn on_network_change(&self) {
        self.inner.health.send_modify(|v| *v += 1);
        self.inner.outbox_kick.notify_one();
    }

    pub fn ui_attached(&self, attached: bool) {
        if attached {
            self.inner.ui_clients.fetch_add(1, Ordering::SeqCst);
        } else {
            self.inner.ui_clients.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub fn ui_connected(&self) -> bool {
        self.inner.ui_clients.load(Ordering::SeqCst) > 0
    }

    /// Shows the UI (from tray / notification click).
    pub fn show_ui(&self, message_id: Option<MessageId>) {
        let connected = self.ui_connected();
        if connected {
            let _ = self.inner.events.send(Event::UiShow { message_id });
        }
        self.inner.platform.show_ui(message_id, connected);
    }

    /// Handles a `nmmail:` URI (notification clicks and buttons).
    pub async fn activate(&self, uri: &str) -> Result<()> {
        let url = url::Url::parse(uri).map_err(|_| bad_request("invalid uri"))?;
        let action = url.host_str().unwrap_or_else(|| url.path().trim_matches('/'));
        let msg = url
            .query_pairs()
            .find(|(k, _)| k == "msg")
            .and_then(|(_, v)| v.parse::<MessageId>().ok());
        match (action, msg) {
            ("markread", Some(id)) => self.set_flags(&[id], Some(true), None).await,
            ("archive", Some(id)) => self.archive(&[id]).await,
            ("delete", Some(id)) => self.delete(&[id]).await,
            ("open", m) => {
                self.show_ui(m);
                Ok(())
            }
            _ => {
                self.show_ui(None);
                Ok(())
            }
        }
    }

    // --------------------------------------------------------------- status

    pub fn status(&self) -> Result<DaemonStatus> {
        Ok(DaemonStatus {
            version: env!("CARGO_PKG_VERSION"),
            pid: std::process::id(),
            uptime_sec: self.inner.started.elapsed().as_secs(),
            memory_bytes: self.inner.platform.memory_bytes(),
            accounts: self.accounts()?,
        })
    }

    // ------------------------------------------------------------- accounts

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let unread = self.inner.store.inbox_unread()?;
        Ok(self
            .inner
            .store
            .accounts()?
            .into_iter()
            .map(|c| self.inner.account_view(&c, &unread))
            .collect())
    }

    pub async fn add_oauth(&self, provider: Provider) -> Result<Account> {
        if provider == Provider::Imap {
            return Err(bad_request("use accounts.addImap for IMAP accounts"));
        }
        let cancel = Arc::new(Notify::new());
        *self.inner.auth_cancel.lock().unwrap() = Some(cancel.clone());
        let client = self.inner.auth.client_id(provider);
        let platform = self.inner.platform.clone();
        let open = move |u: &str| platform.open_url(u);
        let events = self.inner.events.clone();
        let progress = move |stage: oauth::Stage, url: Option<&str>| {
            let _ = events.send(Event::AuthProgress {
                stage: match stage {
                    oauth::Stage::Browser => "browser",
                    oauth::Stage::Exchanging => "exchanging",
                },
                url: url.map(|u| u.to_string()),
            });
        };
        let result = oauth::authorize(provider, &client, &open, &progress, cancel).await;
        *self.inner.auth_cancel.lock().unwrap() = None;
        let (tokens, identity) = result?;
        let _ = self.inner.events.send(Event::AuthProgress {
            stage: "connecting",
            url: None,
        });

        if let Some(existing) = self.inner.store.account_by_email(&identity.email)? {
            // Re-authentication of a known account.
            self.inner.auth.save_oauth(existing.id, &tokens)?;
            self.inner.restart_account(existing.id);
            let _ = self.inner.events.send(Event::AuthProgress { stage: "done", url: None });
            return self.account(existing.id);
        }

        let (imap, smtp) = oauth::servers(provider, &identity);
        let creds = Credentials::OAuth2 {
            user: identity.email.clone(),
            access_token: tokens.access_token.clone(),
        };
        let (mut session, _) = conn::connect(&imap, &creds).await.context("IMAP login")?;
        let _ = conn::tmo_for(Duration::from_secs(5), session.logout()).await;

        let cfg = self.inner.store.insert_account(&NewAccount {
            provider,
            email: identity.email.clone(),
            display_name: identity.name.clone().unwrap_or_else(|| identity.email.clone()),
            username: identity.email.clone(),
            auth: AuthKind::OAuth2,
            imap,
            smtp,
        })?;
        self.inner.auth.save_oauth(cfg.id, &tokens)?;
        self.inner.start_account(cfg.id);
        let _ = self.inner.events.send(Event::AccountsChanged {});
        let _ = self.inner.events.send(Event::AuthProgress { stage: "done", url: None });
        self.account(cfg.id)
    }

    pub fn cancel_auth(&self) {
        if let Some(c) = self.inner.auth_cancel.lock().unwrap().take() {
            c.notify_one();
        }
    }

    pub async fn add_imap(&self, p: ImapAccountParams) -> Result<Account> {
        let email = p.email.trim().to_string();
        if !email.contains('@') {
            return Err(bad_request("invalid e-mail address"));
        }
        if self.inner.store.account_by_email(&email)?.is_some() {
            return Err(bad_request("account already exists"));
        }
        let username = p.username.filter(|u| !u.trim().is_empty()).unwrap_or_else(|| email.clone());
        let imap = ServerConfig {
            host: p.imap_host.trim().to_string(),
            port: p.imap_port,
            security: p.imap_security,
        };
        let smtp = ServerConfig {
            host: p.smtp_host.trim().to_string(),
            port: p.smtp_port,
            security: p.smtp_security,
        };
        let creds = Credentials::Password {
            user: username.clone(),
            password: p.password.clone(),
        };
        let (mut session, _) = conn::connect(&imap, &creds).await?;
        let _ = conn::tmo_for(Duration::from_secs(5), session.logout()).await;
        let cfg = self.inner.store.insert_account(&NewAccount {
            provider: Provider::Imap,
            email: email.clone(),
            display_name: if p.display_name.trim().is_empty() {
                email.clone()
            } else {
                p.display_name.trim().to_string()
            },
            username,
            auth: AuthKind::Password,
            imap,
            smtp,
        })?;
        self.inner.auth.save_password(cfg.id, &p.password)?;
        self.inner.start_account(cfg.id);
        let _ = self.inner.events.send(Event::AccountsChanged {});
        self.account(cfg.id)
    }

    pub fn account(&self, id: AccountId) -> Result<Account> {
        let cfg = self.inner.store.account(id)?;
        let unread = self.inner.store.inbox_unread()?;
        Ok(self.inner.account_view(&cfg, &unread))
    }

    pub fn update_account(
        &self,
        id: AccountId,
        display_name: Option<&str>,
        notify: Option<bool>,
        notify_filter: Option<NotifyFilter>,
    ) -> Result<Account> {
        self.inner
            .store
            .update_account_prefs(id, display_name, notify, notify_filter)?;
        let a = self.account(id)?;
        let _ = self.inner.events.send(Event::AccountStatus(a.clone()));
        Ok(a)
    }

    pub fn remove_account(&self, id: AccountId) -> Result<()> {
        self.inner.store.account(id)?;
        self.inner.stop_account(id);
        self.inner.auth.forget(id);
        self.inner.store.delete_account(id)?;
        let _ = self.inner.events.send(Event::AccountsChanged {});
        self.inner.refresh_unread();
        Ok(())
    }

    // -------------------------------------------------------------- folders

    pub fn folders(&self, account_id: Option<AccountId>) -> Result<Vec<Folder>> {
        self.inner.store.folders(account_id)
    }

    pub fn sync_folder(&self, folder_id: FolderId) -> Result<()> {
        let f = self.inner.store.folder_state(folder_id)?;
        if f.role == Some(FolderRole::Inbox) {
            // INBOX belongs to the watcher (it decides about notifications).
            self.inner.wake(f.account_id);
        } else {
            self.inner.job(f.account_id, Job::SyncFolder(folder_id), None);
        }
        Ok(())
    }

    pub fn sync_now(&self, account_id: Option<AccountId>) {
        let ids: Vec<AccountId> = match account_id {
            Some(id) => vec![id],
            None => self.inner.accounts.lock().unwrap().keys().copied().collect(),
        };
        for id in ids {
            self.inner.wake(id);
            self.inner.job(id, Job::RefreshFolders, None);
        }
        self.inner.outbox_kick.notify_one();
    }

    // ------------------------------------------------------------- messages

    pub fn list_messages(
        &self,
        folder_id: Option<FolderId>,
        unified: bool,
        before: Option<(i64, MessageId)>,
        limit: u32,
        unread_only: bool,
    ) -> Result<(Vec<MessageSummary>, bool)> {
        let scope = match (folder_id, unified) {
            (Some(f), _) => ListScope::Folder(f),
            (None, true) => ListScope::UnifiedInbox,
            _ => return Err(bad_request("folderId or unified required")),
        };
        let limit = limit.clamp(1, 500);
        let mut v = self.inner.store.list_messages(scope, before, limit + 1, unread_only)?;
        let more = v.len() > limit as usize;
        v.truncate(limit as usize);
        Ok((v, more))
    }

    pub async fn load_older(&self, folder_id: FolderId, before: i64) -> Result<(Vec<MessageSummary>, bool)> {
        let f = self.inner.store.folder_state(folder_id)?;
        let out = self
            .inner
            .request(f.account_id, Job::LoadOlder { folder_id, before })
            .await?;
        let ids = match out {
            JobOutput::Ids(ids) => ids,
            _ => Vec::new(),
        };
        let more = !ids.is_empty();
        Ok((self.inner.store.summaries(&ids)?, more))
    }

    /// Raw message, fetched from the server if not cached.
    async fn raw(&self, id: MessageId) -> Result<Vec<u8>> {
        if let Some(raw) = self.inner.store.body(id)? {
            return Ok(raw);
        }
        let row = self.inner.store.message_row(id)?;
        let out = tokio::time::timeout(
            BODY_TIMEOUT,
            self.inner.request(row.summary.account_id, Job::FetchBody(id)),
        )
        .await
        .map_err(|_| coded(ErrorCode::Offline, "timed out downloading the message"))??;
        match out {
            JobOutput::Body(raw) => Ok(raw),
            _ => Err(not_found("message body")),
        }
    }

    pub async fn message(&self, id: MessageId, allow_remote: bool) -> Result<MessageDetail> {
        let raw = self.raw(id).await?;
        let row = self.inner.store.message_row(id)?;
        let detail = tokio::task::spawn_blocking(move || {
            let parsed = mime::parse_full(&raw);
            let rendered = crate::html::render_message(&parsed, &raw, allow_remote);
            let mut summary = row.summary.clone();
            if summary.snippet.is_empty() {
                summary.snippet = parsed.snippet.clone();
            }
            summary.has_attachments = parsed.attachments.iter().any(|a| !a.inline);
            MessageDetail {
                summary,
                cc: if parsed.headers.cc.is_empty() { row.cc.clone() } else { parsed.headers.cc.clone() },
                bcc: Vec::new(),
                reply_to: parsed.headers.reply_to.clone(),
                message_id_header: parsed.headers.message_id.clone(),
                html: Some(rendered.document),
                text: parsed.text.clone(),
                attachments: parsed.attachments.into_iter().filter(|a| !a.inline).collect(),
                has_remote_content: rendered.has_remote_content,
                remote_allowed: allow_remote,
            }
        })
        .await?;
        Ok(detail)
    }

    /// Groups messages by (account, folder) → (folder name, uids, ids).
    fn group(&self, ids: &[MessageId]) -> Result<HashMap<(AccountId, FolderId), (crate::store::MessageRow, Vec<u32>, Vec<MessageId>)>> {
        let mut groups: HashMap<(AccountId, FolderId), (crate::store::MessageRow, Vec<u32>, Vec<MessageId>)> =
            HashMap::new();
        for id in ids {
            let row = self.inner.store.message_row(*id)?;
            let key = (row.summary.account_id, row.summary.folder_id);
            let uid = row.uid;
            let e = groups.entry(key).or_insert_with(|| (row, Vec::new(), Vec::new()));
            e.1.push(uid);
            e.2.push(*id);
        }
        Ok(groups)
    }

    pub async fn set_flags(&self, ids: &[MessageId], seen: Option<bool>, flagged: Option<bool>) -> Result<()> {
        let groups = self.group(ids)?;
        for ((account_id, folder_id), (_, uids, mids)) in groups {
            for (flag, bit, value) in [("\\Seen", flags::SEEN, seen), ("\\Flagged", flags::FLAGGED, flagged)] {
                let Some(v) = value else { continue };
                for id in &mids {
                    if v {
                        self.inner.store.set_flags(*id, bit, 0)?;
                    } else {
                        self.inner.store.set_flags(*id, 0, bit)?;
                    }
                }
                self.inner.job(
                    account_id,
                    Job::Flags {
                        folder_id,
                        uids: uids.clone(),
                        flag: flag.to_string(),
                        add: v,
                    },
                    None,
                );
            }
            let _ = self.inner.events.send(Event::MessagesChanged {
                account_id,
                folder_ids: vec![folder_id],
            });
        }
        self.inner.refresh_unread();
        Ok(())
    }

    async fn move_to_role(&self, ids: &[MessageId], role: FolderRole) -> Result<()> {
        let groups = self.group(ids)?;
        for ((account_id, folder_id), (row, uids, mids)) in groups {
            let account = self.inner.store.account(account_id)?;
            let target = match self.inner.store.folder_by_role(account_id, role)? {
                Some(t) => Some(t),
                None if role == FolderRole::Archive && account.provider == Provider::Gmail => {
                    self.inner.store.folder_by_role(account_id, FolderRole::All)?
                }
                None => None,
            };
            let Some(target) = target else {
                if role == FolderRole::Trash {
                    // No trash folder: delete permanently.
                    self.inner.store.delete_messages(&mids)?;
                    self.inner.job(account_id, Job::Expunge { folder_id, uids }, None);
                    continue;
                }
                return Err(coded(ErrorCode::NotFound, "no archive folder on this account"));
            };
            if target.id == folder_id {
                if role == FolderRole::Trash {
                    self.inner.store.delete_messages(&mids)?;
                    self.inner.job(account_id, Job::Expunge { folder_id, uids }, None);
                }
                continue;
            }
            let _ = row;
            self.inner.store.delete_messages(&mids)?;
            self.inner.job(
                account_id,
                Job::Move {
                    folder_id,
                    uids,
                    target: target.name.clone(),
                },
                None,
            );
            let _ = self.inner.events.send(Event::MessagesChanged {
                account_id,
                folder_ids: vec![folder_id, target.id],
            });
        }
        self.inner.refresh_unread();
        Ok(())
    }

    pub async fn archive(&self, ids: &[MessageId]) -> Result<()> {
        self.move_to_role(ids, FolderRole::Archive).await
    }

    pub async fn delete(&self, ids: &[MessageId]) -> Result<()> {
        self.move_to_role(ids, FolderRole::Trash).await
    }

    pub async fn move_to(&self, ids: &[MessageId], target_folder: FolderId) -> Result<()> {
        let target = self.inner.store.folder_state(target_folder)?;
        let groups = self.group(ids)?;
        for ((account_id, folder_id), (_, uids, mids)) in groups {
            if account_id != target.account_id {
                return Err(bad_request("cannot move messages across accounts"));
            }
            if folder_id == target.id {
                continue;
            }
            self.inner.store.delete_messages(&mids)?;
            self.inner.job(
                account_id,
                Job::Move {
                    folder_id,
                    uids,
                    target: target.name.clone(),
                },
                None,
            );
            let _ = self.inner.events.send(Event::MessagesChanged {
                account_id,
                folder_ids: vec![folder_id, target.id],
            });
        }
        self.inner.refresh_unread();
        Ok(())
    }

    pub fn search(&self, query: &str, account_id: Option<AccountId>, limit: u32) -> Result<Vec<MessageSummary>> {
        self.inner.store.search(query, account_id, limit.clamp(1, 500))
    }

    pub async fn save_attachment(&self, id: MessageId, part_id: u32, path: &str) -> Result<String> {
        let raw = self.raw(id).await?;
        let (_, _, bytes) = tokio::task::spawn_blocking(move || mime::attachment(&raw, part_id))
            .await?
            .ok_or_else(|| not_found("attachment"))?;
        let path = std::path::PathBuf::from(path);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await.ok();
        }
        tokio::fs::write(&path, bytes).await.with_context(|| format!("writing {}", path.display()))?;
        Ok(path.to_string_lossy().into_owned())
    }

    pub fn suggest_contacts(&self, prefix: &str, limit: u32) -> Result<Vec<Addr>> {
        self.inner.store.suggest_contacts(prefix, limit.clamp(1, 50))
    }

    // -------------------------------------------------------------- compose

    pub async fn compose_prepare(&self, mode: Mode, message_id: MessageId, lang: &str) -> Result<Prepared> {
        let raw = self.raw(message_id).await?;
        let row = self.inner.store.message_row(message_id)?;
        let me = self.inner.store.account(row.summary.account_id)?;
        let lang = lang.to_string();
        Ok(tokio::task::spawn_blocking(move || {
            let parsed = mime::parse_full(&raw);
            compose::prepare(mode, &me, &row, &parsed, &lang)
        })
        .await?)
    }

    pub async fn compose_send(&self, req: SendRequest) -> Result<SendResult> {
        let account = self.inner.store.account(req.account_id)?;
        let recipients: Vec<String> = req
            .to
            .iter()
            .chain(req.cc.iter())
            .chain(req.bcc.iter())
            .map(|a| a.address.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();
        if recipients.is_empty() {
            return Err(bad_request("no recipients"));
        }
        let mut extra = Vec::new();
        for fa in &req.forward_attachments {
            let raw = self.raw(fa.message_id).await?;
            if let Some(a) = mime::attachment(&raw, fa.part_id) {
                extra.push(a);
            }
        }
        let from = Addr::new(Some(&account.display_name), &account.email);
        let (raw, _mid) = compose::build(&from, &req, extra)?;
        let all: Vec<Addr> = req.to.iter().chain(req.cc.iter()).chain(req.bcc.iter()).cloned().collect();
        self.inner.store.touch_contacts(&all)?;

        let attempt = tokio::time::timeout(
            Duration::from_secs(45),
            account::send_raw(&self.inner, &account, &recipients, &raw),
        )
        .await;
        match attempt {
            Ok(Ok(())) => {
                self.inner.after_sent(&account, req.reply_to_message_id, raw);
                Ok(SendResult {
                    outbox_id: 0,
                    queued: false,
                })
            }
            Ok(Err(e)) if !matches!(code_of(&e), ErrorCode::Offline | ErrorCode::AuthRequired) => Err(e),
            _ => {
                let id = self.inner.store.outbox_insert(
                    account.id,
                    &raw,
                    &account.email,
                    &recipients,
                    req.reply_to_message_id,
                )?;
                self.inner.outbox_kick.notify_one();
                Ok(SendResult {
                    outbox_id: id,
                    queued: true,
                })
            }
        }
    }

    // ------------------------------------------------------------- settings

    pub fn set_settings(&self, patch: &serde_json::Value) -> Result<Settings> {
        let old = self.settings();
        let new = old.merged(patch)?;
        if new.autostart != old.autostart
            && let Err(e) = self.inner.platform.set_autostart(new.autostart)
        {
            warn!("autostart: {e:#}");
        }
        self.inner.store.save_settings(&new)?;
        *self.inner.settings.write().unwrap() = new.clone();
        if new.google_client_id != old.google_client_id || new.microsoft_client_id != old.microsoft_client_id {
            // Accounts waiting for credentials may now be able to refresh.
            let ids: Vec<AccountId> = self.inner.accounts.lock().unwrap().keys().copied().collect();
            for id in ids {
                self.inner.wake(id);
            }
        }
        let _ = self.inner.events.send(Event::SettingsChanged(new.clone()));
        Ok(new)
    }

    pub fn pause_notifications(&self, until: Option<i64>) -> Result<Settings> {
        self.set_settings(&serde_json::json!({ "quietUntil": until }))
    }

    /// Makes sure the OS autostart entry matches the setting (called at startup).
    pub fn apply_autostart(&self) {
        let s = self.settings();
        if let Err(e) = self.inner.platform.set_autostart(s.autostart) {
            warn!("autostart: {e:#}");
        }
    }
}

impl Inner {
    fn account_view(&self, c: &AccountConfig, unread: &HashMap<AccountId, u32>) -> Account {
        let status = self
            .accounts
            .lock()
            .unwrap()
            .get(&c.id)
            .map(|r| r.status.clone())
            .unwrap_or(Status {
                state: AccountState::Offline,
                error: None,
                last_sync: None,
            });
        Account {
            id: c.id,
            provider: c.provider,
            email: c.email.clone(),
            display_name: c.display_name.clone(),
            state: status.state,
            error: status.error,
            last_sync: status.last_sync,
            unread: unread.get(&c.id).copied().unwrap_or(0),
            notify: c.notify,
            notify_filter: c.notify_filter,
        }
    }

    fn start_account(self: &Arc<Self>, id: AccountId) {
        let (tx, rx) = mpsc::unbounded_channel();
        let wake = Arc::new(Notify::new());
        let watcher = tokio::spawn(account::watcher(self.clone(), id, wake.clone()));
        let worker = tokio::spawn(account::worker(self.clone(), id, rx));
        self.accounts.lock().unwrap().insert(
            id,
            AccountRuntime {
                status: Status {
                    state: AccountState::Connecting,
                    error: None,
                    last_sync: None,
                },
                worker: tx,
                wake,
                tasks: vec![watcher, worker],
            },
        );
    }

    fn stop_account(&self, id: AccountId) {
        if let Some(rt) = self.accounts.lock().unwrap().remove(&id) {
            for t in rt.tasks {
                t.abort();
            }
        }
    }

    fn restart_account(self: &Arc<Self>, id: AccountId) {
        self.stop_account(id);
        self.start_account(id);
    }

    fn wake(&self, id: AccountId) {
        if let Some(rt) = self.accounts.lock().unwrap().get(&id) {
            rt.wake.notify_one();
        }
    }

    fn job(&self, id: AccountId, job: Job, reply: Option<oneshot::Sender<Result<JobOutput>>>) {
        let tx = self.accounts.lock().unwrap().get(&id).map(|r| r.worker.clone());
        match tx {
            Some(tx) => {
                if let Err(e) = tx.send(WorkerMsg { job, reply }) {
                    if let Some(r) = e.0.reply {
                        let _ = r.send(Err(coded(ErrorCode::Offline, "account not running")));
                    }
                }
            }
            None => {
                if let Some(r) = reply {
                    let _ = r.send(Err(not_found(format!("account {id}"))));
                }
            }
        }
    }

    async fn request(&self, id: AccountId, job: Job) -> Result<JobOutput> {
        let (tx, rx) = oneshot::channel();
        self.job(id, job, Some(tx));
        rx.await.map_err(|_| coded(ErrorCode::Offline, "worker stopped"))?
    }

    fn set_status(&self, id: AccountId, state: AccountState, error: Option<String>) {
        let changed = {
            let mut map = self.accounts.lock().unwrap();
            let Some(rt) = map.get_mut(&id) else { return };
            let changed = rt.status.state != state || rt.status.error != error;
            rt.status.state = state;
            rt.status.error = error;
            if state == AccountState::Online {
                rt.status.last_sync = Some(now_ms());
            }
            changed
        };
        if changed && let Ok(cfg) = self.store.account(id) {
            let unread = self.store.inbox_unread().unwrap_or_default();
            let _ = self.events.send(Event::AccountStatus(self.account_view(&cfg, &unread)));
            self.refresh_unread();
        }
    }

    /// Recomputes the unread badge and retracts toasts of messages read elsewhere.
    fn refresh_unread(&self) {
        let unread = self.store.inbox_unread().unwrap_or_default();
        let total: u32 = unread.values().sum();
        let problem = {
            let map = self.accounts.lock().unwrap();
            map.values()
                .find(|r| matches!(r.status.state, AccountState::AuthRequired | AccountState::Error))
                .map(|r| r.status.error.clone().unwrap_or_default())
        };
        let key = (total, problem.clone());
        let mut last = self.last_unread.lock().unwrap();
        if last.as_ref() != Some(&key) {
            *last = Some(key);
            self.platform.unread_changed(total, problem.as_deref());
        }
        drop(last);

        let shown: Vec<(MessageId, AccountId)> =
            self.notified.lock().unwrap().iter().map(|(m, a)| (*m, *a)).collect();
        if shown.is_empty() {
            return;
        }
        let ids: Vec<MessageId> = shown.iter().map(|(m, _)| *m).collect();
        let still: HashMap<MessageId, bool> = self
            .store
            .summaries(&ids)
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.id, s.seen))
            .collect();
        let mut gone: HashMap<AccountId, Vec<MessageId>> = HashMap::new();
        for (m, a) in shown {
            if still.get(&m).is_none_or(|seen| *seen) {
                gone.entry(a).or_default().push(m);
            }
        }
        if !gone.is_empty() {
            let mut n = self.notified.lock().unwrap();
            for (a, ids) in gone {
                for id in &ids {
                    n.remove(id);
                }
                self.platform.retract(a, &ids);
            }
        }
    }

    /// Decides whether and how to notify about freshly arrived messages.
    fn notify_new(&self, account: &AccountConfig, candidates: &[MessageId], allowed: Option<&HashSet<MessageId>>) {
        let settings = self.settings.read().unwrap().clone();
        let _ = self.events.send(Event::MailNew {
            account_id: account.id,
            message_ids: candidates.to_vec(),
        });
        if !settings.notifications_enabled || !account.notify || settings.is_quiet(now_ms()) {
            return;
        }
        let recent_cutoff = now_ms() - 24 * 3_600_000;
        let mut items: Vec<MessageSummary> = self
            .store
            .summaries(candidates)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.seen)
            .filter(|s| s.date >= recent_cutoff)
            .filter(|s| {
                !s.from
                    .as_ref()
                    .is_some_and(|f| f.address.eq_ignore_ascii_case(&account.email))
            })
            .filter(|s| allowed.is_none_or(|a| a.contains(&s.id)))
            .collect();
        if items.is_empty() {
            return;
        }
        items.sort_by(|a, b| b.date.cmp(&a.date));
        let total = items.len();
        {
            let mut n = self.notified.lock().unwrap();
            for s in &items {
                n.insert(s.id, account.id);
            }
        }
        let shown: Vec<NotifyItem> = items
            .iter()
            .take(3)
            .map(|s| NotifyItem {
                message_id: s.id,
                from_name: s.from.as_ref().map(|f| f.label().to_string()).unwrap_or_default(),
                from_address: s.from.as_ref().map(|f| f.address.clone()).unwrap_or_default(),
                subject: s.subject.clone(),
                snippet: s.snippet.clone(),
            })
            .collect();
        self.platform.notify(&Notification {
            account_id: account.id,
            account_email: account.email.clone(),
            account_name: account.display_name.clone(),
            items: shown,
            total,
            sound: settings.notification_sound,
            show_preview: settings.show_preview,
        });
    }

    fn after_sent(&self, account: &AccountConfig, reply_to: Option<MessageId>, raw: Vec<u8>) {
        if let Some(id) = reply_to
            && let Ok(row) = self.store.message_row(id)
        {
            let _ = self.store.set_flags(id, flags::ANSWERED, 0);
            self.job(
                account.id,
                Job::Flags {
                    folder_id: row.summary.folder_id,
                    uids: vec![row.uid],
                    flag: "\\Answered".into(),
                    add: true,
                },
                None,
            );
        }
        if !account.provider.server_saves_sent()
            && let Ok(Some(sent)) = self.store.folder_by_role(account.id, FolderRole::Sent)
        {
            self.job(
                account.id,
                Job::Append {
                    folder: sent.name,
                    raw,
                },
                None,
            );
        }
    }
}

/// Prunes the cache once a day.
async fn maintenance_task(inner: Arc<Inner>) {
    tokio::time::sleep(Duration::from_secs(300)).await;
    loop {
        let days = inner.settings.read().unwrap().cache_days;
        match inner.store.prune(days) {
            Ok(n) if n > 0 => info!(removed = n, "cache pruned"),
            Err(e) => warn!("prune: {e:#}"),
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
    }
}

/// Cross-platform sleep detector: a large wall-clock jump between ticks
/// means the machine was suspended.
async fn clock_jump_task(inner: Arc<Inner>) {
    const TICK: Duration = Duration::from_secs(60);
    let mut last = SystemTime::now();
    loop {
        tokio::time::sleep(TICK).await;
        let now = SystemTime::now();
        let elapsed = now.duration_since(last).unwrap_or_default();
        if elapsed > TICK + Duration::from_secs(45) {
            info!(gap_s = elapsed.as_secs(), "clock jump: assuming resume from sleep");
            inner.reconnect.send_modify(|v| *v += 1);
            inner.outbox_kick.notify_one();
        }
        last = now;
    }
}

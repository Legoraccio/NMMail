//! Per-account tasks: the IDLE watcher (push) and the on-demand worker.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_imap::extensions::idle::IdleResponse;
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::{debug, info, warn};

use super::Inner;
use crate::error::{ErrorCode, code_of, coded};
use crate::events::Event;
use crate::imap::conn::{self, Caps, Session, tmo, tmo_for};
use crate::imap::sync::{self, Changes, SyncMode};
use crate::mime;
use crate::model::*;

/// IDLE is re-issued this often (Gmail drops it after 29 min, NATs often sooner;
/// TCP keepalive covers the gap).
const IDLE_RENEW: Duration = Duration::from_secs(8 * 60);
/// A healthy server answers DONE immediately; silence means a dead link.
const DONE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the watcher reconciles flags/expunges of the whole cache window.
const FULL_RESYNC: Duration = Duration::from_secs(30 * 60);
/// Polling interval for servers without IDLE.
const POLL: Duration = Duration::from_secs(60);
/// Worker connections are closed after this much inactivity.
const WORKER_IDLE_CLOSE: Duration = Duration::from_secs(180);
/// New messages up to this size are downloaded entirely on arrival.
const PREFETCH_MAX: u32 = 2 * 1024 * 1024;
const PREFETCH_COUNT: usize = 10;

struct Backoff(usize);

impl Backoff {
    const STEPS: [u64; 7] = [1, 2, 5, 10, 30, 60, 120];
    fn next(&mut self) -> Duration {
        let d = Self::STEPS[self.0.min(Self::STEPS.len() - 1)];
        self.0 += 1;
        Duration::from_secs(d)
    }
    fn reset(&mut self) {
        self.0 = 0;
    }
}

enum Exit {
    /// Reconnect right away (resume from sleep).
    Reconnect,
}

/// Keeps one connection in IDLE on INBOX and processes pushes.
pub(super) async fn watcher(inner: Arc<Inner>, account_id: AccountId, wake: Arc<Notify>) {
    let mut backoff = Backoff(0);
    let mut reconnect_rx = inner.reconnect.subscribe();
    let mut health_rx = inner.health.subscribe();
    reconnect_rx.borrow_and_update();
    health_rx.borrow_and_update();
    loop {
        inner.set_status(account_id, AccountState::Connecting, None);
        let res = watch_once(
            &inner,
            account_id,
            &wake,
            &mut backoff,
            &mut reconnect_rx,
            &mut health_rx,
        )
        .await;
        let delay = match res {
            Ok(Exit::Reconnect) => {
                backoff.reset();
                continue;
            }
            Err(e) => {
                let code = code_of(&e);
                let msg = format!("{e:#}");
                match code {
                    ErrorCode::AuthRequired => {
                        warn!(account_id, "authentication required: {msg}");
                        inner.auth.invalidate(account_id);
                        inner.set_status(account_id, AccountState::AuthRequired, Some(msg));
                        Duration::from_secs(15 * 60)
                    }
                    ErrorCode::NotFound if inner.store.account(account_id).is_err() => {
                        // Account removed underneath us.
                        return;
                    }
                    ErrorCode::Offline => {
                        debug!(account_id, "offline: {msg}");
                        inner.set_status(account_id, AccountState::Offline, Some(msg));
                        backoff.next()
                    }
                    _ => {
                        warn!(account_id, "watcher error: {msg}");
                        inner.set_status(account_id, AccountState::Error, Some(msg));
                        backoff.next()
                    }
                }
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = wake.notified() => backoff.reset(),
            _ = reconnect_rx.changed() => backoff.reset(),
            _ = health_rx.changed() => backoff.reset(),
        }
    }
}

async fn watch_once(
    inner: &Arc<Inner>,
    account_id: AccountId,
    wake: &Notify,
    backoff: &mut Backoff,
    reconnect_rx: &mut tokio::sync::watch::Receiver<u64>,
    health_rx: &mut tokio::sync::watch::Receiver<u64>,
) -> Result<Exit> {
    let account = inner.store.account(account_id)?;
    let creds = inner.auth.credentials(&account).await?;
    let (mut session, caps) = conn::connect(&account.imap, &creds).await?;
    info!(
        account_id,
        idle = caps.idle,
        gmail = caps.gmail,
        condstore = caps.condstore,
        "watcher connected"
    );
    backoff.reset();
    // Folder list + flags of other folders are the worker's job.
    inner.job(account_id, Job::RefreshFolders, None);

    let mut mode = SyncMode::Full;
    let mut last_full = Instant::now();
    loop {
        if mode != SyncMode::Full && last_full.elapsed() >= FULL_RESYNC {
            mode = SyncMode::Full;
        }
        if mode == SyncMode::Full {
            last_full = Instant::now();
        }
        let cache_days = inner.settings.read().unwrap().cache_days;
        // Re-resolved every round: the folder list refresh may have recreated it.
        let folder = inner
            .store
            .ensure_folder(account_id, "INBOX", Some(FolderRole::Inbox))?;
        let inbox = folder.clone();
        let out =
            sync::sync_folder(&mut session, &inner.store, &folder, caps, cache_days, mode).await?;
        inner.set_status(account_id, AccountState::Online, None);
        if out.changed {
            let _ = inner.events.send(Event::MessagesChanged {
                account_id,
                folder_ids: vec![inbox.id],
            });
        }
        if out.initial {
            inner.job(
                account_id,
                Job::Prefetch {
                    folder_id: inbox.id,
                    limit: 60,
                },
                None,
            );
        } else if !out.new_ids.is_empty() {
            on_new_mail(
                inner,
                &account,
                &mut session,
                caps,
                &out.new_ids,
                &out.new_uids,
            )
            .await;
        }
        inner.refresh_unread();

        // Changes that arrived while we were busy.
        let pending = sync::drain_unsolicited(&mut session);
        if pending.any() {
            mode = mode_for(pending);
            continue;
        }

        if !caps.idle {
            tokio::select! {
                _ = tokio::time::sleep(POLL) => {}
                _ = wake.notified() => {}
                _ = health_rx.changed() => {}
                _ = reconnect_rx.changed() => return Ok(Exit::Reconnect),
            }
            tmo_for(DONE_TIMEOUT, session.noop()).await?;
            mode = SyncMode::Recent;
            continue;
        }

        // ---- IDLE ----
        let mut handle = session.idle();
        tmo(handle.init()).await?;
        enum Wakeup {
            Server(Result<IdleResponse, async_imap::error::Error>),
            Manual,
            Health,
            Reconnect,
        }
        let (wakeup, resp) = {
            let (fut, stop) = handle.wait_with_timeout(IDLE_RENEW);
            tokio::pin!(fut);
            let w = tokio::select! {
                r = &mut fut => Wakeup::Server(r),
                _ = wake.notified() => Wakeup::Manual,
                _ = health_rx.changed() => Wakeup::Health,
                _ = reconnect_rx.changed() => Wakeup::Reconnect,
            };
            drop(stop);
            match w {
                Wakeup::Reconnect => return Ok(Exit::Reconnect),
                Wakeup::Server(r) => (Wakeup::Server(Ok(IdleResponse::ManualInterrupt)), Some(r)),
                other => {
                    // Interrupted locally: let the idle future observe the stop.
                    let _ = tokio::time::timeout(Duration::from_secs(2), fut.as_mut()).await;
                    (other, None)
                }
            }
        };
        let mut changes = Changes::default();
        if let Some(r) = resp {
            match r? {
                IdleResponse::NewData(data) => changes.merge(sync::classify(data.parsed())),
                IdleResponse::Timeout | IdleResponse::ManualInterrupt => {}
            }
        }
        // DONE doubles as a liveness probe.
        session = tmo_for(DONE_TIMEOUT, handle.done()).await?;
        changes.merge(sync::drain_unsolicited(&mut session));
        mode = match wakeup {
            // User asked for a refresh ("sync now", INBOX opened): reconcile everything.
            Wakeup::Manual => SyncMode::Full,
            // Server push, renewal timeout or network/health check.
            _ if changes.any() => mode_for(changes),
            _ => SyncMode::NewOnly,
        };
    }
}

fn mode_for(c: Changes) -> SyncMode {
    if c.expunge || c.flags {
        SyncMode::Recent
    } else {
        SyncMode::NewOnly
    }
}

/// Downloads new messages (for preview + instant open) and notifies.
async fn on_new_mail(
    inner: &Arc<Inner>,
    account: &AccountConfig,
    session: &mut Session,
    caps: Caps,
    ids: &[MessageId],
    uids: &[u32],
) {
    info!(account_id = account.id, count = ids.len(), "new mail");
    // Preferences may have changed since the watcher connected.
    let fresh = inner.store.account(account.id).ok();
    let account = fresh.as_ref().unwrap_or(account);
    let summaries = inner.store.summaries(ids).unwrap_or_default();
    let mut want: Vec<(MessageId, u32)> = Vec::new();
    for s in summaries.iter().take(PREFETCH_COUNT) {
        if s.size <= PREFETCH_MAX
            && let Ok(row) = inner.store.message_row(s.id)
        {
            want.push((s.id, row.uid));
        }
    }
    if !want.is_empty() {
        let fetch_uids: Vec<u32> = want.iter().map(|(_, u)| *u).collect();
        match sync::fetch_bodies(session, &fetch_uids).await {
            Ok(bodies) => {
                for (uid, raw) in bodies {
                    if let Some((id, _)) = want.iter().find(|(_, u)| *u == uid) {
                        store_body(inner, *id, &raw);
                    }
                }
            }
            Err(e) => debug!("prefetch failed: {e:#}"),
        }
    }
    let allowed: Option<HashSet<MessageId>> =
        if account.notify_filter == NotifyFilter::Primary && caps.gmail {
            match sync::gmail_primary(session, uids).await {
                Ok(primary) => Some(
                    ids.iter()
                        .filter(|id| {
                            inner
                                .store
                                .message_row(**id)
                                .map(|r| primary.contains(&r.uid))
                                .unwrap_or(true)
                        })
                        .copied()
                        .collect(),
                ),
                Err(_) => None,
            }
        } else {
            None
        };
    inner.notify_new(account, ids, allowed.as_ref());
}

fn store_body(inner: &Inner, id: MessageId, raw: &[u8]) {
    let parsed = mime::parse_full(raw);
    let has_att = parsed.attachments.iter().any(|a| !a.inline);
    if let Err(e) = inner.store.store_body(
        id,
        raw,
        &parsed.snippet,
        parsed.text.as_deref().unwrap_or(""),
        has_att,
    ) {
        warn!("store body: {e:#}");
    }
}

// ------------------------------------------------------------------ worker

#[derive(Debug, Clone)]
pub(super) enum Job {
    RefreshFolders,
    SyncFolder(FolderId),
    FetchBody(MessageId),
    Flags {
        folder_id: FolderId,
        uids: Vec<u32>,
        flag: String,
        add: bool,
    },
    Move {
        folder_id: FolderId,
        uids: Vec<u32>,
        target: String,
    },
    Expunge {
        folder_id: FolderId,
        uids: Vec<u32>,
    },
    LoadOlder {
        folder_id: FolderId,
        before: i64,
    },
    Append {
        folder: String,
        raw: Vec<u8>,
    },
    Prefetch {
        folder_id: FolderId,
        limit: u32,
    },
}

impl Job {
    /// Jobs that must eventually reach the server even if we are offline now.
    fn must_retry(&self) -> bool {
        matches!(
            self,
            Job::Flags { .. } | Job::Move { .. } | Job::Expunge { .. } | Job::Append { .. }
        )
    }
}

pub(super) enum JobOutput {
    None,
    Body(Vec<u8>),
    Ids(Vec<MessageId>),
}

pub(super) struct WorkerMsg {
    pub job: Job,
    pub reply: Option<oneshot::Sender<Result<JobOutput>>>,
}

struct Conn {
    session: Session,
    caps: Caps,
    selected: Option<String>,
}

pub(super) async fn worker(
    inner: Arc<Inner>,
    account_id: AccountId,
    mut rx: mpsc::UnboundedReceiver<WorkerMsg>,
) {
    let mut conn: Option<Conn> = None;
    let mut retry: VecDeque<Job> = VecDeque::new();
    let mut health_rx = inner.health.subscribe();
    let mut reconnect_rx = inner.reconnect.subscribe();
    loop {
        let msg = tokio::select! {
            m = rx.recv() => match m {
                Some(m) => Some(m),
                None => break,
            },
            _ = tokio::time::sleep(WORKER_IDLE_CLOSE), if conn.is_some() => {
                if let Some(mut c) = conn.take() {
                    let _ = tmo_for(Duration::from_secs(5), c.session.logout()).await;
                }
                continue;
            }
            _ = health_rx.changed(), if !retry.is_empty() => None,
            _ = reconnect_rx.changed() => {
                conn = None;
                if retry.is_empty() { continue } else { None }
            }
            _ = tokio::time::sleep(Duration::from_secs(60)), if !retry.is_empty() => None,
        };

        // Replay jobs that failed while offline, oldest first.
        while let Some(job) = retry.pop_front() {
            if let Err(e) = run_job(&inner, account_id, &mut conn, &job).await {
                if code_of(&e) == ErrorCode::Offline {
                    retry.push_front(job);
                    break;
                }
                warn!("retried job failed: {e:#}");
            }
        }

        let Some(WorkerMsg { job, reply }) = msg else {
            continue;
        };
        let res = run_job(&inner, account_id, &mut conn, &job).await;
        if let Err(e) = &res
            && code_of(e) == ErrorCode::Offline
            && job.must_retry()
            && reply.is_none()
        {
            debug!("queueing job for retry: {e:#}");
            retry.push_back(job);
            continue;
        }
        if let Err(e) = &res {
            if code_of(e) == ErrorCode::NotFound {
                debug!("job skipped: {e:#}");
            } else {
                warn!(account_id, "job failed: {e:#}");
            }
        }
        if let Some(r) = reply {
            let _ = r.send(res);
        }
    }
}

async fn ensure_conn<'a>(
    inner: &Inner,
    account_id: AccountId,
    conn: &'a mut Option<Conn>,
) -> Result<&'a mut Conn> {
    if conn.is_none() {
        let account = inner.store.account(account_id)?;
        let creds = inner.auth.credentials(&account).await?;
        let (session, caps) = conn::connect(&account.imap, &creds).await?;
        *conn = Some(Conn {
            session,
            caps,
            selected: None,
        });
    }
    Ok(conn.as_mut().expect("connected"))
}

async fn select(c: &mut Conn, name: &str) -> Result<()> {
    if c.selected.as_deref() != Some(name) {
        tmo(c.session.select(sync::wire_name(name))).await?;
        c.selected = Some(name.to_string());
    }
    sync::drain_unsolicited(&mut c.session);
    Ok(())
}

/// Runs a job; a panic is contained (and the connection dropped) instead of
/// killing the worker.
async fn run_job(
    inner: &Inner,
    account_id: AccountId,
    conn: &mut Option<Conn>,
    job: &Job,
) -> Result<JobOutput> {
    use futures::FutureExt as _;
    let res = std::panic::AssertUnwindSafe(run_job_retrying(inner, account_id, conn, job))
        .catch_unwind()
        .await;
    match res {
        Ok(r) => r,
        Err(_) => {
            tracing::error!(account_id, ?job, "worker job panicked");
            *conn = None;
            Err(anyhow::anyhow!("internal error while running {job:?}"))
        }
    }
}

/// Runs a job, reconnecting once if a reused connection turned out dead.
async fn run_job_retrying(
    inner: &Inner,
    account_id: AccountId,
    conn: &mut Option<Conn>,
    job: &Job,
) -> Result<JobOutput> {
    let reused = conn.is_some();
    match run_job_once(inner, account_id, conn, job).await {
        Err(e) if reused && code_of(&e) == ErrorCode::Offline => {
            *conn = None;
            run_job_once(inner, account_id, conn, job).await
        }
        Err(e) => {
            if code_of(&e) == ErrorCode::Offline {
                *conn = None;
            }
            Err(e)
        }
        ok => ok,
    }
}

async fn run_job_once(
    inner: &Inner,
    account_id: AccountId,
    conn: &mut Option<Conn>,
    job: &Job,
) -> Result<JobOutput> {
    match job {
        Job::RefreshFolders => {
            let c = ensure_conn(inner, account_id, conn).await?;
            let remote = sync::list_folders(&mut c.session).await?;
            if inner.store.sync_folders(account_id, &remote)? {
                let _ = inner.events.send(Event::FoldersChanged { account_id });
            }
            Ok(JobOutput::None)
        }
        Job::SyncFolder(folder_id) => {
            let folder = inner.store.folder_state(*folder_id)?;
            let cache_days = inner.settings.read().unwrap().cache_days;
            let c = ensure_conn(inner, account_id, conn).await?;
            c.selected = Some(folder.name.clone());
            let out = sync::sync_folder(
                &mut c.session,
                &inner.store,
                &folder,
                c.caps,
                cache_days,
                SyncMode::Full,
            )
            .await?;
            let _ = inner.events.send(Event::MessagesChanged {
                account_id,
                folder_ids: vec![*folder_id],
            });
            if out.initial {
                let _ = prefetch(inner, c, *folder_id, 30).await;
            }
            Ok(JobOutput::None)
        }
        Job::FetchBody(id) => {
            let row = inner.store.message_row(*id)?;
            let c = ensure_conn(inner, account_id, conn).await?;
            select(c, &row.folder_name).await?;
            let bodies = sync::fetch_bodies(&mut c.session, &[row.uid]).await?;
            let raw = bodies
                .into_iter()
                .find(|(u, _)| *u == row.uid)
                .map(|(_, r)| r)
                .ok_or_else(|| coded(ErrorCode::NotFound, "message no longer on the server"))?;
            store_body(inner, *id, &raw);
            Ok(JobOutput::Body(raw))
        }
        Job::Flags {
            folder_id,
            uids,
            flag,
            add,
        } => {
            let folder = inner.store.folder_state(*folder_id)?;
            let c = ensure_conn(inner, account_id, conn).await?;
            select(c, &folder.name).await?;
            sync::store_flag(&mut c.session, uids, flag, *add).await?;
            Ok(JobOutput::None)
        }
        Job::Move {
            folder_id,
            uids,
            target,
        } => {
            let folder = inner.store.folder_state(*folder_id)?;
            let c = ensure_conn(inner, account_id, conn).await?;
            select(c, &folder.name).await?;
            let caps = c.caps;
            sync::move_uids(&mut c.session, uids, target, caps).await?;
            Ok(JobOutput::None)
        }
        Job::Expunge { folder_id, uids } => {
            let folder = inner.store.folder_state(*folder_id)?;
            let c = ensure_conn(inner, account_id, conn).await?;
            select(c, &folder.name).await?;
            let caps = c.caps;
            sync::expunge_uids(&mut c.session, uids, caps).await?;
            Ok(JobOutput::None)
        }
        Job::LoadOlder { folder_id, before } => {
            let folder = inner.store.folder_state(*folder_id)?;
            let c = ensure_conn(inner, account_id, conn).await?;
            c.selected = Some(folder.name.clone());
            let caps = c.caps;
            let ids =
                sync::load_older(&mut c.session, &inner.store, &folder, caps, *before, 50).await?;
            Ok(JobOutput::Ids(ids))
        }
        Job::Append { folder, raw } => {
            let c = ensure_conn(inner, account_id, conn).await?;
            tmo(c
                .session
                .append(sync::wire_name(folder), Some("(\\Seen)"), None, raw))
            .await?;
            Ok(JobOutput::None)
        }
        Job::Prefetch { folder_id, limit } => {
            let c = ensure_conn(inner, account_id, conn).await?;
            prefetch(inner, c, *folder_id, *limit).await?;
            Ok(JobOutput::None)
        }
    }
}

/// Downloads bodies of the newest messages lacking one (snippets, instant open).
async fn prefetch(inner: &Inner, c: &mut Conn, folder_id: FolderId, limit: u32) -> Result<()> {
    let folder = inner.store.folder_state(folder_id)?;
    let todo = inner
        .store
        .messages_without_body(folder_id, limit, 512 * 1024)?;
    if todo.is_empty() {
        return Ok(());
    }
    select(c, &folder.name).await?;
    for chunk in todo.chunks(10) {
        let uids: Vec<u32> = chunk.iter().map(|(_, u)| *u).collect();
        for (uid, raw) in sync::fetch_bodies(&mut c.session, &uids).await? {
            if let Some((id, _)) = chunk.iter().find(|(_, u)| *u == uid) {
                store_body(inner, *id, &raw);
            }
        }
    }
    let _ = inner.events.send(Event::MessagesChanged {
        account_id: folder.account_id,
        folder_ids: vec![folder_id],
    });
    Ok(())
}

// ------------------------------------------------------------------ outbox

pub(super) async fn send_raw(
    inner: &Inner,
    account: &AccountConfig,
    recipients: &[String],
    raw: &[u8],
) -> Result<()> {
    let creds = inner.auth.credentials(account).await?;
    let res = crate::smtp::send(&account.smtp, &creds, &account.email, recipients, raw).await;
    if let Err(e) = &res
        && code_of(e) == ErrorCode::AuthRequired
    {
        inner.auth.invalidate(account.id);
    }
    res
}

/// Retries queued messages when connectivity returns.
pub(super) async fn outbox_task(inner: Arc<Inner>) {
    let mut health_rx = inner.health.subscribe();
    loop {
        let items = inner.store.outbox().unwrap_or_default();
        for item in items {
            let Ok(account) = inner.store.account(item.account_id) else {
                let _ = inner.store.outbox_done(item.id);
                continue;
            };
            let res = tokio::time::timeout(
                Duration::from_secs(90),
                send_raw(&inner, &account, &item.recipients, &item.raw),
            )
            .await
            .unwrap_or_else(|_| Err(coded(ErrorCode::Offline, "SMTP timeout")));
            match res {
                Ok(()) => {
                    info!(outbox_id = item.id, "queued message sent");
                    let _ = inner.store.outbox_done(item.id);
                    let _ = inner.events.send(Event::OutboxSent { outbox_id: item.id });
                    inner.after_sent(&account, item.reply_to_message, item.raw.clone());
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let _ = inner.store.outbox_failed(item.id, &msg);
                    let permanent =
                        !matches!(code_of(&e), ErrorCode::Offline | ErrorCode::AuthRequired);
                    if permanent && item.attempts + 1 >= 3 {
                        let _ = inner.store.outbox_done(item.id);
                        let _ = inner.events.send(Event::OutboxFailed {
                            outbox_id: item.id,
                            error: msg,
                        });
                    }
                }
            }
        }
        let pending = !inner.store.outbox().unwrap_or_default().is_empty();
        tokio::select! {
            _ = inner.outbox_kick.notified() => {}
            _ = health_rx.changed() => {}
            _ = tokio::time::sleep(Duration::from_secs(120)), if pending => {}
        }
    }
}

//! End-to-end tests of the engine against the in-memory IMAP/SMTP server.
//!
//! These verify the "never miss" guarantees: push latency, catch-up after a
//! dropped connection, recovery from half-open links, plus user actions.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nmmail_core::auth::secrets::MemoryStore;
use nmmail_core::engine::{Engine, ImapAccountParams, Notification, Platform};
use nmmail_core::events::Event;
use nmmail_core::model::*;
use nmmail_core::store::Store;
use nmmail_testserver::{Config, TestServer};
use tokio::sync::broadcast;

const USER: &str = "test@nmmail.local";

#[derive(Default)]
struct Recorder {
    notes: Mutex<Vec<Notification>>,
    unread: Mutex<Vec<u32>>,
}

impl Platform for Recorder {
    fn open_url(&self, _url: &str) {}
    fn notify(&self, n: &Notification) {
        self.notes.lock().unwrap().push(n.clone());
    }
    fn retract(&self, _a: AccountId, _ids: &[MessageId]) {}
    fn unread_changed(&self, total: u32, _p: Option<&str>) {
        self.unread.lock().unwrap().push(total);
    }
    fn set_autostart(&self, _e: bool, _u: bool) -> anyhow::Result<()> {
        Ok(())
    }
    fn show_ui(&self, _m: Option<MessageId>, _c: bool) {}
}

struct Env {
    server: TestServer,
    engine: Engine,
    events: broadcast::Receiver<Event>,
    platform: Arc<Recorder>,
    account: Account,
    _dir: tempfile::TempDir,
}

fn mail(subject: &str, body: &str) -> Vec<u8> {
    let date = chrono::Utc::now().to_rfc2822();
    format!(
        "From: Mario Rossi <mario@example.com>\r\nTo: {USER}\r\nSubject: {subject}\r\nDate: {date}\r\n\
Message-ID: <{}.{}@example.com>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n",
        subject.len(),
        nmmail_core::model::now_ms()
    )
    .into_bytes()
}

async fn wait_for<T>(
    events: &mut broadcast::Receiver<Event>,
    timeout: Duration,
    mut pred: impl FnMut(&Event) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        match tokio::time::timeout(left, events.recv()).await {
            Ok(Ok(ev)) => {
                if let Some(v) = pred(&ev) {
                    return Some(v);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            _ => return None,
        }
    }
}

fn init_logs() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();
}

async fn setup(preload: usize) -> Env {
    setup_cfg(Config::default(), preload).await
}

async fn setup_cfg(config: Config, preload: usize) -> Env {
    init_logs();
    let server = TestServer::start(config).await.unwrap();
    for i in 0..preload {
        server.deliver(USER, "INBOX", &mail(&format!("Vecchio {i}"), "contenuto"));
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db")).unwrap();
    let platform = Arc::new(Recorder::default());
    let engine = Engine::start(store, Arc::new(MemoryStore::default()), platform.clone()).unwrap();
    let mut events = engine.subscribe();
    let account = engine
        .add_imap(ImapAccountParams {
            email: USER.into(),
            display_name: "Test".into(),
            username: None,
            password: "test".into(),
            imap_host: "127.0.0.1".into(),
            imap_port: server.imap_addr().port(),
            imap_security: Security::None,
            smtp_host: "127.0.0.1".into(),
            smtp_port: server.smtp_addr().port(),
            smtp_security: Security::None,
        })
        .await
        .unwrap();
    let id = account.id;
    // Generous timeout: shared CI runners can be very slow under parallel tests.
    let mut seen_states = Vec::new();
    let online = wait_for(&mut events, Duration::from_secs(30), |e| match e {
        Event::AccountStatus(a) if a.id == id => {
            seen_states.push(format!("{:?} {:?}", a.state, a.error));
            (a.state == AccountState::Online).then_some(())
        }
        _ => None,
    })
    .await;
    assert!(
        online.is_some(),
        "account never came online; states seen: {seen_states:?}; current: {:?}",
        engine.accounts().ok()
    );
    // Let the watcher enter IDLE.
    tokio::time::sleep(Duration::from_millis(300)).await;
    Env {
        server,
        engine,
        events,
        platform,
        account,
        _dir: dir,
    }
}

async fn wait_new_mail(env: &mut Env, timeout: Duration) -> Option<Vec<MessageId>> {
    let id = env.account.id;
    wait_for(&mut env.events, timeout, |e| match e {
        Event::MailNew {
            account_id,
            message_ids,
        } if *account_id == id => Some(message_ids.clone()),
        _ => None,
    })
    .await
}

fn inbox(env: &Env) -> Folder {
    env.engine
        .folders(Some(env.account.id))
        .unwrap()
        .into_iter()
        .find(|f| f.role == Some(FolderRole::Inbox))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn initial_sync_does_not_notify_and_push_is_instant() {
    let mut env = setup(5).await;
    assert_eq!(inbox(&env).total, 5);
    assert!(
        env.platform.notes.lock().unwrap().is_empty(),
        "initial sync must not notify"
    );

    let t0 = Instant::now();
    env.server
        .deliver(USER, "INBOX", &mail("Fattura urgente", "Ecco la fattura"));
    let ids = wait_new_mail(&mut env, Duration::from_secs(5))
        .await
        .expect("push not received");
    let latency = t0.elapsed();
    assert_eq!(ids.len(), 1);
    assert!(
        latency < Duration::from_millis(2000),
        "push latency too high: {latency:?}"
    );

    let notes = env.platform.notes.lock().unwrap().clone();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].items[0].subject, "Fattura urgente");
    assert_eq!(
        notes[0].items[0].snippet, "Ecco la fattura",
        "body should be prefetched for the toast"
    );
    println!("push latency: {latency:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn catches_up_after_connection_drop() {
    let mut env = setup(0).await;
    env.server.drop_all_connections();
    env.server
        .deliver(USER, "INBOX", &mail("Mentre eri offline 1", "a"));
    env.server
        .deliver(USER, "INBOX", &mail("Mentre eri offline 2", "b"));
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    while got.len() < 2 && Instant::now() < deadline {
        if let Some(ids) = wait_new_mail(&mut env, Duration::from_secs(15)).await {
            got.extend(ids);
        }
    }
    assert_eq!(
        got.len(),
        2,
        "messages delivered while disconnected were missed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_reconnects_from_half_open_link() {
    let mut env = setup(0).await;
    // The link goes silent (as after sleep / Wi-Fi change) but is not closed.
    env.server.freeze_connections(true);
    env.server
        .deliver(USER, "INBOX", &mail("Dopo il resume", "x"));
    let t0 = Instant::now();
    env.engine.on_resume();
    let ids = wait_new_mail(&mut env, Duration::from_secs(10))
        .await
        .expect("not recovered after resume");
    assert_eq!(ids.len(), 1);
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "resume recovery too slow: {:?}",
        t0.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn network_change_detects_dead_link() {
    let mut env = setup(0).await;
    env.server.freeze_connections(true);
    env.server
        .deliver(USER, "INBOX", &mail("Dopo cambio rete", "x"));
    env.engine.on_network_change();
    // DONE gets no answer → 10 s watchdog → reconnect → catch-up.
    let ids = wait_new_mail(&mut env, Duration::from_secs(25))
        .await
        .expect("dead link not detected");
    assert_eq!(ids.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_archive_delete_reach_the_server() {
    let mut env = setup(3).await;
    let (list, _) = env
        .engine
        .list_messages(None, true, None, 50, false)
        .unwrap();
    assert_eq!(list.len(), 3);

    env.engine
        .set_flags(&[list[0].id], Some(true), Some(true))
        .await
        .unwrap();
    env.engine.archive(&[list[1].id]).await.unwrap();
    env.engine.delete(&[list[2].id]).await.unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let inbox = env.server.messages(USER, "INBOX");
        let archive = env.server.messages(USER, "Archive");
        let trash = env.server.messages(USER, "Trash");
        let seen = inbox.iter().any(|m| {
            m.flags.iter().any(|f| f == "\\Seen") && m.flags.iter().any(|f| f == "\\Flagged")
        });
        if inbox.len() == 1 && archive.len() == 1 && trash.len() == 1 && seen {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "server state not updated: inbox={inbox:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Local cache agrees.
    let (list, _) = env
        .engine
        .list_messages(None, true, None, 50, false)
        .unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].seen && list[0].flagged);
    let _ = &mut env.events;
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_changed_elsewhere_are_pushed() {
    let env = setup(1).await;
    let uid = env.server.messages(USER, "INBOX")[0].uid;
    env.server.set_flags(USER, "INBOX", uid, &["\\Seen"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (list, _) = env
            .engine
            .list_messages(None, true, None, 50, false)
            .unwrap();
        if list[0].seen {
            break;
        }
        assert!(Instant::now() < deadline, "flag change not propagated");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    env.server.expunge_uid(USER, "INBOX", uid);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !env
        .engine
        .list_messages(None, true, None, 50, false)
        .unwrap()
        .0
        .is_empty()
    {
        assert!(Instant::now() < deadline, "expunge not propagated");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn read_reply_and_send() {
    let mut env = setup(0).await;
    env.server.deliver(
        USER,
        "INBOX",
        b"From: Anna <anna@nmmail.local>\r\nTo: test@nmmail.local\r\nSubject: Pranzo?\r\n\
Message-ID: <pranzo@nmmail.local>\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
<p>Ci vediamo <b>domani</b>? <img src=\"https://tracker.example/p.gif\"></p>\r\n",
    );
    let ids = wait_new_mail(&mut env, Duration::from_secs(5))
        .await
        .unwrap();
    let detail = env.engine.message(ids[0], false).await.unwrap();
    let html = detail.html.unwrap();
    assert!(html.contains("<b>domani</b>"));
    assert!(!html.contains("tracker.example"));
    assert!(detail.has_remote_content);

    let prep = env
        .engine
        .compose_prepare(nmmail_core::compose::Mode::Reply, ids[0], "it")
        .await
        .unwrap();
    assert_eq!(prep.subject, "Re: Pranzo?");
    assert_eq!(prep.to[0].address, "anna@nmmail.local");

    let res = env
        .engine
        .compose_send(nmmail_core::compose::SendRequest {
            account_id: env.account.id,
            to: vec![Addr::new(Some("Me"), USER)],
            cc: vec![],
            bcc: vec![],
            subject: prep.subject.clone(),
            html: format!("<p>Sì!</p>{}", prep.html),
            text: String::new(),
            attachments: vec![],
            forward_attachments: vec![],
            in_reply_to: prep.in_reply_to.clone(),
            references: prep.references.clone(),
            reply_to_message_id: Some(ids[0]),
        })
        .await
        .unwrap();
    assert!(!res.queued);
    // Sent to ourselves: it arrives through SMTP → INBOX → IDLE push.
    let new = wait_new_mail(&mut env, Duration::from_secs(5))
        .await
        .expect("sent mail not received");
    let m = env.engine.message(new[0], false).await.unwrap();
    assert_eq!(m.summary.subject, "Re: Pranzo?");
    // Replied message got \Answered on the server.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let answered = env
            .server
            .messages(USER, "INBOX")
            .iter()
            .any(|m| m.flags.iter().any(|f| f == "\\Answered"));
        if answered {
            break;
        }
        assert!(Instant::now() < deadline, "\\Answered not set");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Search finds it.
    assert!(!env.engine.search("pranzo", None, 10).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_reports_auth_required() {
    let server = TestServer::start(Config::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db")).unwrap();
    let engine = Engine::start(
        store,
        Arc::new(MemoryStore::default()),
        Arc::new(Recorder::default()),
    )
    .unwrap();
    let err = engine
        .add_imap(ImapAccountParams {
            email: USER.into(),
            display_name: String::new(),
            username: None,
            password: "wrong".into(),
            imap_host: "127.0.0.1".into(),
            imap_port: server.imap_addr().port(),
            imap_security: Security::None,
            smtp_host: "127.0.0.1".into(),
            smtp_port: server.smtp_addr().port(),
            smtp_security: Security::None,
        })
        .await
        .unwrap_err();
    assert_eq!(
        nmmail_core::error::code_of(&err),
        nmmail_core::error::ErrorCode::AuthRequired
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gmail_primary_filter_and_archive_to_all_mail() {
    let mut env = setup_cfg(
        Config {
            gmail_mode: true,
            ..Config::default()
        },
        0,
    )
    .await;
    env.engine
        .update_account(env.account.id, None, None, Some(NotifyFilter::Primary))
        .unwrap();

    let mut promo = mail("Offerta imperdibile", "sconti");
    promo.splice(
        0..0,
        b"X-Test-Category: promotions
"
        .iter()
        .copied(),
    );
    env.server.deliver(USER, "INBOX", &promo);
    wait_new_mail(&mut env, Duration::from_secs(5))
        .await
        .expect("promotion not received");
    assert!(
        env.platform.notes.lock().unwrap().is_empty(),
        "promotions must not notify with the Primary filter"
    );

    env.server
        .deliver(USER, "INBOX", &mail("Dal capo", "Vieni nel mio ufficio"));
    let ids = wait_new_mail(&mut env, Duration::from_secs(5))
        .await
        .expect("primary mail not received");
    {
        let notes = env.platform.notes.lock().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].items[0].subject, "Dal capo");
    }

    env.engine.archive(&ids).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let in_inbox = env
            .server
            .messages(USER, "INBOX")
            .iter()
            .any(|m| String::from_utf8_lossy(&m.raw).contains("Dal capo"));
        let in_all = env
            .server
            .messages(USER, "[Gmail]/All Mail")
            .iter()
            .any(|m| String::from_utf8_lossy(&m.raw).contains("Dal capo"));
        if !in_inbox && in_all {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "archive did not reach [Gmail]/All Mail"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn xoauth2_login_for_imap_and_smtp() {
    use nmmail_core::imap::conn::{Credentials, connect};
    let server = TestServer::start(Config::default()).await.unwrap();
    let imap = ServerConfig {
        host: "127.0.0.1".into(),
        port: server.imap_addr().port(),
        security: Security::None,
    };
    let smtp = ServerConfig {
        host: "127.0.0.1".into(),
        port: server.smtp_addr().port(),
        security: Security::None,
    };
    let good = Credentials::OAuth2 {
        user: USER.into(),
        access_token: "test-token".into(),
    };
    let (mut session, caps) = connect(&imap, &good).await.unwrap();
    assert!(caps.idle && caps.mv);
    session.logout().await.unwrap();

    let bad = Credentials::OAuth2 {
        user: USER.into(),
        access_token: "expired".into(),
    };
    let err = connect(&imap, &bad).await.unwrap_err();
    assert_eq!(
        nmmail_core::error::code_of(&err),
        nmmail_core::error::ErrorCode::AuthRequired
    );

    nmmail_core::smtp::send(
        &smtp,
        &good,
        USER,
        &[USER.to_string()],
        &mail("Via XOAUTH2", "ok"),
    )
    .await
    .unwrap();
    assert_eq!(server.messages(USER, "INBOX").len(), 1);
    let err = nmmail_core::smtp::send(&smtp, &bad, USER, &[USER.to_string()], &mail("x", "y"))
        .await
        .unwrap_err();
    assert_eq!(
        nmmail_core::error::code_of(&err),
        nmmail_core::error::ErrorCode::AuthRequired
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_in_again_updates_the_existing_account() {
    let mut env = setup(1).await;
    let params = |password: &str| ImapAccountParams {
        email: USER.into(),
        display_name: String::new(),
        username: None,
        password: password.into(),
        imap_host: "127.0.0.1".into(),
        imap_port: env.server.imap_addr().port(),
        imap_security: Security::None,
        smtp_host: "127.0.0.1".into(),
        smtp_port: env.server.smtp_addr().port(),
        smtp_security: Security::None,
    };
    // A wrong password is rejected and changes nothing.
    assert!(env.engine.add_imap(params("wrong")).await.is_err());
    let again = env.engine.add_imap(params("test")).await.unwrap();
    assert_eq!(again.id, env.account.id);
    assert_eq!(env.engine.accounts().unwrap().len(), 1);
    // The restarted watcher still receives push.
    let id = again.id;
    wait_for(&mut env.events, Duration::from_secs(10), |e| match e {
        Event::AccountStatus(a) if a.id == id && a.state == AccountState::Online => Some(()),
        _ => None,
    })
    .await
    .expect("account not back online");
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.server
        .deliver(USER, "INBOX", &mail("Dopo il nuovo accesso", "ok"));
    assert!(
        wait_new_mail(&mut env, Duration::from_secs(5))
            .await
            .is_some()
    );
}

/// Waits until the account's folder list (refreshed by the worker) has `name`.
async fn wait_folder(env: &Env, name: &str) -> Folder {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(f) = env
            .engine
            .folders(Some(env.account.id))
            .unwrap()
            .into_iter()
            .find(|f| f.name == name)
        {
            return f;
        }
        assert!(Instant::now() < deadline, "folder {name} never listed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Waits until a folder's cache holds `total` messages.
async fn wait_total(env: &Env, folder_id: FolderId, total: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let f = env
            .engine
            .folders(Some(env.account.id))
            .unwrap()
            .into_iter()
            .find(|f| f.id == folder_id)
            .unwrap();
        if f.total == total {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "folder total stuck at {}",
            f.total
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn monitored_folder_is_pushed_and_notified() {
    let mut env = setup(0).await;
    // A folder filled by a server-side rule, already holding an old message.
    assert!(env.server.create_mailbox(USER, "Lavoro"));
    env.server
        .deliver(USER, "Lavoro", &mail("Vecchio messaggio", "gia' qui"));
    env.engine.sync_now(Some(env.account.id));
    let work = wait_folder(&env, "Lavoro").await;
    assert!(!work.monitored);

    let work = env.engine.set_folder_monitored(work.id, true).unwrap();
    assert!(work.monitored);
    // First sync of the folder: cached, never notified.
    wait_total(&env, work.id, 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(env.platform.notes.lock().unwrap().is_empty());

    let t0 = Instant::now();
    env.server.deliver(
        USER,
        "Lavoro",
        &mail("Spostata da una regola", "Arriva direttamente in Lavoro"),
    );
    let ids = wait_new_mail(&mut env, Duration::from_secs(5))
        .await
        .expect("push from the monitored folder not received");
    let latency = t0.elapsed();
    assert_eq!(ids.len(), 1);
    assert!(latency < Duration::from_millis(2000), "latency {latency:?}");
    {
        let notes = env.platform.notes.lock().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].items[0].subject, "Spostata da una regola");
        assert_eq!(notes[0].folder.as_deref(), Some("Lavoro"));
    }

    // INBOX cannot be turned off; turning the folder off stops its watcher.
    let inbox = inbox(&env);
    assert!(
        env.engine
            .set_folder_monitored(inbox.id, false)
            .unwrap()
            .monitored
    );
    assert!(
        !env.engine
            .set_folder_monitored(work.id, false)
            .unwrap()
            .monitored
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    env.server
        .deliver(USER, "Lavoro", &mail("Dopo la disattivazione", "silenzio"));
    assert!(
        wait_new_mail(&mut env, Duration::from_millis(1500))
            .await
            .is_none()
    );
    assert_eq!(env.platform.notes.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn monitored_folders_are_capped_per_account() {
    let env = setup(0).await;
    for i in 1..=6 {
        assert!(env.server.create_mailbox(USER, &format!("Regola{i}")));
    }
    env.engine.sync_now(Some(env.account.id));
    let mut ids = Vec::new();
    for i in 1..=6 {
        ids.push(wait_folder(&env, &format!("Regola{i}")).await.id);
    }
    for id in &ids[..5] {
        assert!(
            env.engine
                .set_folder_monitored(*id, true)
                .unwrap()
                .monitored
        );
    }
    let err = env.engine.set_folder_monitored(ids[5], true).unwrap_err();
    assert_eq!(
        nmmail_core::error::code_of(&err),
        nmmail_core::error::ErrorCode::BadRequest
    );
    // Re-enabling an already monitored folder is not counted twice.
    assert!(
        env.engine
            .set_folder_monitored(ids[0], true)
            .unwrap()
            .monitored
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gmail_all_mail_notifies_only_mail_that_skipped_the_inbox() {
    let mut env = setup_cfg(
        Config {
            gmail_mode: true,
            ..Config::default()
        },
        0,
    )
    .await;
    let all_mail = wait_folder(&env, "[Gmail]/All Mail").await;
    assert_eq!(all_mail.role, Some(FolderRole::All));
    env.engine.set_folder_monitored(all_mail.id, true).unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;

    // 1. Normal mail: INBOX plus its copy in All Mail (same X-GM-MSGID, as Gmail does).
    let uid = env
        .server
        .deliver(USER, "INBOX", &mail("Normale", "in arrivo"));
    wait_new_mail(&mut env, Duration::from_secs(5))
        .await
        .expect("INBOX push");
    {
        let imap = ServerConfig {
            host: "127.0.0.1".into(),
            port: env.server.imap_addr().port(),
            security: Security::None,
        };
        let creds = nmmail_core::imap::conn::Credentials::Password {
            user: USER.into(),
            password: "test".into(),
        };
        let (mut s, _) = nmmail_core::imap::conn::connect(&imap, &creds)
            .await
            .unwrap();
        s.select("INBOX").await.unwrap();
        s.uid_copy(uid.to_string(), "[Gmail]/All Mail")
            .await
            .unwrap();
        s.logout().await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(
        env.platform.notes.lock().unwrap().len(),
        1,
        "the All Mail copy of an INBOX message must not notify again"
    );

    // 2. Mail filtered past the inbox: only in All Mail.
    env.server.deliver(
        USER,
        "[Gmail]/All Mail",
        &mail("Filtrata fuori dalla posta in arrivo", "etichetta"),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while env.platform.notes.lock().unwrap().len() < 2 {
        assert!(Instant::now() < deadline, "skip-inbox mail not notified");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let notes = env.platform.notes.lock().unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0].items[0].subject, "Normale");
    assert_eq!(notes[0].folder, None);
    assert_eq!(
        notes[1].items[0].subject,
        "Filtrata fuori dalla posta in arrivo"
    );
    assert_eq!(notes[1].folder.as_deref(), Some("Tutti i messaggi"));
}

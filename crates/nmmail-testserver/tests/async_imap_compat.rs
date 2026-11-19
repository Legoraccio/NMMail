//! Compatibility tests with async-imap 0.12 (the IMAP client used by the engine).

mod common;

use std::time::{Duration, Instant};

use async_imap::error::Error;
use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::{BodyStructure, MailboxDatum, MessageSection, Response, SectionPath};
use async_imap::types::{Fetch, Flag, Name, NameAttribute};
use async_imap::{Authenticator, Client, Session};
use common::*;
use futures::TryStreamExt;
use nmmail_testserver::chrono::{FixedOffset, TimeZone};
use nmmail_testserver::{Config, TestServer};
use tokio::net::TcpStream;

struct XOAuth2 {
    user: String,
    token: String,
}

impl Authenticator for &XOAuth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
    }
}

async fn connect(server: &TestServer) -> Client<TcpStream> {
    let stream = TcpStream::connect(server.imap_addr()).await.unwrap();
    let mut client = Client::new(stream);
    let greeting = client.read_response().await.unwrap().expect("greeting");
    assert!(matches!(greeting.parsed(), Response::Data { .. }));
    client
}

async fn login(server: &TestServer) -> Session<TcpStream> {
    connect(server)
        .await
        .login(USER, PASSWORD)
        .await
        .map_err(|(err, _)| err)
        .expect("login")
}

fn raw(subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: \"Rossi, Mario\" <mario@example.com>\r\nTo: Test <{USER}>\r\nCc: anna@example.com\r\n\
         Subject: {subject}\r\nDate: Mon, 3 Mar 2025 10:00:00 +0100\r\nMessage-ID: <{}@example.com>\r\n\
         X-Other: ignored\r\n\r\n{body}\r\n",
        subject.replace(' ', "-")
    )
    .into_bytes()
}

#[tokio::test]
async fn login_and_select() {
    let server = start().await;
    server.deliver(USER, "INBOX", &raw("uno", "a"));
    server.deliver_with_flags(USER, "INBOX", &raw("due", "b"), &["\\Seen"], None);

    let err = connect(&server)
        .await
        .login(USER, "wrong")
        .await
        .err()
        .unwrap()
        .0;
    assert!(matches!(err, Error::No(_)), "{err:?}");

    let mut session = login(&server).await;
    let caps = session.capabilities().await.unwrap();
    for cap in ["IDLE", "MOVE", "UIDPLUS", "SPECIAL-USE", "ID"] {
        assert!(caps.has_str(cap), "{cap}");
    }
    let mailbox = session.select("INBOX").await.unwrap();
    assert_eq!(mailbox.exists, 2);
    assert_eq!(mailbox.recent, 2);
    assert_eq!(mailbox.unseen, Some(1));
    assert_eq!(mailbox.uid_next, Some(3));
    assert_eq!(mailbox.uid_validity, server.uid_validity(USER, "INBOX"));
    assert!(mailbox.flags.contains(&Flag::Seen));
    assert!(mailbox.permanent_flags.contains(&Flag::MayCreate));
    let id = session
        .id([("name", Some("nmmail-tests"))])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        id.get("name").map(String::as_str),
        Some("nmmail-testserver")
    );
    session.noop().await.unwrap();
    session.logout().await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn authenticate_xoauth2() {
    let server = start().await;
    let good = XOAuth2 {
        user: USER.to_string(),
        token: "test-token".to_string(),
    };
    let mut session = connect(&server)
        .await
        .authenticate("XOAUTH2", &good)
        .await
        .map_err(|(err, _)| err)
        .expect("XOAUTH2 authentication");
    session.select("INBOX").await.unwrap();
    session.logout().await.unwrap();

    let bad = XOAuth2 {
        user: USER.to_string(),
        token: "expired-token".to_string(),
    };
    let err = connect(&server)
        .await
        .authenticate("XOAUTH2", &bad)
        .await
        .map(drop)
        .expect_err("authentication must fail")
        .0;
    assert!(matches!(err, Error::No(_)), "{err:?}");

    // An unknown user with the right token fails too.
    let stranger = XOAuth2 {
        user: "nobody@nmmail.local".to_string(),
        token: "test-token".to_string(),
    };
    assert!(
        connect(&server)
            .await
            .authenticate("XOAUTH2", &stranger)
            .await
            .is_err()
    );
    server.shutdown().await;
}

#[tokio::test]
async fn uid_fetch_headers_and_full_body() {
    let server = start().await;
    let date = FixedOffset::east_opt(3600)
        .unwrap()
        .with_ymd_and_hms(2025, 3, 3, 10, 0, 5)
        .unwrap();
    let raws = [
        raw("primo messaggio", "uno"),
        raw("secondo", "due"),
        raw("terzo", "tre"),
    ];
    server.deliver_with_flags(
        USER,
        "INBOX",
        &raws[0],
        &["\\Seen", "$Forwarded"],
        Some(date),
    );
    server.deliver(USER, "INBOX", &raws[1]);
    server.deliver_with_flags(USER, "INBOX", &raws[2], &["\\Flagged"], None);

    let mut session = login(&server).await;
    session.select("INBOX").await.unwrap();
    let fetches: Vec<Fetch> = session
        .uid_fetch(
            "1:*",
            "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER.FIELDS (FROM TO CC SUBJECT DATE MESSAGE-ID)])",
        )
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(fetches.len(), 3);
    for (i, fetch) in fetches.iter().enumerate() {
        assert_eq!(fetch.message, i as u32 + 1);
        assert_eq!(fetch.uid, Some(i as u32 + 1));
        assert_eq!(fetch.size, Some(raws[i].len() as u32));
        assert!(fetch.internal_date().is_some());
        let header = String::from_utf8(fetch.header().expect("header fields").to_vec()).unwrap();
        assert!(
            header.starts_with("From: \"Rossi, Mario\" <mario@example.com>\r\n"),
            "{header}"
        );
        assert!(header.contains("Cc: anna@example.com\r\n") && header.contains("Message-ID: <"));
        assert!(!header.contains("X-Other"));
        assert!(header.ends_with("\r\n\r\n"));
    }
    assert_eq!(fetches[0].internal_date(), Some(date));
    let flags: Vec<Flag<'_>> = fetches[0].flags().collect();
    assert!(flags.contains(&Flag::Seen) && flags.contains(&Flag::Custom("$Forwarded".into())));
    assert!(flags.contains(&Flag::Recent));
    assert!(fetches[2].flags().any(|f| f == Flag::Flagged));

    let full: Vec<Fetch> = session
        .uid_fetch("2", "(UID BODY.PEEK[])")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].uid, Some(2));
    assert_eq!(full[0].body(), Some(raws[1].as_slice()));
    assert!(
        !server.messages(USER, "INBOX")[1]
            .flags
            .contains(&"\\Seen".to_string())
    );

    // Non-PEEK fetch: \Seen is set and reported.
    let text: Vec<Fetch> = session
        .uid_fetch("2", "BODY[TEXT]")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(text[0].uid, Some(2));
    assert_eq!(text[0].text(), Some(&b"due\r\n"[..]));
    assert!(text[0].flags().any(|f| f == Flag::Seen));

    // ENVELOPE / BODYSTRUCTURE / RFC822.* parse with imap-proto.
    let rich: Vec<Fetch> = session
        .uid_fetch("1:*", "(ENVELOPE BODYSTRUCTURE RFC822.HEADER RFC822.SIZE)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let envelope = rich[0].envelope().expect("envelope");
    assert_eq!(envelope.subject.as_deref(), Some(&b"primo messaggio"[..]));
    let from = envelope.from.as_ref().unwrap();
    assert_eq!(from[0].name.as_deref(), Some(&b"Rossi, Mario"[..]));
    assert_eq!(from[0].mailbox.as_deref(), Some(&b"mario"[..]));
    assert_eq!(from[0].host.as_deref(), Some(&b"example.com"[..]));
    assert_eq!(envelope.sender, envelope.from);
    assert!(matches!(
        rich[0].bodystructure(),
        Some(BodyStructure::Text { .. })
    ));
    assert!(rich[0].header().is_some());
    session.logout().await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn search_store_move_status_list() {
    let server = start().await;
    server.deliver(USER, "INBOX", &raw("fattura marzo", "importo"));
    server.deliver(USER, "INBOX", &raw("pranzo", "giovedi"));
    server.deliver_with_flags(
        USER,
        "INBOX",
        &raw("fattura aprile", "importo"),
        &["\\Seen"],
        None,
    );

    let mut session = login(&server).await;
    session.select("INBOX").await.unwrap();
    let mut unseen: Vec<u32> = session
        .uid_search("UNSEEN")
        .await
        .unwrap()
        .into_iter()
        .collect();
    unseen.sort_unstable();
    assert_eq!(unseen, vec![1, 2]);
    let mut hits: Vec<u32> = session
        .uid_search("SUBJECT \"fattura\" UID 2:*")
        .await
        .unwrap()
        .into_iter()
        .collect();
    hits.sort_unstable();
    assert_eq!(hits, vec![3]);
    assert!(
        session
            .uid_search("SUBJECT nessuno")
            .await
            .unwrap()
            .is_empty()
    );

    let updates: Vec<Fetch> = session
        .uid_store("1", "+FLAGS (\\Seen \\Flagged)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].uid, Some(1));
    let flags: Vec<Flag<'_>> = updates[0].flags().collect();
    assert!(flags.contains(&Flag::Seen) && flags.contains(&Flag::Flagged));
    assert_eq!(
        server.messages(USER, "INBOX")[0].flags,
        vec!["\\Flagged".to_string(), "\\Seen".to_string()]
    );

    session.uid_mv("2", "Archive").await.unwrap();
    assert_eq!(server.messages(USER, "INBOX").len(), 2);
    let archived = server.messages(USER, "Archive");
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].raw, raw("pranzo", "giovedi"));
    session.uid_copy("1", "Trash").await.unwrap();
    assert_eq!(server.messages(USER, "Trash").len(), 1);

    let status = session
        .status("Archive", "(MESSAGES RECENT UIDNEXT UIDVALIDITY UNSEEN)")
        .await
        .unwrap();
    assert_eq!(status.exists, 1);
    assert_eq!(status.uid_next, Some(2));
    assert_eq!(status.unseen, Some(1));
    assert_eq!(status.uid_validity, server.uid_validity(USER, "Archive"));

    let names: Vec<Name> = session
        .list(Some(""), Some("*"))
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let find = |name: &str| {
        names
            .iter()
            .find(|n| n.name() == name)
            .unwrap_or_else(|| panic!("{name} not listed"))
    };
    assert_eq!(find("INBOX").delimiter(), Some("/"));
    for (name, attribute) in [
        ("Sent", NameAttribute::Sent),
        ("Drafts", NameAttribute::Drafts),
        ("Trash", NameAttribute::Trash),
        ("Junk", NameAttribute::Junk),
        ("Archive", NameAttribute::Archive),
    ] {
        assert!(find(name).attributes().contains(&attribute), "{name}");
    }
    session.create("Lavoro/Progetti").await.unwrap();
    let names: Vec<Name> = session
        .list(Some(""), Some("Lavoro/%"))
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].name(), "Lavoro/Progetti");

    // Delete flow: \Deleted + UID EXPUNGE.
    session
        .uid_store("3", "+FLAGS.SILENT (\\Deleted)")
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let expunged: Vec<u32> = session
        .uid_expunge("3")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(expunged, vec![2]);
    assert_eq!(server.messages(USER, "INBOX").len(), 1);

    // APPEND (used for drafts).
    session
        .append("Drafts", Some("(\\Draft)"), None, raw("bozza", "testo"))
        .await
        .unwrap();
    assert_eq!(
        server.messages(USER, "Drafts")[0].flags,
        vec!["\\Draft".to_string()]
    );
    session.logout().await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn idle_wakes_up_on_delivery() {
    let server = start().await;
    server.deliver(USER, "INBOX", &raw("vecchia", "x"));
    let mut session = login(&server).await;
    let mailbox = session.select("INBOX").await.unwrap();
    assert_eq!(mailbox.exists, 1);

    let mut idle = session.idle();
    idle.init().await.unwrap();
    let (wait, _stop) = idle.wait_with_timeout(Duration::from_secs(10));
    let delivered = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = Instant::now();
        let uid = server.deliver(USER, "INBOX", &raw("nuova", "ciao"));
        (uid, started)
    };
    let (response, (uid, started)) = tokio::join!(wait, delivered);
    let latency = started.elapsed();
    match response.unwrap() {
        IdleResponse::NewData(data) => {
            assert!(
                matches!(
                    data.parsed(),
                    Response::MailboxData(MailboxDatum::Exists(2))
                ),
                "{:?}",
                data.parsed()
            );
        }
        other => panic!("unexpected IDLE outcome: {other:?}"),
    }
    assert!(
        latency < Duration::from_millis(200),
        "IDLE latency {latency:?}"
    );
    drop(_stop);
    let mut session = idle.done().await.unwrap();

    // The session is usable after DONE; catch-up by UID finds the new message.
    let fetched: Vec<Fetch> = session
        .uid_fetch(format!("{}:*", uid), "(UID FLAGS BODY.PEEK[])")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].uid, Some(uid));
    assert_eq!(fetched[0].body(), Some(raw("nuova", "ciao").as_slice()));

    // IDLE timeout path, then a flag change from another client while idling.
    let mut idle = session.idle();
    idle.init().await.unwrap();
    let (wait, _stop) = idle.wait_with_timeout(Duration::from_millis(200));
    assert_eq!(wait.await.unwrap(), IdleResponse::Timeout);
    let (wait, _stop) = idle.wait_with_timeout(Duration::from_secs(10));
    let flag = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.set_flags(USER, "INBOX", 1, &["\\Seen"]);
    };
    let (response, ()) = tokio::join!(wait, flag);
    match response.unwrap() {
        IdleResponse::NewData(data) => {
            assert!(
                matches!(data.parsed(), Response::Fetch(1, _)),
                "{:?}",
                data.parsed()
            );
        }
        other => panic!("unexpected IDLE outcome: {other:?}"),
    }
    let mut session = idle.done().await.unwrap();
    session.noop().await.unwrap();
    session.logout().await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn idle_detects_dropped_connection() {
    let server = start().await;
    let mut session = login(&server).await;
    session.select("INBOX").await.unwrap();
    let mut idle = session.idle();
    idle.init().await.unwrap();
    let (wait, _stop) = idle.wait_with_timeout(Duration::from_secs(10));
    let drop_all = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.drop_all_connections();
    };
    let (response, ()) = tokio::join!(wait, drop_all);
    // A reset is reported as an error, a clean EOF as ManualInterrupt: never NewData.
    assert!(
        !matches!(response, Ok(IdleResponse::NewData(_))),
        "unexpected {response:?}"
    );
    assert!(idle.done().await.is_err());
    assert!(wait_until(|| server.imap_connection_count() == 0).await);
    server.shutdown().await;
}

#[tokio::test]
async fn gmail_extensions() {
    let server = start_with(Config {
        gmail_mode: true,
        ..Config::default()
    })
    .await;
    server.deliver(USER, "INBOX", &raw("primaria", "x"));
    let promo = [
        &b"X-Test-Category: promotions\r\n"[..],
        &raw("offerta", "sconto"),
    ]
    .concat();
    server.deliver(USER, "INBOX", &promo);
    let reply = [
        &b"In-Reply-To: <primaria@example.com>\r\n"[..],
        &raw("Re: primaria", "risposta"),
    ]
    .concat();
    server.deliver(USER, "INBOX", &reply);

    let mut session = login(&server).await;
    session.select("INBOX").await.unwrap();
    let mut primary: Vec<u32> = session
        .uid_search("X-GM-RAW \"category:primary\"")
        .await
        .unwrap()
        .into_iter()
        .collect();
    primary.sort_unstable();
    assert_eq!(primary, vec![1, 3]);

    let fetches: Vec<Fetch> = session
        .uid_fetch("1:*", "(UID X-GM-MSGID X-GM-THRID X-GM-LABELS FLAGS)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(fetches.len(), 3);
    let labels = fetches[0].gmail_labels().expect("labels");
    assert_eq!(
        labels.iter().map(|l| l.as_ref()).collect::<Vec<_>>(),
        vec!["\\Inbox"]
    );
    let ids: Vec<u64> = fetches
        .iter()
        .map(|f| *f.gmail_msg_id().expect("msgid"))
        .collect();
    assert!(ids[0] < ids[1] && ids[1] < ids[2]);

    let names: Vec<Name> = session
        .list(Some(""), Some("*"))
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let all = names
        .iter()
        .find(|n| n.name() == "[Gmail]/All Mail")
        .expect("All Mail");
    assert!(all.attributes().contains(&NameAttribute::All));
    let parent = names
        .iter()
        .find(|n| n.name() == "[Gmail]")
        .expect("[Gmail]");
    assert!(parent.attributes().contains(&NameAttribute::NoSelect));
    session.uid_mv("2", "[Gmail]/All Mail").await.unwrap();
    assert_eq!(server.messages(USER, "[Gmail]/All Mail").len(), 1);
    session.logout().await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn seeded_demo_messages_parse() {
    let server = start().await;
    let count = server.seed_demo_messages(USER);
    assert!(count >= 25, "{count}");
    let stored = server.messages(USER, "INBOX");

    let mut session = login(&server).await;
    let mailbox = session.select("INBOX").await.unwrap();
    assert_eq!(mailbox.exists as usize, stored.len());
    let fetches: Vec<Fetch> = session
        .uid_fetch(
            "1:*",
            "(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE BODY.PEEK[])",
        )
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(fetches.len(), stored.len());
    let mut multipart = 0;
    for (fetch, message) in fetches.iter().zip(&stored) {
        assert_eq!(fetch.uid, Some(message.uid));
        assert_eq!(fetch.body(), Some(message.raw.as_slice()));
        assert_eq!(fetch.internal_date(), Some(message.internal_date));
        assert!(fetch.envelope().and_then(|e| e.subject.as_ref()).is_some());
        if matches!(fetch.bodystructure(), Some(BodyStructure::Multipart { .. })) {
            multipart += 1;
        }
    }
    assert!(multipart >= 5, "{multipart}");

    // The PDF attachment can be fetched by part number.
    let ticket = stored
        .iter()
        .find(|m| String::from_utf8_lossy(&m.raw).contains("biglietto-milano-roma.pdf"))
        .expect("message with the PDF");
    let parts: Vec<Fetch> = session
        .uid_fetch(ticket.uid.to_string(), "(BODY.PEEK[2] BODY.PEEK[2.MIME])")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let pdf = parts[0]
        .section(&SectionPath::Part(vec![2], None))
        .expect("part 2");
    assert!(
        pdf.starts_with(b"JVBERi0xLjQ"),
        "{:?}",
        String::from_utf8_lossy(&pdf[..20])
    );

    let sent = session.status("Sent", "(MESSAGES)").await.unwrap();
    assert!(sent.exists >= 3);
    session.logout().await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn forwarded_message_structure_and_sections() {
    let server = start().await;
    let inner = "From: Anna <anna@example.com>\r\nSubject: Originale\r\n\
                 Content-Type: multipart/alternative; boundary=\"in\"\r\n\r\n\
                 --in\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nTesto\r\n\
                 --in\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Testo</p>\r\n--in--\r\n";
    let outer = format!(
        "From: Luca <luca@example.com>\r\nTo: {USER}\r\nSubject: Fwd: Originale\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"out\"\r\n\r\n\
         --out\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nVedi sotto.\r\n\
         --out\r\nContent-Type: message/rfc822\r\nContent-Disposition: attachment; filename=\"originale.eml\"\r\n\r\n\
         {inner}\r\n--out--\r\n"
    );
    let uid = server.deliver(USER, "INBOX", outer.as_bytes());

    let mut session = login(&server).await;
    session.select("INBOX").await.unwrap();
    let fetches: Vec<Fetch> = session
        .uid_fetch(
            uid.to_string(),
            "(BODYSTRUCTURE BODY.PEEK[1] BODY.PEEK[2] BODY.PEEK[2.HEADER] BODY.PEEK[2.1] BODY.PEEK[2.2.MIME] BODY.PEEK[TEXT]<0.6>)",
        )
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let fetch = &fetches[0];
    let Some(BodyStructure::Multipart { bodies, .. }) = fetch.bodystructure() else {
        panic!("unexpected structure {:?}", fetch.bodystructure());
    };
    assert_eq!(bodies.len(), 2);
    assert!(matches!(bodies[0], BodyStructure::Text { .. }));
    let BodyStructure::Message { envelope, body, .. } = &bodies[1] else {
        panic!("part 2 should be message/rfc822: {:?}", bodies[1]);
    };
    assert_eq!(envelope.subject.as_deref(), Some(&b"Originale"[..]));
    assert!(matches!(body.as_ref(), BodyStructure::Multipart { bodies, .. } if bodies.len() == 2));

    let section = |path: Vec<u32>, kind: Option<MessageSection>| {
        fetch
            .section(&SectionPath::Part(path, kind))
            .map(|s| String::from_utf8_lossy(s).into_owned())
    };
    assert_eq!(section(vec![1], None).as_deref(), Some("Vedi sotto."));
    assert_eq!(section(vec![2], None).as_deref(), Some(inner));
    assert!(
        section(vec![2], Some(MessageSection::Header))
            .unwrap()
            .starts_with("From: Anna")
    );
    assert_eq!(section(vec![2, 1], None).as_deref(), Some("Testo"));
    assert_eq!(
        section(vec![2, 2], Some(MessageSection::Mime)).as_deref(),
        Some("Content-Type: text/html; charset=utf-8\r\n\r\n")
    );
    assert_eq!(fetch.text(), Some(&b"--out\r\n"[..6]));
    session.logout().await.unwrap();
    server.shutdown().await;
}

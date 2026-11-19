//! Protocol-level tests with a raw TCP client.

mod common;

use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use common::*;
use nmmail_testserver::Config;
use nmmail_testserver::chrono::{FixedOffset, TimeZone};

#[tokio::test]
async fn login_select_list_status_logout() {
    let server = start().await;
    server.deliver(USER, "INBOX", &message("uno", "primo"));
    server.deliver_with_flags(USER, "INBOX", &message("due", "secondo"), &["\\Seen"], None);

    let mut c = RawClient::connect(server.imap_addr()).await;
    for cap in [
        "IMAP4rev1",
        "IDLE",
        "MOVE",
        "UIDPLUS",
        "SPECIAL-USE",
        "LITERAL+",
        "AUTH=XOAUTH2",
        "ID",
    ] {
        assert!(
            c.greeting.contains(cap),
            "{cap} missing from {}",
            c.greeting
        );
    }
    assert!(!c.greeting.contains("X-GM-EXT-1"));

    let r = c.cmd("SELECT INBOX").await;
    assert!(tagged(&r).contains("BAD"), "{}", joined(&r));
    let r = c.cmd(&format!("LOGIN {USER} wrong")).await;
    assert!(
        tagged(&r).contains("NO [AUTHENTICATIONFAILED]"),
        "{}",
        joined(&r)
    );
    let r = c
        .cmd(&format!("LOGIN \"{}\" \"{PASSWORD}\"", USER.to_uppercase()))
        .await;
    assert!(
        is_ok(&r) && tagged(&r).contains("[CAPABILITY "),
        "{}",
        joined(&r)
    );

    let r = c.cmd("SELECT inbox").await;
    let text = joined(&r);
    let validity = server.uid_validity(USER, "INBOX").unwrap();
    for expected in [
        "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)".to_string(),
        "* 2 EXISTS".to_string(),
        "* 2 RECENT".to_string(),
        "* OK [UNSEEN 1]".to_string(),
        format!("* OK [UIDVALIDITY {validity}]"),
        "* OK [UIDNEXT 3]".to_string(),
        "* OK [PERMANENTFLAGS (".to_string(),
        "OK [READ-WRITE] SELECT completed".to_string(),
    ] {
        assert!(text.contains(&expected), "{expected} missing from:\n{text}");
    }

    let text = joined(&c.cmd("LIST \"\" \"*\"").await);
    for expected in [
        "* LIST (\\HasNoChildren) \"/\" \"INBOX\"",
        "* LIST (\\HasNoChildren \\Sent) \"/\" \"Sent\"",
        "* LIST (\\HasNoChildren \\Drafts) \"/\" \"Drafts\"",
        "* LIST (\\HasNoChildren \\Trash) \"/\" \"Trash\"",
        "* LIST (\\HasNoChildren \\Junk) \"/\" \"Junk\"",
        "* LIST (\\HasNoChildren \\Archive) \"/\" \"Archive\"",
    ] {
        assert!(text.contains(expected), "{expected} missing from:\n{text}");
    }
    let text = joined(&c.cmd("LIST \"\" \"\"").await);
    assert!(text.contains("* LIST (\\Noselect) \"/\" \"\""), "{text}");
    let text = joined(&c.cmd("LSUB \"\" \"%\"").await);
    assert!(
        text.contains("* LSUB (\\HasNoChildren \\Junk) \"/\" \"Junk\""),
        "{text}"
    );

    let text = joined(
        &c.cmd("STATUS INBOX (MESSAGES RECENT UIDNEXT UIDVALIDITY UNSEEN)")
            .await,
    );
    assert!(
        text.contains(&format!(
            "* STATUS \"INBOX\" (MESSAGES 2 RECENT 0 UIDNEXT 3 UIDVALIDITY {validity} UNSEEN 1)"
        )),
        "{text}"
    );
    let text = joined(&c.cmd("ID (\"name\" \"test\")").await);
    assert!(
        text.contains("* ID (\"name\" \"nmmail-testserver\""),
        "{text}"
    );
    let text = joined(&c.cmd("NAMESPACE").await);
    assert!(
        text.contains("* NAMESPACE ((\"\" \"/\")) NIL NIL"),
        "{text}"
    );

    let r = c.cmd("LOGOUT").await;
    assert!(joined(&r).contains("* BYE") && is_ok(&r));
    assert!(c.closed_within(TIMEOUT).await);
    server.shutdown().await;
}

#[tokio::test]
async fn idle_receives_exists_within_200ms() {
    let server = start().await;
    let mut c = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&c.cmd("SELECT INBOX").await));

    c.send_line("i1 IDLE").await;
    assert_eq!(c.read_line().await, "+ idling");
    // Nothing is pending: the server must stay silent.
    assert_eq!(c.try_read_line(Duration::from_millis(100)).await, Ok(None));

    let started = Instant::now();
    let uid = server.deliver(USER, "INBOX", &message("nuova", "ciao"));
    let line = c.read_line().await;
    let elapsed = started.elapsed();
    eprintln!("deliver() -> * EXISTS latency: {elapsed:?}");
    assert_eq!(line, "* 1 EXISTS");
    assert!(
        elapsed < Duration::from_millis(200),
        "EXISTS took {elapsed:?}"
    );
    assert_eq!(c.read_line().await, "* 1 RECENT");
    assert_eq!(uid, 1);

    // Several deliveries in a row.
    server.deliver(USER, "INBOX", &message("altra", "x"));
    assert_eq!(c.read_line().await, "* 2 EXISTS");
    assert_eq!(c.read_line().await, "* 2 RECENT");

    c.send_line("DONE").await;
    assert_eq!(c.read_line().await, "i1 OK IDLE terminated");
    assert!(server.command_log().iter().any(|l| l == "DONE"));
    assert!(server.command_log().iter().any(|l| l == "i1 IDLE"));

    // Changes while not idling are reported before the next tagged response.
    server.deliver(USER, "INBOX", &message("terza", "x"));
    let text = joined(&c.cmd("NOOP").await);
    assert!(text.contains("* 3 EXISTS"), "{text}");
    server.shutdown().await;
}

#[tokio::test]
async fn fetch_body_peek_literal_roundtrip() {
    let server = start().await;
    let raw: Vec<u8> = [
        &b"From: =?UTF-8?B?Tmljb2zDsg==?= <nicolo@example.com>\r\nTo: test@nmmail.local\r\n"[..],
        b"Subject: Prova \"virgolette\"\r\nX-Folded: a\r\n b\r\nContent-Type: text/plain; charset=UTF-8\r\n",
        b"Content-Transfer-Encoding: 8bit\r\n\r\nPerch\xc3\xa9 s\xc3\xac.\r\n.riga con punto\r\n\r\n{5}\r\nfine",
    ]
    .concat();
    let date = FixedOffset::east_opt(-7 * 3600)
        .unwrap()
        .with_ymd_and_hms(1996, 7, 17, 2, 44, 25)
        .unwrap();
    let uid = server.deliver_with_flags(USER, "INBOX", &raw, &[], Some(date));
    let mut c = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&c.cmd("SELECT INBOX").await));

    let r = c
        .cmd(&format!(
            "UID FETCH {uid} (BODY.PEEK[] INTERNALDATE RFC822.SIZE)"
        ))
        .await;
    assert!(is_ok(&r));
    let fetch = &r[0];
    assert!(
        fetch.text.starts_with(&format!(
            "* 1 FETCH (UID {uid} BODY[] {{{}}} INTERNALDATE \"17-Jul-1996 02:44:25 -0700\" RFC822.SIZE {})",
            raw.len(),
            raw.len()
        )),
        "{}",
        fetch.text
    );
    assert_eq!(fetch.literals, vec![raw.clone()]);
    assert!(
        !server.messages(USER, "INBOX")[0]
            .flags
            .contains(&"\\Seen".to_string())
    );

    // Partial fetch and header subsets.
    let r = c.cmd("FETCH 1 (BODY.PEEK[]<5.10>)").await;
    assert!(r[0].text.contains("BODY[]<5> {10}"), "{}", r[0].text);
    assert_eq!(r[0].literals[0], raw[5..15].to_vec());
    let r = c
        .cmd("UID FETCH 1 (BODY.PEEK[HEADER.FIELDS (subject x-folded)])")
        .await;
    assert!(
        r[0].text
            .contains("BODY[HEADER.FIELDS (subject x-folded)] {"),
        "{}",
        r[0].text
    );
    assert_eq!(
        r[0].literals[0],
        b"Subject: Prova \"virgolette\"\r\nX-Folded: a\r\n b\r\n\r\n".to_vec()
    );
    let r = c.cmd("UID FETCH 1 (BODY.PEEK[HEADER.FIELDS.NOT (subject x-folded content-type content-transfer-encoding)])").await;
    assert_eq!(
        r[0].literals[0],
        b"From: =?UTF-8?B?Tmljb2zDsg==?= <nicolo@example.com>\r\nTo: test@nmmail.local\r\n\r\n"
            .to_vec()
    );
    let r = c.cmd("UID FETCH 1 (ENVELOPE)").await;
    assert!(
        r[0].text.contains(
            "ENVELOPE (NIL \"Prova \\\"virgolette\\\"\" ((\"=?UTF-8?B?Tmljb2zDsg==?=\" NIL \"nicolo\" \"example.com\"))"
        ),
        "{}",
        r[0].text
    );

    // A non-PEEK fetch sets \Seen and reports the new flags.
    let r = c.cmd("UID FETCH 1 (BODY[TEXT])").await;
    assert!(
        r[0].text.contains("FLAGS (\\Seen \\Recent)"),
        "{}",
        r[0].text
    );
    assert!(r[0].literals[0].starts_with("Perché".as_bytes()));
    assert!(
        server.messages(USER, "INBOX")[0]
            .flags
            .contains(&"\\Seen".to_string())
    );

    // Macros and RFC822.* items.
    let r = c.cmd("FETCH 1:* FAST").await;
    assert!(
        r[0].text.contains("FLAGS (\\Seen \\Recent) INTERNALDATE"),
        "{}",
        r[0].text
    );
    let r = c.cmd("FETCH * (RFC822.HEADER BODYSTRUCTURE)").await;
    assert!(r[0].text.contains("RFC822.HEADER {"), "{}", r[0].text);
    assert!(
        r[0].text
            .contains("BODYSTRUCTURE (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"8BIT\""),
        "{}",
        r[0].text
    );
    server.shutdown().await;
}

#[tokio::test]
async fn store_propagates_to_idling_session() {
    let server = start().await;
    server.deliver(USER, "INBOX", &message("uno", "x"));
    let mut idler = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&idler.cmd("SELECT INBOX").await)); // claims \Recent
    let mut other = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&other.cmd("SELECT INBOX").await));

    idler.send_line("i IDLE").await;
    assert_eq!(idler.read_line().await, "+ idling");

    let r = other.cmd("UID STORE 1 +FLAGS (\\Seen $Forwarded)").await;
    assert!(is_ok(&r));
    assert_eq!(r[0].text, "* 1 FETCH (UID 1 FLAGS (\\Seen $Forwarded))");
    assert_eq!(
        idler.read_line().await,
        "* 1 FETCH (UID 1 FLAGS (\\Seen $Forwarded \\Recent))"
    );

    // Library API, as another client would do.
    server.set_flags(USER, "INBOX", 1, &["\\Flagged"]);
    assert_eq!(
        idler.read_line().await,
        "* 1 FETCH (UID 1 FLAGS (\\Flagged \\Recent))"
    );

    // .SILENT: no FETCH for the session itself, the idler is still notified.
    let r = other.cmd("STORE 1 -FLAGS.SILENT (\\Flagged)").await;
    assert_eq!(r.len(), 2, "{}", joined(&r)); // the pending set_flags update + tagged OK
    assert_eq!(
        idler.read_line().await,
        "* 1 FETCH (UID 1 FLAGS (\\Recent))"
    );

    idler.send_line("DONE").await;
    assert_eq!(idler.read_line().await, "i OK IDLE terminated");
    assert!(server.messages(USER, "INBOX")[0].flags.is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn expunge_propagates_and_keeps_sequence_numbers_consistent() {
    let server = start().await;
    for i in 1..=4 {
        server.deliver(USER, "INBOX", &message(&format!("m{i}"), "x"));
    }
    let mut idler = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&idler.cmd("SELECT INBOX").await));
    let mut other = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&other.cmd("SELECT INBOX").await));
    idler.send_line("i IDLE").await;
    assert_eq!(idler.read_line().await, "+ idling");

    assert!(is_ok(
        &other.cmd("UID STORE 2 +FLAGS.SILENT (\\Deleted)").await
    ));
    assert_eq!(
        idler.read_line().await,
        "* 2 FETCH (UID 2 FLAGS (\\Deleted \\Recent))"
    );
    let r = other.cmd("EXPUNGE").await;
    assert_eq!(joined(&r).lines().next(), Some("* 2 EXPUNGE"));
    assert_eq!(idler.read_line().await, "* 2 EXPUNGE");

    // UID 4 is now sequence number 3.
    server.expunge_uid(USER, "INBOX", 4);
    assert_eq!(idler.read_line().await, "* 3 EXPUNGE");
    idler.send_line("DONE").await;
    assert_eq!(idler.read_line().await, "i OK IDLE terminated");

    let text = joined(&idler.cmd("UID SEARCH ALL").await);
    assert!(text.contains("* SEARCH 1 3"), "{text}");
    let text = joined(&idler.cmd("FETCH 1:* (UID)").await);
    assert!(
        text.contains("* 1 FETCH (UID 1)") && text.contains("* 2 FETCH (UID 3)"),
        "{text}"
    );

    // EXPUNGE responses are withheld during FETCH / STORE / SEARCH (sequence numbers).
    server.expunge_uid(USER, "INBOX", 1);
    let r = idler.cmd("FETCH 1:* (UID)").await;
    assert!(!joined(&r).contains("EXPUNGE"), "{}", joined(&r));
    let r = idler.cmd("NOOP").await;
    assert_eq!(r[0].text, "* 1 EXPUNGE");
    server.shutdown().await;
}

#[tokio::test]
async fn smtp_send_delivers_to_inbox_and_sent() {
    let server = start_with(two_users()).await;
    let mut idler = RawClient::login(server.imap_addr(), OTHER, "pw").await;
    assert!(is_ok(&idler.cmd("SELECT INBOX").await));
    idler.send_line("i IDLE").await;
    assert_eq!(idler.read_line().await, "+ idling");

    let mut smtp = SmtpClient::connect(server.smtp_addr()).await;
    smtp.inner_send_ehlo_and_check().await;
    assert!(
        smtp.send(&format!("MAIL FROM:<{USER}>"))
            .await
            .starts_with("530")
    );
    let plain = BASE64.encode(format!("\0{USER}\0wrong"));
    assert!(
        smtp.send(&format!("AUTH PLAIN {plain}"))
            .await
            .starts_with("535")
    );
    let plain = BASE64.encode(format!("\0{USER}\0{PASSWORD}"));
    assert!(
        smtp.send(&format!("AUTH PLAIN {plain}"))
            .await
            .starts_with("235")
    );
    assert!(
        smtp.send(&format!("MAIL FROM:<{USER}> SIZE=200 BODY=8BITMIME"))
            .await
            .starts_with("250")
    );
    assert!(
        smtp.send(&format!("RCPT TO:<{OTHER}>"))
            .await
            .starts_with("250")
    );
    assert!(
        smtp.send("RCPT TO:<stranger@example.com>")
            .await
            .starts_with("250")
    );
    assert!(smtp.send("DATA").await.starts_with("354"));
    smtp.send_raw(
        format!(
            "From: Test <{USER}>\r\nTo: {OTHER}\r\nSubject: Saluti da SMTP\r\n\r\nCiao!\r\n..riga che inizia con un punto\r\n.\r\n"
        )
        .as_bytes(),
    )
    .await;
    assert!(smtp.reply().await[0].starts_with("250"));
    assert!(smtp.send("QUIT").await.starts_with("221"));

    assert_eq!(idler.read_line().await, "* 1 EXISTS");
    let inbox = server.messages(OTHER, "INBOX");
    assert_eq!(inbox.len(), 1);
    let expected = format!(
        "From: Test <{USER}>\r\nTo: {OTHER}\r\nSubject: Saluti da SMTP\r\n\r\nCiao!\r\n.riga che inizia con un punto\r\n"
    );
    assert_eq!(String::from_utf8_lossy(&inbox[0].raw), expected);
    assert!(inbox[0].flags.is_empty());
    let sent = server.messages(USER, "Sent");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].raw, expected.as_bytes());
    assert_eq!(sent[0].flags, vec!["\\Seen".to_string()]);
    assert!(server.messages(USER, "INBOX").is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn smtp_auth_login_and_xoauth2() {
    let server = start().await;
    let mut smtp = SmtpClient::connect(server.smtp_addr()).await;
    smtp.inner_send_ehlo_and_check().await;
    assert_eq!(smtp.send("AUTH LOGIN").await, "334 VXNlcm5hbWU6");
    assert_eq!(smtp.send(&BASE64.encode(USER)).await, "334 UGFzc3dvcmQ6");
    assert!(smtp.send(&BASE64.encode(PASSWORD)).await.starts_with("235"));
    assert!(smtp.send("RSET").await.starts_with("250"));

    let mut smtp = SmtpClient::connect(server.smtp_addr()).await;
    let bad = BASE64.encode(format!("user={USER}\x01auth=Bearer nope\x01\x01"));
    let reply = smtp.send(&format!("AUTH XOAUTH2 {bad}")).await;
    assert!(reply.starts_with("334 "), "{reply}");
    let challenge = BASE64.decode(reply[4..].trim()).unwrap();
    assert!(String::from_utf8_lossy(&challenge).contains("\"status\":\"400\""));
    assert!(smtp.send("").await.starts_with("535"));
    let good = BASE64.encode(format!("user={USER}\x01auth=Bearer test-token\x01\x01"));
    assert!(
        smtp.send(&format!("AUTH XOAUTH2 {good}"))
            .await
            .starts_with("235")
    );
    server.shutdown().await;
}

#[tokio::test]
async fn drop_all_connections_closes_sockets() {
    let server = start().await;
    let mut imap = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&imap.cmd("SELECT INBOX").await));
    imap.send_line("i IDLE").await;
    assert_eq!(imap.read_line().await, "+ idling");
    let mut smtp = SmtpClient::connect(server.smtp_addr()).await;
    assert!(wait_until(|| server.imap_connection_count() == 1).await);

    server.drop_all_connections();
    assert!(
        imap.closed_within(Duration::from_secs(2)).await,
        "IMAP socket still open"
    );
    assert!(
        smtp.closed_within(Duration::from_secs(2)).await,
        "SMTP socket still open"
    );
    assert!(wait_until(|| server.imap_connection_count() == 0).await);

    // New connections keep working.
    let mut again = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert_eq!(server.imap_connection_count(), 1);
    assert!(is_ok(&again.cmd("NOOP").await));
    server.shutdown().await;
}

#[tokio::test]
async fn freeze_makes_existing_connections_silent() {
    let server = start().await;
    let mut idler = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&idler.cmd("SELECT INBOX").await));
    idler.send_line("i IDLE").await;
    assert_eq!(idler.read_line().await, "+ idling");
    let mut busy = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&busy.cmd("SELECT INBOX").await));

    server.freeze_connections(true);
    server.deliver(USER, "INBOX", &message("congelata", "x"));
    assert_eq!(
        idler.try_read_line(Duration::from_millis(300)).await,
        Ok(None)
    );
    busy.send_line("b1 NOOP").await;
    assert_eq!(
        busy.try_read_line(Duration::from_millis(300)).await,
        Ok(None)
    );

    // Frozen connections stay open; new connections work normally.
    assert_eq!(server.imap_connection_count(), 2);
    let mut fresh = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    let text = joined(&fresh.cmd("SELECT INBOX").await);
    assert!(text.contains("* 1 EXISTS"), "{text}");

    server.freeze_connections(false);
    assert_eq!(idler.read_line().await, "* 1 EXISTS");
    let text = joined(&busy.read_until_tagged("b1").await);
    assert!(
        text.contains("* 1 EXISTS") && text.contains("b1 OK"),
        "{text}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn authenticate_plain_and_xoauth2() {
    let server = start().await;
    // XOAUTH2 with SASL-IR.
    let mut c = RawClient::connect(server.imap_addr()).await;
    let ir = BASE64.encode(format!("user={USER}\x01auth=Bearer test-token\x01\x01"));
    let r = c.cmd(&format!("AUTHENTICATE XOAUTH2 {ir}")).await;
    assert!(is_ok(&r), "{}", joined(&r));
    assert!(is_ok(&c.cmd("SELECT INBOX").await));

    // XOAUTH2 with continuation and a bad token: JSON error challenge, then NO.
    let mut c = RawClient::connect(server.imap_addr()).await;
    c.send_line("a1 AUTHENTICATE XOAUTH2").await;
    assert_eq!(c.read_line().await, "+ ");
    c.send_line(&BASE64.encode(format!("user={USER}\x01auth=Bearer expired\x01\x01")))
        .await;
    let challenge = c.read_line().await;
    let json = BASE64.decode(challenge.trim_start_matches("+ ")).unwrap();
    assert_eq!(
        String::from_utf8(json).unwrap(),
        r#"{"status":"400","schemes":"Bearer","scope":"https://mail.google.com/"}"#
    );
    c.send_line("").await;
    assert_eq!(
        c.read_line().await,
        "a1 NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)"
    );

    // PLAIN with continuation, then with SASL-IR; cancellation.
    c.send_line("a2 AUTHENTICATE PLAIN").await;
    assert_eq!(c.read_line().await, "+ ");
    c.send_line("*").await;
    assert!(c.read_line().await.starts_with("a2 BAD"));
    c.send_line("a3 AUTHENTICATE PLAIN").await;
    assert_eq!(c.read_line().await, "+ ");
    c.send_line(&BASE64.encode(format!("\0{USER}\0{PASSWORD}")))
        .await;
    assert!(
        c.read_line()
            .await
            .starts_with("a3 OK [CAPABILITY IMAP4rev1")
    );

    // Secrets never reach the command log.
    let log = server.command_log();
    assert!(
        log.contains(&"t1 AUTHENTICATE XOAUTH2 ***".to_string()),
        "{log:?}"
    );
    assert!(
        log.contains(&"a1 AUTHENTICATE XOAUTH2".to_string()),
        "{log:?}"
    );
    assert!(
        !log.iter().any(|l| l.contains(&ir) || l.contains("expired")),
        "{log:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn literals_and_append() {
    let server = start().await;
    let mut c = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    let body = message("appesa", "corpo");

    // Synchronizing literal.
    c.send_line(&format!(
        "a1 APPEND Drafts (\\Draft \\Seen) \"05-Jan-2024 09:30:00 +0100\" {{{}}}",
        body.len()
    ))
    .await;
    assert_eq!(c.read_line().await, "+ Ready for literal");
    c.send_raw(&body).await;
    c.send_raw(b"\r\n").await;
    let validity = server.uid_validity(USER, "Drafts").unwrap();
    assert_eq!(
        c.read_line().await,
        format!("a1 OK [APPENDUID {validity} 1] APPEND completed")
    );
    let drafts = server.messages(USER, "Drafts");
    assert_eq!(drafts[0].raw, body);
    assert_eq!(
        drafts[0].flags,
        vec!["\\Seen".to_string(), "\\Draft".to_string()]
    );
    assert_eq!(
        drafts[0].internal_date.to_rfc3339(),
        "2024-01-05T09:30:00+01:00"
    );

    // Non-synchronizing literal (LITERAL+), pipelined with another command.
    let mut data = format!("a2 APPEND INBOX {{{}+}}\r\n", body.len()).into_bytes();
    data.extend_from_slice(&body);
    data.extend_from_slice(b"\r\na3 LOGIN {4+}\r\nuser {2}\r\n");
    c.send_raw(&data).await;
    assert!(c.read_line().await.starts_with("a2 OK [APPENDUID "));
    assert_eq!(c.read_line().await, "+ Ready for literal");
    c.send_raw(b"pw\r\n").await;
    assert_eq!(c.read_line().await, "a3 BAD Already authenticated");

    let r = c.cmd("APPEND Missing {1+}\r\nx").await;
    assert!(tagged(&r).contains("NO [TRYCREATE]"), "{}", joined(&r));
    let r = c.cmd("APPEND INBOX \"not a date\" {1+}\r\nx").await;
    assert!(tagged(&r).contains("BAD"), "{}", joined(&r));
    assert!(
        server
            .command_log()
            .iter()
            .any(|l| l.starts_with("a1 APPEND Drafts"))
    );
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_input_never_crashes_the_server() {
    let server = start().await;
    let mut c = RawClient::connect(server.imap_addr()).await;
    for (line, expected) in [
        ("", None),
        ("*", Some("* BAD")),
        ("a1", Some("a1 BAD")),
        ("a2 FROBNICATE", Some("a2 BAD")),
        ("a3 LOGIN \"unterminated", Some("a3 BAD")),
        ("a4 SELECT (((", Some("a4 BAD")),
        ("+++ NOOP", Some("* BAD")),
        ("a5 LOGIN a", Some("a5 BAD")),
    ] {
        c.send_line(line).await;
        if let Some(expected) = expected {
            let reply = c.read_line().await;
            assert!(reply.starts_with(expected), "{line:?} -> {reply}");
        }
    }
    c.send_raw(b"a6 \xff\xfe\x00 garbage\r\n").await;
    assert!(c.read_line().await.starts_with("a6 BAD"));
    assert!(
        !server
            .command_log()
            .iter()
            .any(|l| l.contains("unterminated"))
    );

    let mut c = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    for command in [
        "FETCH 1 (FLAGS)",
        "SELECT",
        "SELECT Nope",
        "STATUS INBOX",
        "STATUS INBOX (BOGUS)",
        "UID",
        "UID FROB 1",
        "LIST",
    ] {
        let r = c.cmd(command).await;
        assert!(!is_ok(&r), "{command}: {}", joined(&r));
    }
    assert!(is_ok(&c.cmd("SELECT INBOX").await));
    server.deliver(USER, "INBOX", &message("x", "y"));
    for command in [
        "FETCH 0:x (FLAGS)",
        "FETCH 1 (BODY[0])",
        "FETCH 1 (BODY[HEADER.FIELDS])",
        "FETCH 1 (BODY[]<a.b>)",
        "FETCH 1 BOGUS",
        "STORE 1 FLAGS",
        "STORE 1 +FLAGZ (\\Seen)",
        "SEARCH",
        "SEARCH NOT",
        "SEARCH SINCE 31-Foo-2020",
        "SEARCH X-GM-RAW foo",
        "COPY 1",
        "UID EXPUNGE",
    ] {
        let r = c.cmd(command).await;
        assert!(tagged(&r).contains("BAD"), "{command}: {}", joined(&r));
    }
    // Out of range sets and unknown sections are not errors.
    let r = c.cmd("FETCH 1 (BODY[5.HEADER] UID)").await;
    assert!(
        r[0].text.contains("BODY[5.HEADER] NIL UID 1"),
        "{}",
        joined(&r)
    );
    assert!(is_ok(&c.cmd("UID FETCH 1000:2000 (FLAGS)").await));

    // Oversized lines close the connection (with BYE when it can still be delivered);
    // the server keeps running.
    let mut c = RawClient::connect(server.imap_addr()).await;
    c.try_send_raw(&vec![b'a'; 1024 * 1024 + 100]).await;
    match c.try_read_line(TIMEOUT).await {
        Ok(Some(line)) => assert!(line.starts_with("* BYE"), "{line}"),
        Ok(None) => panic!("the server did not react to an oversized line"),
        Err(()) => {} // reset before the BYE could be read
    }
    let mut c = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&c.cmd("NOOP").await));
    server.shutdown().await;
}

#[tokio::test]
async fn search_keys() {
    let server = start().await;
    let date = |d: u32| {
        FixedOffset::east_opt(3600)
            .unwrap()
            .with_ymd_and_hms(2025, 3, d, 12, 0, 0)
            .unwrap()
    };
    let raw1 = b"From: Anna Verdi <anna@example.com>\r\nTo: test@nmmail.local\r\nSubject: =?UTF-8?Q?Caff=C3=A8_domani?=\r\nDate: Mon, 3 Mar 2025 10:00:00 +0100\r\n\r\nPortiamo i cornetti.\r\n";
    let raw2 = b"From: Banca <info@banca.example>\r\nTo: test@nmmail.local\r\nCc: archivio@example.com\r\nSubject: Fattura marzo\r\nX-Test-Category: promotions\r\nDate: Wed, 5 Mar 2025 10:00:00 +0100\r\n\r\nImporto 12,50 euro.\r\n";
    let raw3 = b"From: Luca <luca@example.org>\r\nTo: test@nmmail.local\r\nSubject: Re: Fattura marzo\r\nDate: Fri, 7 Mar 2025 10:00:00 +0100\r\n\r\nGrazie!\r\n";
    server.deliver_with_flags(USER, "INBOX", raw1, &["\\Seen"], Some(date(3)));
    server.deliver_with_flags(USER, "INBOX", raw2, &["\\Flagged"], Some(date(5)));
    server.deliver_with_flags(
        USER,
        "INBOX",
        raw3,
        &["\\Seen", "\\Answered"],
        Some(date(7)),
    );
    let mut c = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&c.cmd("SELECT INBOX").await));
    for (query, expected) in [
        ("ALL", "1 2 3"),
        ("UNSEEN", "2"),
        ("SEEN NOT ANSWERED", "1"),
        ("FLAGGED", "2"),
        ("SUBJECT fattura", "2 3"),
        ("SUBJECT \"caffè\"", "1"),
        ("FROM ANNA", "1"),
        ("TO test@nmmail.local", "1 2 3"),
        ("CC archivio", "2"),
        ("BODY cornetti", "1"),
        ("TEXT banca", "2"),
        ("HEADER X-Test-Category promo", "2"),
        ("SINCE 5-Mar-2025", "2 3"),
        ("BEFORE 5-Mar-2025", "1"),
        ("ON 05-Mar-2025", "2"),
        ("SENTSINCE 6-Mar-2025", "3"),
        ("OR FLAGGED ANSWERED", "2 3"),
        ("NOT (SEEN FLAGGED)", "1 2 3"),
        ("2:*", "2 3"),
        ("UID 1,3", "1 3"),
        ("LARGER 10 SMALLER 100000", "1 2 3"),
        ("CHARSET UTF-8 SUBJECT marzo UNANSWERED", "2"),
        ("KEYWORD $Junk", ""),
    ] {
        let text = joined(&c.cmd(&format!("UID SEARCH {query}")).await);
        let line = text
            .lines()
            .find(|l| l.starts_with("* SEARCH"))
            .unwrap_or_default();
        assert_eq!(
            line.trim_start_matches("* SEARCH").trim(),
            expected,
            "{query}"
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn copy_move_and_uid_expunge() {
    let server = start().await;
    for i in 1..=3 {
        server.deliver(USER, "INBOX", &message(&format!("m{i}"), "x"));
    }
    let mut c = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&c.cmd("SELECT INBOX").await));
    let archive = server.uid_validity(USER, "Archive").unwrap();

    let r = c.cmd("UID COPY 1:2 Archive").await;
    assert!(
        tagged(&r).ends_with(&format!(" OK [COPYUID {archive} 1:2 1:2] COPY completed")),
        "{}",
        joined(&r)
    );
    let r = c.cmd("UID MOVE 3 \"Archive\"").await;
    let text = joined(&r);
    assert!(
        text.contains(&format!("* OK [COPYUID {archive} 3 3] Moved")),
        "{text}"
    );
    assert!(text.contains("* 3 EXPUNGE"), "{text}");
    assert_eq!(server.messages(USER, "Archive").len(), 3);
    assert_eq!(server.messages(USER, "INBOX").len(), 2);
    let r = c.cmd("MOVE 1 Missing").await;
    assert!(tagged(&r).contains("NO [TRYCREATE]"), "{}", joined(&r));

    assert!(is_ok(
        &c.cmd("UID STORE 1:2 +FLAGS.SILENT (\\Deleted)").await
    ));
    let r = c.cmd("UID EXPUNGE 2").await;
    assert_eq!(r[0].text, "* 2 EXPUNGE");
    assert_eq!(server.messages(USER, "INBOX").len(), 1);
    assert!(is_ok(&c.cmd("CLOSE").await));
    assert!(server.messages(USER, "INBOX").is_empty());
    assert!(tagged(&c.cmd("FETCH 1 (FLAGS)").await).contains("BAD"));

    // Mailbox management.
    assert!(is_ok(&c.cmd("CREATE Lavoro/Progetti").await));
    let text = joined(&c.cmd("LIST \"\" \"Lavoro*\"").await);
    assert!(
        text.contains("* LIST (\\HasChildren) \"/\" \"Lavoro\""),
        "{text}"
    );
    assert!(
        text.contains("* LIST (\\HasNoChildren) \"/\" \"Lavoro/Progetti\""),
        "{text}"
    );
    assert!(is_ok(&c.cmd("RENAME Lavoro/Progetti Lavoro/Clienti").await));
    assert!(is_ok(&c.cmd("DELETE Lavoro/Clienti").await));
    assert!(tagged(&c.cmd("DELETE INBOX").await).contains("NO"));
    assert!(tagged(&c.cmd("CREATE Archive").await).contains("NO [ALREADYEXISTS]"));

    // EXAMINE is read-only.
    let r = c.cmd("EXAMINE Archive").await;
    assert!(tagged(&r).contains("[READ-ONLY]"));
    assert!(tagged(&c.cmd("STORE 1 +FLAGS (\\Seen)").await).contains("NO"));
    let r = c.cmd("FETCH 1 (BODY[])").await;
    assert!(!r[0].text.contains("FLAGS"), "{}", r[0].text);
    assert!(is_ok(&c.cmd("UNSELECT").await));
    server.shutdown().await;
}

#[tokio::test]
async fn uidvalidity_reset_disconnects_selected_sessions() {
    let server = start().await;
    for i in 1..=3 {
        server.deliver(USER, "INBOX", &message(&format!("m{i}"), "x"));
    }
    server.expunge_uid(USER, "INBOX", 1);
    let old = server.uid_validity(USER, "INBOX").unwrap();
    let mut idler = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(is_ok(&idler.cmd("SELECT INBOX").await));
    idler.send_line("i IDLE").await;
    assert_eq!(idler.read_line().await, "+ idling");

    server.reset_uidvalidity(USER, "INBOX");
    assert!(idler.read_line().await.starts_with("* BYE"));
    assert!(idler.closed_within(TIMEOUT).await);

    let new = server.uid_validity(USER, "INBOX").unwrap();
    assert!(new > old);
    let uids: Vec<u32> = server
        .messages(USER, "INBOX")
        .iter()
        .map(|m| m.uid)
        .collect();
    assert_eq!(uids, vec![1, 2]);
    let mut c = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    let text = joined(&c.cmd("SELECT INBOX").await);
    assert!(
        text.contains(&format!("[UIDVALIDITY {new}]")) && text.contains("[UIDNEXT 3]"),
        "{text}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn gmail_mode() {
    let server = start_with(Config {
        gmail_mode: true,
        ..Config::default()
    })
    .await;
    let primary = message("primaria", "x");
    let promo = [
        &b"X-Test-Category: promotions\r\n"[..],
        &message("offerta", "sconto"),
    ]
    .concat();
    server.deliver(USER, "INBOX", &primary);
    server.deliver(USER, "INBOX", &promo);
    let mut c = RawClient::login(server.imap_addr(), USER, PASSWORD).await;
    assert!(c.greeting.contains("X-GM-EXT-1"));

    let text = joined(&c.cmd("LIST \"\" \"*\"").await);
    for expected in [
        "* LIST (\\HasChildren \\Noselect) \"/\" \"[Gmail]\"",
        "* LIST (\\HasNoChildren \\All) \"/\" \"[Gmail]/All Mail\"",
        "* LIST (\\HasNoChildren \\Sent) \"/\" \"[Gmail]/Sent Mail\"",
        "* LIST (\\HasNoChildren \\Junk) \"/\" \"[Gmail]/Spam\"",
        "* LIST (\\HasNoChildren \\Trash) \"/\" \"[Gmail]/Trash\"",
        "* LIST (\\HasNoChildren \\Drafts) \"/\" \"[Gmail]/Drafts\"",
    ] {
        assert!(text.contains(expected), "{expected} missing from:\n{text}");
    }
    assert!(!text.contains("\"Archive\""));
    assert!(tagged(&c.cmd("SELECT [Gmail]").await).contains("NO"));

    assert!(is_ok(&c.cmd("SELECT INBOX").await));
    let text = joined(&c.cmd("UID SEARCH X-GM-RAW \"category:primary\"").await);
    assert!(text.lines().any(|l| l == "* SEARCH 1"), "{text}");
    let text = joined(&c.cmd("UID SEARCH X-GM-RAW \"category:promotions\"").await);
    assert!(text.lines().any(|l| l == "* SEARCH 2"), "{text}");
    let text = joined(
        &c.cmd("UID SEARCH X-GM-RAW \"sconto -category:primary\"")
            .await,
    );
    assert!(text.lines().any(|l| l == "* SEARCH 2"), "{text}");

    assert!(is_ok(&c.cmd("UID COPY 1 \"[Gmail]/All Mail\"").await));
    let r = c
        .cmd("UID FETCH 1:2 (X-GM-MSGID X-GM-THRID X-GM-LABELS FLAGS)")
        .await;
    let first = &r[0].text;
    assert!(first.contains("X-GM-LABELS (\\Inbox)"), "{first}");
    assert!(
        first.contains("X-GM-MSGID ") && first.contains("X-GM-THRID "),
        "{first}"
    );
    assert!(is_ok(&r));
    server.shutdown().await;
}

impl SmtpClient {
    async fn inner_send_ehlo_and_check(&mut self) {
        self.send_raw(b"EHLO client.example\r\n").await;
        let lines = self.reply().await;
        let text = lines.join("\n");
        for expected in [
            "250-SIZE",
            "8BITMIME",
            "PIPELINING",
            "AUTH PLAIN LOGIN XOAUTH2",
        ] {
            assert!(text.contains(expected), "{expected} missing from {text}");
        }
    }
}

//! Helpers shared by the integration tests: a raw IMAP client and a raw SMTP client.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use nmmail_testserver::{Config, TestServer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const USER: &str = "test@nmmail.local";
pub const PASSWORD: &str = "test";
pub const OTHER: &str = "other@nmmail.local";
pub const TIMEOUT: Duration = Duration::from_secs(5);

pub async fn start() -> TestServer {
    TestServer::start(Config::default())
        .await
        .expect("start test server")
}

pub async fn start_with(config: Config) -> TestServer {
    TestServer::start(config).await.expect("start test server")
}

/// Two users: [`USER`] / [`PASSWORD`] and [`OTHER`] / `pw`.
pub fn two_users() -> Config {
    Config {
        users: vec![
            (USER.to_string(), PASSWORD.to_string()),
            (OTHER.to_string(), "pw".to_string()),
        ],
        ..Config::default()
    }
}

pub fn message(subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: Mario Rossi <mario@example.com>\r\nTo: {USER}\r\nSubject: {subject}\r\n\
         Date: Wed, 19 Nov 2025 10:15:00 +0100\r\nMessage-ID: <{subject}@example.com>\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// Polls `condition` until it holds or the timeout expires.
pub async fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    condition()
}

/// One server response: its text (literal contents replaced by their `{n}` header)
/// and the literals in order.
#[derive(Debug, Clone)]
pub struct Resp {
    pub text: String,
    pub literals: Vec<Vec<u8>>,
}

pub fn joined(resps: &[Resp]) -> String {
    resps
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn literal_len(line: &str) -> Option<usize> {
    let inner = line.strip_suffix('}')?;
    let open = inner.rfind('{')?;
    inner[open + 1..].parse().ok()
}

/// A minimal IMAP client working on raw lines.
pub struct RawClient {
    stream: TcpStream,
    buf: Vec<u8>,
    next_tag: u32,
    pub greeting: String,
}

impl RawClient {
    pub async fn connect(addr: SocketAddr) -> RawClient {
        let stream = TcpStream::connect(addr).await.expect("connect");
        let mut client = RawClient {
            stream,
            buf: Vec::new(),
            next_tag: 0,
            greeting: String::new(),
        };
        client.greeting = client.read_line().await;
        assert!(
            client.greeting.starts_with("* OK"),
            "greeting: {}",
            client.greeting
        );
        client
    }

    pub async fn login(addr: SocketAddr, user: &str, password: &str) -> RawClient {
        let mut client = RawClient::connect(addr).await;
        let resps = client.cmd(&format!("LOGIN {user} {password}")).await;
        assert!(is_ok(&resps), "login failed: {}", joined(&resps));
        client
    }

    pub fn tag(&mut self) -> String {
        self.next_tag += 1;
        format!("t{}", self.next_tag)
    }

    pub async fn send_raw(&mut self, data: &[u8]) {
        self.stream.write_all(data).await.expect("write");
    }

    /// Like [`RawClient::send_raw`] but tolerates a connection closed by the server.
    pub async fn try_send_raw(&mut self, data: &[u8]) -> bool {
        self.stream.write_all(data).await.is_ok()
    }

    pub async fn send_line(&mut self, line: &str) {
        self.send_raw(format!("{line}\r\n").as_bytes()).await;
    }

    /// Reads more data; `Ok(false)` on EOF, `Err(())` on timeout.
    async fn fill(&mut self, timeout: Duration) -> Result<bool, ()> {
        let mut chunk = [0u8; 8192];
        match tokio::time::timeout(timeout, self.stream.read(&mut chunk)).await {
            Err(_) => Err(()),
            Ok(Ok(0)) | Ok(Err(_)) => Ok(false),
            Ok(Ok(n)) => {
                self.buf.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
        }
    }

    /// One line without CRLF; `Ok(None)` on timeout, `Err(())` if the connection closed.
    pub async fn try_read_line(&mut self, timeout: Duration) -> Result<Option<String>, ()> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pos) = self.buf.windows(2).position(|w| w == b"\r\n") {
                let line: Vec<u8> = self.buf.drain(..pos + 2).collect();
                return Ok(Some(String::from_utf8_lossy(&line[..pos]).into_owned()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.fill(remaining).await {
                Err(()) => return Ok(None),
                Ok(false) => return Err(()),
                Ok(true) => {}
            }
        }
    }

    pub async fn read_line(&mut self) -> String {
        match self.try_read_line(TIMEOUT).await {
            Ok(Some(line)) => line,
            Ok(None) => panic!("timed out waiting for a line"),
            Err(()) => panic!("connection closed while waiting for a line"),
        }
    }

    async fn read_bytes(&mut self, n: usize) -> Vec<u8> {
        while self.buf.len() < n {
            match self.fill(TIMEOUT).await {
                Ok(true) => {}
                _ => panic!("connection closed or timed out while reading a literal"),
            }
        }
        self.buf.drain(..n).collect()
    }

    /// One complete response (line plus literals).
    pub async fn read_response(&mut self) -> Resp {
        let mut text = String::new();
        let mut literals = Vec::new();
        loop {
            let line = self.read_line().await;
            text.push_str(&line);
            match literal_len(&line) {
                Some(n) => literals.push(self.read_bytes(n).await),
                None => return Resp { text, literals },
            }
        }
    }

    pub async fn read_until_tagged(&mut self, tag: &str) -> Vec<Resp> {
        let prefix = format!("{tag} ");
        let mut resps = Vec::new();
        loop {
            let resp = self.read_response().await;
            let done = resp.text.starts_with(&prefix);
            resps.push(resp);
            if done {
                return resps;
            }
        }
    }

    /// Sends `<tag> <command>` and returns every response up to the tagged one.
    pub async fn cmd(&mut self, command: &str) -> Vec<Resp> {
        let tag = self.tag();
        self.send_line(&format!("{tag} {command}")).await;
        self.read_until_tagged(&tag).await
    }

    /// Waits until the server closes the connection (EOF or reset).
    pub async fn closed_within(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.fill(remaining).await {
                Ok(false) => return true,
                Err(()) => return false,
                Ok(true) => self.buf.clear(),
            }
        }
    }
}

/// Whether the tagged (last) response is OK.
pub fn is_ok(resps: &[Resp]) -> bool {
    resps.last().is_some_and(|r| {
        r.text
            .split_once(' ')
            .is_some_and(|(_, rest)| rest.starts_with("OK"))
    })
}

pub fn tagged(resps: &[Resp]) -> &str {
    resps.last().map(|r| r.text.as_str()).unwrap_or_default()
}

/// A minimal SMTP client.
pub struct SmtpClient {
    inner: RawClient,
}

impl SmtpClient {
    pub async fn connect(addr: SocketAddr) -> SmtpClient {
        let stream = TcpStream::connect(addr).await.expect("connect");
        let mut inner = RawClient {
            stream,
            buf: Vec::new(),
            next_tag: 0,
            greeting: String::new(),
        };
        inner.greeting = inner.read_line().await;
        assert!(
            inner.greeting.starts_with("220 "),
            "greeting: {}",
            inner.greeting
        );
        SmtpClient { inner }
    }

    /// Reads a (possibly multi-line) reply.
    pub async fn reply(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let line = self.inner.read_line().await;
            let last = line.as_bytes().get(3) != Some(&b'-');
            lines.push(line);
            if last {
                return lines;
            }
        }
    }

    /// Sends a line and returns the reply (last line, code included).
    pub async fn send(&mut self, line: &str) -> String {
        self.inner.send_line(line).await;
        self.reply().await.pop().unwrap_or_default()
    }

    pub async fn send_raw(&mut self, data: &[u8]) {
        self.inner.send_raw(data).await;
    }

    pub async fn closed_within(&mut self, timeout: Duration) -> bool {
        self.inner.closed_within(timeout).await
    }
}

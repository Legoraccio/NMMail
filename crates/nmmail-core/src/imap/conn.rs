//! IMAP connection setup: TCP (+keepalive), TLS / STARTTLS, authentication.

use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::error::{ErrorCode, coded};
use crate::model::{Security, ServerConfig};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Plain or TLS-wrapped TCP stream.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stream::Plain(_) => f.write_str("Stream::Plain"),
            Stream::Tls(_) => f.write_str("Stream::Tls"),
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

pub type Session = async_imap::Session<Stream>;

/// How to authenticate.
#[derive(Clone)]
pub enum Credentials {
    Password { user: String, password: String },
    OAuth2 { user: String, access_token: String },
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credentials::Password { user, .. } => write!(f, "Password({user})"),
            Credentials::OAuth2 { user, .. } => write!(f, "OAuth2({user})"),
        }
    }
}

/// Server capabilities we care about.
#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    pub idle: bool,
    pub mv: bool,
    pub uidplus: bool,
    pub gmail: bool,
    pub condstore: bool,
    pub special_use: bool,
}

/// Shared rustls configuration using the OS certificate verifier.
pub fn tls_config() -> Result<Arc<rustls::ClientConfig>> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(c) = CONFIG.get() {
        return Ok(c.clone());
    }
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
    Ok(CONFIG.get_or_init(|| Arc::new(cfg)).clone())
}

async fn tcp_connect(host: &str, port: u16) -> Result<TcpStream> {
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| coded(ErrorCode::Offline, format!("timeout connecting to {host}:{port}")))?
        .map_err(|e| coded(ErrorCode::Offline, format!("{host}:{port}: {e}")))?;
    tcp.set_nodelay(true).ok();
    // Keepalive keeps NAT mappings warm and detects dead peers while idling.
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(10));
    socket2::SockRef::from(&tcp).set_tcp_keepalive(&ka).ok();
    Ok(tcp)
}

async fn tls_wrap(tcp: TcpStream, host: &str) -> Result<Stream> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| anyhow!("invalid server name {host}"))?;
    let connector = TlsConnector::from(tls_config()?);
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(name, tcp))
        .await
        .map_err(|_| coded(ErrorCode::Offline, format!("TLS timeout with {host}")))?
        .map_err(|e| coded(ErrorCode::Offline, format!("TLS with {host}: {e}")))?;
    Ok(Stream::Tls(Box::new(tls)))
}

async fn read_greeting(client: &mut async_imap::Client<Stream>) -> Result<()> {
    tokio::time::timeout(CONNECT_TIMEOUT, client.read_response())
        .await
        .map_err(|_| coded(ErrorCode::Offline, "no IMAP greeting"))??
        .ok_or_else(|| coded(ErrorCode::Offline, "connection closed before greeting"))?;
    Ok(())
}

struct XOAuth2 {
    payload: String,
    sent: bool,
}

impl async_imap::Authenticator for XOAuth2 {
    type Response = Vec<u8>;
    fn process(&mut self, _challenge: &[u8]) -> Vec<u8> {
        if self.sent {
            // Server sent an error challenge; an empty reply makes it finish with NO.
            Vec::new()
        } else {
            self.sent = true;
            self.payload.as_bytes().to_vec()
        }
    }
}

/// XOAUTH2 SASL initial response (shared with SMTP).
pub fn xoauth2_payload(user: &str, token: &str) -> String {
    format!("user={user}\x01auth=Bearer {token}\x01\x01")
}

fn auth_error(e: async_imap::error::Error) -> anyhow::Error {
    match e {
        async_imap::error::Error::No(msg) | async_imap::error::Error::Bad(msg) => {
            coded(ErrorCode::AuthRequired, format!("authentication failed: {msg}"))
        }
        other => anyhow::Error::new(other).context("authentication"),
    }
}

/// Opens an authenticated IMAP session.
pub async fn connect(server: &ServerConfig, creds: &Credentials) -> Result<(Session, Caps)> {
    let tcp = tcp_connect(&server.host, server.port).await?;
    let mut client = match server.security {
        Security::Tls => {
            let mut c = async_imap::Client::new(tls_wrap(tcp, &server.host).await?);
            read_greeting(&mut c).await?;
            c
        }
        Security::None => {
            let mut c = async_imap::Client::new(Stream::Plain(tcp));
            read_greeting(&mut c).await?;
            c
        }
        Security::Starttls => {
            let mut c = async_imap::Client::new(Stream::Plain(tcp));
            read_greeting(&mut c).await?;
            tokio::time::timeout(COMMAND_TIMEOUT, c.run_command_and_check_ok("STARTTLS", None))
                .await
                .map_err(|_| coded(ErrorCode::Offline, "STARTTLS timeout"))?
                .context("STARTTLS")?;
            let Stream::Plain(tcp) = c.into_inner() else {
                unreachable!()
            };
            async_imap::Client::new(tls_wrap(tcp, &server.host).await?)
        }
    };
    // async-imap needs to know nothing else before auth; avoid an extra round trip.
    let _ = &mut client;
    let auth = async {
        match creds {
            Credentials::Password { user, password } => {
                client.login(user, password).await.map_err(|(e, _)| auth_error(e))
            }
            Credentials::OAuth2 { user, access_token } => client
                .authenticate(
                    "XOAUTH2",
                    XOAuth2 {
                        payload: xoauth2_payload(user, access_token),
                        sent: false,
                    },
                )
                .await
                .map_err(|(e, _)| auth_error(e)),
        }
    };
    let mut session = tokio::time::timeout(COMMAND_TIMEOUT, auth)
        .await
        .map_err(|_| coded(ErrorCode::Offline, "authentication timeout"))??;

    let caps = tokio::time::timeout(COMMAND_TIMEOUT, session.capabilities())
        .await
        .map_err(|_| coded(ErrorCode::Offline, "CAPABILITY timeout"))??;
    let c = Caps {
        idle: caps.has_str("IDLE"),
        mv: caps.has_str("MOVE"),
        uidplus: caps.has_str("UIDPLUS"),
        gmail: caps.has_str("X-GM-EXT-1"),
        condstore: caps.has_str("CONDSTORE"),
        special_use: caps.has_str("SPECIAL-USE"),
    };
    Ok((session, c))
}

/// Runs an IMAP future with the standard command timeout.
pub async fn tmo<T, E>(fut: impl Future<Output = std::result::Result<T, E>>) -> Result<T>
where
    E: std::error::Error + Send + Sync + 'static,
{
    tmo_for(COMMAND_TIMEOUT, fut).await
}

pub async fn tmo_for<T, E>(
    d: Duration,
    fut: impl Future<Output = std::result::Result<T, E>>,
) -> Result<T>
where
    E: std::error::Error + Send + Sync + 'static,
{
    match tokio::time::timeout(d, fut).await {
        Ok(r) => r.map_err(|e| anyhow::Error::new(e)),
        Err(_) => Err(coded(ErrorCode::Offline, "IMAP command timed out")),
    }
}

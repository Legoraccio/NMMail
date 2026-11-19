//! SMTP submission.

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, Result};
use lettre::address::Envelope;
use lettre::transport::smtp::authentication::{Credentials as SmtpCreds, Mechanism};
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::error::{ErrorCode, coded};
use crate::imap::conn::Credentials;
use crate::model::{Security, ServerConfig};

/// Submits a raw RFC 5322 message.
pub async fn send(
    server: &ServerConfig,
    creds: &Credentials,
    sender: &str,
    recipients: &[String],
    raw: &[u8],
) -> Result<()> {
    let builder = match server.security {
        Security::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&server.host)?,
        Security::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&server.host)?,
        Security::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&server.host),
    };
    let (c, mechs) = match creds {
        Credentials::Password { user, password } => (
            SmtpCreds::new(user.clone(), password.clone()),
            vec![Mechanism::Plain, Mechanism::Login],
        ),
        Credentials::OAuth2 { user, access_token } => (
            SmtpCreds::new(user.clone(), access_token.clone()),
            vec![Mechanism::Xoauth2],
        ),
    };
    let transport = builder
        .port(server.port)
        .credentials(c)
        .authentication(mechs)
        // Don't leak the machine name in EHLO / Received headers.
        .hello_name(ClientId::Ipv4(Ipv4Addr::LOCALHOST))
        .timeout(Some(Duration::from_secs(60)))
        .build();

    let envelope = Envelope::new(
        Some(sender.parse().context("invalid sender address")?),
        recipients
            .iter()
            .map(|r| r.parse())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| coded(ErrorCode::BadRequest, format!("invalid recipient: {e}")))?,
    )
    .map_err(|e| coded(ErrorCode::BadRequest, format!("{e}")))?;

    transport.send_raw(&envelope, raw).await.map_err(|e| {
        let msg = e.to_string();
        let lower = msg.to_lowercase();
        // A rejected XOAUTH2 token comes back as a "334 <error JSON>" challenge,
        // which lettre reports as an unexpected challenge.
        let code = if lower.contains("535")
            || lower.contains("534")
            || lower.contains("authentication")
            || lower.contains("does not expect a challenge")
        {
            ErrorCode::AuthRequired
        } else if e.is_permanent() {
            ErrorCode::Server
        } else {
            ErrorCode::Offline
        };
        coded(code, format!("SMTP: {msg}"))
    })?;
    Ok(())
}

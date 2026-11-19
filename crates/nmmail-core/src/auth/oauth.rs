//! OAuth 2.0 authorization code flow with PKCE and a loopback redirect
//! (RFC 8252), for Google and Microsoft accounts.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rand::RngExt as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

use crate::error::{ErrorCode, coded};
use crate::model::{Provider, Security, ServerConfig, now_ms};

pub const MS_CONSUMER_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// Static description of an OAuth provider.
pub struct Endpoints {
    pub auth_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static str,
    pub extra: &'static [(&'static str, &'static str)],
    /// Redirect host: Google wants the loopback IP, Microsoft registers "localhost".
    pub redirect_host: &'static str,
    /// Microsoft wants the scopes repeated when refreshing.
    pub refresh_scope: bool,
}

pub fn endpoints(p: Provider) -> Option<Endpoints> {
    match p {
        Provider::Gmail => Some(Endpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scopes: "https://mail.google.com/ openid email profile",
            extra: &[("access_type", "offline"), ("prompt", "consent")],
            redirect_host: "127.0.0.1",
            refresh_scope: false,
        }),
        Provider::Outlook => Some(Endpoints {
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            scopes: "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access openid email profile",
            extra: &[("prompt", "select_account")],
            redirect_host: "localhost",
            refresh_scope: true,
        }),
        Provider::Imap => None,
    }
}

/// Client registration supplied by the user (see docs/OAUTH_SETUP.md).
#[derive(Clone, Debug)]
pub struct ClientId {
    pub id: String,
    pub secret: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Tokens {
    pub access_token: String,
    pub expires_at: i64,
    pub refresh_token: Option<String>,
}

/// Who signed in, from the ID token.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub email: String,
    pub name: Option<String>,
    pub tenant: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// IMAP/SMTP servers for an OAuth provider and identity.
pub fn servers(p: Provider, id: &Identity) -> (ServerConfig, ServerConfig) {
    match p {
        Provider::Gmail => (
            ServerConfig {
                host: "imap.gmail.com".into(),
                port: 993,
                security: Security::Tls,
            },
            ServerConfig {
                host: "smtp.gmail.com".into(),
                port: 465,
                security: Security::Tls,
            },
        ),
        _ => {
            let personal = id.tenant.as_deref() == Some(MS_CONSUMER_TENANT);
            (
                ServerConfig {
                    host: "outlook.office365.com".into(),
                    port: 993,
                    security: Security::Tls,
                },
                ServerConfig {
                    host: if personal {
                        "smtp-mail.outlook.com".into()
                    } else {
                        "smtp.office365.com".into()
                    },
                    port: 587,
                    security: Security::Starttls,
                },
            )
        }
    }
}

fn random_string(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::rng();
    (0..len)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Decodes the (unverified) payload of a JWT received directly from the token endpoint over TLS.
pub fn identity_from_id_token(id_token: &str) -> Option<Identity> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .or_else(|_| STANDARD.decode(payload))
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let email = v
        .get("email")
        .and_then(|e| e.as_str())
        .or_else(|| v.get("preferred_username").and_then(|e| e.as_str()))?
        .to_string();
    Some(Identity {
        email,
        name: v
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string()),
        tenant: v.get("tid").and_then(|n| n.as_str()).map(|s| s.to_string()),
    })
}

fn http_agent() -> ureq::Agent {
    let cfg = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    ureq::Agent::new_with_config(cfg)
}

async fn post_token(url: &'static str, form: Vec<(&'static str, String)>) -> Result<TokenResponse> {
    tokio::task::spawn_blocking(move || -> Result<TokenResponse> {
        let mut resp = http_agent()
            .post(url)
            .send_form(form.iter().map(|(k, v)| (*k, v.as_str())))
            .map_err(|e| coded(ErrorCode::Offline, format!("token endpoint: {e}")))?;
        let body = resp.body_mut().read_to_string().context("token response")?;
        serde_json::from_str(&body).with_context(|| format!("unexpected token response: {body}"))
    })
    .await?
}

fn tokens_from(r: TokenResponse) -> Result<(Tokens, Option<String>)> {
    if let Some(err) = r.error {
        let desc = r.error_description.unwrap_or_default();
        let code =
            if err == "invalid_grant" || err == "interaction_required" || err == "invalid_client" {
                ErrorCode::AuthRequired
            } else {
                ErrorCode::Server
            };
        return Err(coded(code, format!("{err}: {desc}")));
    }
    let access_token = r
        .access_token
        .ok_or_else(|| anyhow!("no access_token in response"))?;
    Ok((
        Tokens {
            access_token,
            expires_at: now_ms() + r.expires_in.unwrap_or(3600) * 1000,
            refresh_token: r.refresh_token,
        },
        r.id_token,
    ))
}

/// Uses a refresh token to obtain a new access token.
pub async fn refresh(p: Provider, client: &ClientId, refresh_token: &str) -> Result<Tokens> {
    let ep = endpoints(p).ok_or_else(|| anyhow!("provider has no OAuth"))?;
    refresh_with(&ep, client, refresh_token).await
}

/// [`refresh`] against explicit endpoints.
pub async fn refresh_with(
    ep: &Endpoints,
    client: &ClientId,
    refresh_token: &str,
) -> Result<Tokens> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", client.id.clone()),
    ];
    if let Some(s) = &client.secret {
        form.push(("client_secret", s.clone()));
    }
    if ep.refresh_scope {
        form.push(("scope", ep.scopes.to_string()));
    }
    Ok(tokens_from(post_token(ep.token_url, form).await?)?.0)
}

/// Progress callback stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Browser,
    Exchanging,
}

/// Runs the interactive browser flow. `open_url` launches the system browser;
/// `cancel` aborts the wait.
pub async fn authorize(
    p: Provider,
    client: &ClientId,
    open_url: &(dyn Fn(&str) + Send + Sync),
    progress: &(dyn Fn(Stage, Option<&str>) + Send + Sync),
    cancel: Arc<Notify>,
) -> Result<(Tokens, Identity)> {
    let ep = endpoints(p).ok_or_else(|| anyhow!("provider has no OAuth"))?;
    authorize_with(&ep, client, open_url, progress, cancel).await
}

/// [`authorize`] against explicit endpoints.
pub async fn authorize_with(
    ep: &Endpoints,
    client: &ClientId,
    open_url: &(dyn Fn(&str) + Send + Sync),
    progress: &(dyn Fn(Stage, Option<&str>) + Send + Sync),
    cancel: Arc<Notify>,
) -> Result<(Tokens, Identity)> {
    if client.id.trim().is_empty() {
        return Err(coded(
            ErrorCode::BadRequest,
            "OAuth client ID not configured (Impostazioni → OAuth, see docs/OAUTH_SETUP.md)",
        ));
    }
    let v4 = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = v4.local_addr()?.port();
    // Browsers may resolve "localhost" to ::1: listen there too when possible.
    let v6 = TcpListener::bind(("::1", port)).await.ok();
    let redirect_uri = format!("http://{}:{port}", ep.redirect_host);

    let verifier = random_string(64);
    let state = random_string(24);
    let mut url = url::Url::parse(ep.auth_url)?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &client.id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", ep.scopes)
            .append_pair("state", &state)
            .append_pair("code_challenge", &pkce_challenge(&verifier))
            .append_pair("code_challenge_method", "S256");
        for (k, v) in ep.extra {
            q.append_pair(k, v);
        }
    }
    progress(Stage::Browser, Some(url.as_str()));
    open_url(url.as_str());

    let code = receive_code(&v4, v6.as_ref(), &state, cancel).await?;

    progress(Stage::Exchanging, None);
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client.id.clone()),
        ("code_verifier", verifier),
    ];
    if let Some(s) = &client.secret {
        form.push(("client_secret", s.clone()));
    }
    let (tokens, id_token) = tokens_from(post_token(ep.token_url, form).await?)?;
    if tokens.refresh_token.is_none() {
        bail!("the provider did not return a refresh token");
    }
    let identity = id_token
        .as_deref()
        .and_then(identity_from_id_token)
        .ok_or_else(|| anyhow!("could not determine the account e-mail address"))?;
    Ok((tokens, identity))
}

/// Waits for the browser to hit the loopback redirect and returns the code.
async fn receive_code(
    v4: &TcpListener,
    v6: Option<&TcpListener>,
    state: &str,
    cancel: Arc<Notify>,
) -> Result<String> {
    let wait = async {
        loop {
            let (mut sock, _) = match v6 {
                Some(v6) => tokio::select! {
                    r = v4.accept() => r?,
                    r = v6.accept() => r?,
                },
                None => v4.accept().await?,
            };
            match handle_redirect(&mut sock, state).await {
                Ok(Some(code)) => return Ok::<_, anyhow::Error>(code),
                Ok(None) => continue,
                Err(e) => return Err(e),
            }
        }
    };
    tokio::select! {
        r = wait => r,
        _ = cancel.notified() => Err(coded(ErrorCode::Cancelled, "sign-in cancelled")),
        _ = tokio::time::sleep(Duration::from_secs(600)) => Err(coded(ErrorCode::Cancelled, "sign-in timed out")),
    }
}

const PAGE_OK: &str = "<!DOCTYPE html><html lang=\"it\"><head><meta charset=\"utf-8\"><title>NM Mail</title>\
<style>body{font-family:'Segoe UI',system-ui,sans-serif;display:flex;align-items:center;justify-content:center;\
height:100vh;margin:0;background:#f3f6fb;color:#1b1b1b}div{text-align:center}h1{font-weight:600;font-size:22px}</style></head>\
<body><div><h1>&#10003; Accesso completato</h1><p>Puoi chiudere questa finestra e tornare a NM Mail.</p>\
<p style=\"color:#666\">Sign-in complete. You can close this window.</p></div></body></html>";

const PAGE_ERR: &str = "<!DOCTYPE html><html lang=\"it\"><head><meta charset=\"utf-8\"><title>NM Mail</title></head>\
<body style=\"font-family:'Segoe UI',sans-serif;text-align:center;padding-top:20vh\"><h1>Accesso non riuscito</h1>\
<p>Torna a NM Mail e riprova. / Sign-in failed, please try again.</p></body></html>";

/// Handles one HTTP request on the loopback listener.
/// Returns `Some(code)` on success, `None` for unrelated requests (favicon...).
async fn handle_redirect(sock: &mut tokio::net::TcpStream, state: &str) -> Result<Option<String>> {
    let mut buf = vec![0u8; 8192];
    let mut n = 0;
    loop {
        let r = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf[n..])).await;
        let Ok(Ok(read)) = r else { return Ok(None) };
        if read == 0 {
            break;
        }
        n += read;
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") || n == buf.len() {
            break;
        }
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let target = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");
    let url = url::Url::parse(&format!("http://localhost{target}"))?;
    let get = |k: &str| {
        url.query_pairs()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.into_owned())
    };
    let respond = |status: &str, body: &str| {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    if get("state").is_none() && get("error").is_none() {
        let _ = sock
            .write_all(respond("404 Not Found", "").as_bytes())
            .await;
        return Ok(None);
    }
    if let Some(err) = get("error") {
        let _ = sock.write_all(respond("200 OK", PAGE_ERR).as_bytes()).await;
        let code = if err == "access_denied" {
            ErrorCode::Cancelled
        } else {
            ErrorCode::Server
        };
        return Err(coded(code, format!("authorization error: {err}")));
    }
    if get("state").as_deref() != Some(state) {
        let _ = sock
            .write_all(respond("400 Bad Request", PAGE_ERR).as_bytes())
            .await;
        return Ok(None);
    }
    let Some(code) = get("code") else {
        let _ = sock
            .write_all(respond("400 Bad Request", PAGE_ERR).as_bytes())
            .await;
        return Ok(None);
    };
    let _ = sock.write_all(respond("200 OK", PAGE_OK).as_bytes()).await;
    let _ = sock.shutdown().await;
    Ok(Some(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn id_token_email() {
        let payload = URL_SAFE_NO_PAD.encode(
            br#"{"email":"mario@example.com","name":"Mario Rossi","tid":"9188040d-6c67-4c5b-b112-36a304b66dad"}"#,
        );
        let id = identity_from_id_token(&format!("h.{payload}.s")).unwrap();
        assert_eq!(id.email, "mario@example.com");
        assert_eq!(id.name.as_deref(), Some("Mario Rossi"));
        let (_, smtp) = servers(Provider::Outlook, &id);
        assert_eq!(smtp.host, "smtp-mail.outlook.com");
    }

    async fn browser_get(port: u16, path: &str) -> String {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).await.unwrap();
        resp
    }

    #[tokio::test]
    async fn loopback_redirect_delivers_code() {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let cancel = Arc::new(Notify::new());
        let recv = receive_code(&l, None, "st4te", cancel);
        let browser = async {
            let r = browser_get(port, "/favicon.ico").await;
            assert!(r.starts_with("HTTP/1.1 404"));
            let r = browser_get(port, "/?state=wrong&code=x").await;
            assert!(r.starts_with("HTTP/1.1 400"));
            let r = browser_get(port, "/?state=st4te&code=abc").await;
            assert!(r.contains("Accesso completato"));
        };
        let (code, _) = tokio::join!(recv, browser);
        assert_eq!(code.unwrap(), "abc");
    }

    /// Minimal token endpoint: answers one POST with `body`, returns the form it got.
    async fn mock_token_endpoint(l: TcpListener, body: String) -> String {
        let (mut s, _) = l.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = s.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(h) = text.find("\r\n\r\n") {
                let len: usize = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if buf.len() >= h + 4 + len {
                    let form = text[h + 4..h + 4 + len].to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    s.write_all(resp.as_bytes()).await.unwrap();
                    return form;
                }
            }
            if n == 0 {
                return String::new();
            }
        }
    }

    fn leaked_token_url(l: &TcpListener) -> &'static str {
        Box::leak(
            format!("http://127.0.0.1:{}/token", l.local_addr().unwrap().port()).into_boxed_str(),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn full_flow_against_mock_provider() {
        let token = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let ep = Endpoints {
            auth_url: "https://login.example.invalid/authorize",
            token_url: leaked_token_url(&token),
            scopes: "mail offline_access",
            extra: &[("prompt", "consent")],
            redirect_host: "127.0.0.1",
            refresh_scope: true,
        };
        let id_payload = URL_SAFE_NO_PAD.encode(br#"{"email":"mario@example.com","name":"Mario"}"#);
        let body = format!(
            r#"{{"access_token":"at-1","expires_in":3600,"refresh_token":"rt-1","id_token":"h.{id_payload}.s"}}"#
        );
        let server = tokio::spawn(mock_token_endpoint(token, body));

        let challenge = Arc::new(std::sync::Mutex::new(String::new()));
        let ch = challenge.clone();
        // The "browser" follows the authorization URL straight to the redirect.
        let open = move |auth_url: &str| {
            let u = url::Url::parse(auth_url).unwrap();
            let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
            assert_eq!(q["code_challenge_method"], "S256");
            assert_eq!(q["prompt"], "consent");
            *ch.lock().unwrap() = q["code_challenge"].clone();
            let port = url::Url::parse(&q["redirect_uri"]).unwrap().port().unwrap();
            let path = format!("/?code=c0de&state={}", q["state"]);
            tokio::spawn(async move {
                let r = browser_get(port, &path).await;
                assert!(r.contains("Accesso completato"));
            });
        };
        let progress = |_: Stage, _: Option<&str>| {};
        let client = ClientId {
            id: "client-1".into(),
            secret: Some("s3cret".into()),
        };
        let (tokens, id) = authorize_with(&ep, &client, &open, &progress, Arc::new(Notify::new()))
            .await
            .unwrap();
        assert_eq!(tokens.access_token, "at-1");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-1"));
        assert_eq!(id.email, "mario@example.com");

        // The token request carried the code and a verifier matching the challenge.
        let form: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(server.await.unwrap().as_bytes())
                .into_owned()
                .collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "c0de");
        assert_eq!(form["client_secret"], "s3cret");
        assert_eq!(
            pkce_challenge(&form["code_verifier"]),
            *challenge.lock().unwrap()
        );

        // Refresh (Microsoft-style: scopes repeated) and error mapping.
        let token = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let ep2 = Endpoints {
            token_url: leaked_token_url(&token),
            ..ep
        };
        let server = tokio::spawn(mock_token_endpoint(
            token,
            r#"{"error":"invalid_grant","error_description":"expired"}"#.into(),
        ));
        let err = refresh_with(&ep2, &client, "rt-1").await.unwrap_err();
        assert_eq!(crate::error::code_of(&err), ErrorCode::AuthRequired);
        let form = server.await.unwrap();
        assert!(form.contains("grant_type=refresh_token"));
        assert!(form.contains("scope=mail+offline_access"));
    }

    #[tokio::test]
    async fn loopback_cancel() {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let cancel = Arc::new(Notify::new());
        cancel.notify_one();
        let err = receive_code(&l, None, "s", cancel).await.unwrap_err();
        assert_eq!(crate::error::code_of(&err), ErrorCode::Cancelled);
    }
}

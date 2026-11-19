//! IPC protocol (see docs/IPC.md): JSON lines over a local stream.
//!
//! The OS layer accepts connections (named pipe / unix socket) and hands each
//! stream to [`serve_connection`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::compose::{Mode, SendRequest};
use crate::engine::{Engine, ImapAccountParams};
use crate::error::{ErrorCode, bad_request, code_of};
use crate::model::{MessageId, NotifyFilter, Provider};

pub const PROTOCOL_VERSION: u32 = 1;
const MAX_LINE: usize = 32 * 1024 * 1024;

#[derive(Deserialize)]
struct Request {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn p<T: for<'de> Deserialize<'de>>(params: &Value) -> Result<T> {
    let v = if params.is_null() { json!({}) } else { params.clone() };
    serde_json::from_value(v).map_err(|e| bad_request(format!("invalid params: {e}")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hello {
    #[serde(default)]
    client: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdParam {
    id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdsParam {
    ids: Vec<MessageId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OptAccount {
    account_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FolderParam {
    folder_id: i64,
}

#[derive(Deserialize)]
struct ProviderParam {
    provider: Provider,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountUpdate {
    id: i64,
    display_name: Option<String>,
    notify: Option<bool>,
    notify_filter: Option<NotifyFilter>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListParams {
    folder_id: Option<i64>,
    unified: Option<String>,
    before_date: Option<i64>,
    before_id: Option<i64>,
    limit: Option<u32>,
    #[serde(default)]
    unread_only: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OlderParams {
    folder_id: i64,
    before_date: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetParams {
    id: MessageId,
    #[serde(default)]
    allow_remote: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FlagParams {
    ids: Vec<MessageId>,
    seen: Option<bool>,
    flagged: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveParams {
    ids: Vec<MessageId>,
    folder_id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchParams {
    query: String,
    account_id: Option<i64>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveParams {
    message_id: MessageId,
    part_id: u32,
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuggestParams {
    prefix: String,
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareParams {
    mode: Mode,
    message_id: MessageId,
    #[serde(default)]
    lang: Option<String>,
}

#[derive(Deserialize)]
struct PauseParams {
    until: Option<i64>,
}

#[derive(Deserialize)]
struct ActivateParams {
    uri: String,
}

/// Executes one request.
pub async fn dispatch(engine: &Engine, method: &str, params: &Value) -> Result<Value> {
    Ok(match method {
        "hello" => json!({ "daemonVersion": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL_VERSION }),
        "daemon.status" => serde_json::to_value(engine.status()?)?,
        "accounts.list" => json!({ "accounts": engine.accounts()? }),
        "accounts.addOAuth" => {
            let a: ProviderParam = p(params)?;
            json!({ "account": engine.add_oauth(a.provider).await? })
        }
        "accounts.cancelAuth" => {
            engine.cancel_auth();
            json!({})
        }
        "accounts.addImap" => {
            let a: ImapAccountParams = p(params)?;
            json!({ "account": engine.add_imap(a).await? })
        }
        "accounts.update" => {
            let u: AccountUpdate = p(params)?;
            json!({ "account": engine.update_account(u.id, u.display_name.as_deref(), u.notify, u.notify_filter)? })
        }
        "accounts.remove" => {
            let a: IdParam = p(params)?;
            engine.remove_account(a.id)?;
            json!({})
        }
        "folders.list" => {
            let a: OptAccount = p(params)?;
            json!({ "folders": engine.folders(a.account_id)? })
        }
        "folders.sync" => {
            let f: FolderParam = p(params)?;
            engine.sync_folder(f.folder_id)?;
            json!({})
        }
        "messages.list" => {
            let l: ListParams = p(params)?;
            let before = l.before_date.map(|d| (d, l.before_id.unwrap_or(i64::MAX)));
            let (messages, has_more) = engine.list_messages(
                l.folder_id,
                l.unified.as_deref() == Some("inbox"),
                before,
                l.limit.unwrap_or(50),
                l.unread_only,
            )?;
            json!({ "messages": messages, "hasMore": has_more })
        }
        "messages.loadOlder" => {
            let o: OlderParams = p(params)?;
            let (messages, has_more) = engine.load_older(o.folder_id, o.before_date).await?;
            json!({ "messages": messages, "hasMore": has_more })
        }
        "messages.get" => {
            let g: GetParams = p(params)?;
            json!({ "message": engine.message(g.id, g.allow_remote).await? })
        }
        "messages.setFlags" => {
            let f: FlagParams = p(params)?;
            engine.set_flags(&f.ids, f.seen, f.flagged).await?;
            json!({})
        }
        "messages.archive" => {
            let i: IdsParam = p(params)?;
            engine.archive(&i.ids).await?;
            json!({})
        }
        "messages.delete" => {
            let i: IdsParam = p(params)?;
            engine.delete(&i.ids).await?;
            json!({})
        }
        "messages.move" => {
            let m: MoveParams = p(params)?;
            engine.move_to(&m.ids, m.folder_id).await?;
            json!({})
        }
        "messages.search" => {
            let s: SearchParams = p(params)?;
            json!({ "messages": engine.search(&s.query, s.account_id, s.limit.unwrap_or(100))? })
        }
        "attachments.save" => {
            let s: SaveParams = p(params)?;
            json!({ "path": engine.save_attachment(s.message_id, s.part_id, &s.path).await? })
        }
        "contacts.suggest" => {
            let s: SuggestParams = p(params)?;
            json!({ "contacts": engine.suggest_contacts(&s.prefix, s.limit.unwrap_or(8))? })
        }
        "compose.prepare" => {
            let c: PrepareParams = p(params)?;
            let lang = c.lang.unwrap_or_else(|| {
                let l = engine.settings().language;
                if l == "en" { "en".into() } else { "it".into() }
            });
            serde_json::to_value(engine.compose_prepare(c.mode, c.message_id, &lang).await?)?
        }
        "compose.send" => {
            let r: SendRequest = p(params)?;
            serde_json::to_value(engine.compose_send(r).await?)?
        }
        "settings.get" => json!({ "settings": engine.settings() }),
        "settings.set" => json!({ "settings": engine.set_settings(params)? }),
        "sync.now" => {
            let a: OptAccount = p(params)?;
            engine.sync_now(a.account_id);
            json!({})
        }
        "notifications.pause" => {
            let a: PauseParams = p(params)?;
            engine.pause_notifications(a.until)?;
            json!({})
        }
        "activate" => {
            let a: ActivateParams = p(params)?;
            engine.activate(&a.uri).await?;
            json!({})
        }
        "ui.show" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Show {
                message_id: Option<MessageId>,
            }
            let s: Show = p(params)?;
            engine.show_ui(s.message_id);
            json!({})
        }
        other => return Err(bad_request(format!("unknown method {other}"))),
    })
}

fn error_json(err: &anyhow::Error) -> Value {
    let code = code_of(err);
    let message = match code {
        // Keep the root cause readable for users.
        ErrorCode::Internal => format!("{err:#}"),
        _ => err
            .chain()
            .find_map(|c| c.downcast_ref::<crate::error::CodedError>().map(|c| c.message.clone()))
            .unwrap_or_else(|| format!("{err:#}")),
    };
    json!({ "code": code, "message": message })
}

/// Serves one client until it disconnects.
pub async fn serve_connection<S>(engine: Engine, stream: S)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (rd, mut wr) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let writer = tokio::spawn(async move {
        while let Some(mut line) = rx.recv().await {
            line.push('\n');
            if wr.write_all(line.as_bytes()).await.is_err() || wr.flush().await.is_err() {
                break;
            }
        }
    });

    let is_ui = Arc::new(AtomicBool::new(false));
    let mut events = engine.subscribe();
    let ev_tx = tx.clone();
    let forwarder = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(ev) => {
                    if let Ok(s) = serde_json::to_string(&ev)
                        && ev_tx.send(s).is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(lagged = n, "IPC client lagging behind events");
                }
                Err(_) => break,
            }
        }
    });

    let mut reader = BufReader::new(rd);
    let mut buf = Vec::with_capacity(4096);
    loop {
        buf.clear();
        match (&mut reader).take(MAX_LINE as u64).read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                debug!("IPC read error: {e}");
                break;
            }
        }
        if buf.len() >= MAX_LINE {
            warn!("IPC message too large; closing connection");
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(json!({ "id": null, "error": { "code": "badRequest", "message": e.to_string() } }).to_string());
                continue;
            }
        };
        if req.method == "hello"
            && let Ok(h) = p::<Hello>(&req.params)
            && h.client.eq_ignore_ascii_case("ui")
            && !is_ui.swap(true, Ordering::SeqCst)
        {
            engine.ui_attached(true);
        }
        let engine = engine.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let resp = match dispatch(&engine, &req.method, &req.params).await {
                Ok(result) => json!({ "id": req.id, "result": result }),
                Err(e) => {
                    debug!(method = %req.method, "request failed: {e:#}");
                    json!({ "id": req.id, "error": error_json(&e) })
                }
            };
            let _ = tx.send(resp.to_string());
        });
    }
    if is_ui.load(Ordering::SeqCst) {
        engine.ui_attached(false);
    }
    forwarder.abort();
    drop(tx);
    let _ = writer.await;
}

use tokio::io::AsyncReadExt as _;

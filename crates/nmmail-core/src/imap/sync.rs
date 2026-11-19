//! Folder synchronization primitives operating on an authenticated session.

use std::collections::HashSet;

use anyhow::Result;
use async_imap::types::{Fetch, Flag, NameAttribute};
use futures::TryStreamExt;
use tracing::{debug, info};

use super::conn::{Caps, Session, tmo};
use crate::mime;
use crate::model::{FolderRole, MessageId, flags, now_ms};
use crate::store::{FolderState, NewMessage, RemoteFolder, Store};

/// Upper bound of messages fetched during the first sync of a folder.
pub const MAX_INITIAL: usize = 5000;
const HEADER_CHUNK: usize = 200;

const HEADER_FIELDS: &str = "FROM TO CC REPLY-TO SENDER SUBJECT DATE MESSAGE-ID IN-REPLY-TO REFERENCES CONTENT-TYPE X-MS-HAS-ATTACH X-TEST-CATEGORY";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Only look for new messages (cheap; used on EXISTS).
    NewOnly,
    /// New messages + flags/expunges of the most recent cached messages.
    Recent,
    /// New messages + flags/expunges of the whole cache window.
    Full,
}

/// How many of the newest cached messages `SyncMode::Recent` re-checks.
pub const RECENT_WINDOW: usize = 300;

#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub new_ids: Vec<MessageId>,
    pub changed: bool,
    pub initial: bool,
    /// UIDs of the newly inserted messages, matching `new_ids`.
    pub new_uids: Vec<u32>,
}

/// Compresses UIDs into an IMAP sequence set ("1:4,7,9:10").
pub fn uid_set(uids: &[u32]) -> String {
    let mut v: Vec<u32> = uids.to_vec();
    v.sort_unstable();
    v.dedup();
    let mut out = String::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        let mut end = start;
        while i + 1 < v.len() && v[i + 1] == end + 1 {
            i += 1;
            end = v[i];
        }
        if !out.is_empty() {
            out.push(',');
        }
        if start == end {
            out.push_str(&start.to_string());
        } else {
            out.push_str(&format!("{start}:{end}"));
        }
        i += 1;
    }
    out
}

/// IMAP SEARCH date (e.g. `5-Mar-2026`) for a unix-ms timestamp.
pub fn imap_date(ms: i64) -> String {
    let dt = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
    dt.format("%-d-%b-%Y").to_string()
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Mailbox name as sent on the wire (modified UTF-7, quoted).
pub fn wire_name(name: &str) -> String {
    quote(name)
}

pub fn flags_of(f: &Fetch) -> i64 {
    let mut out = 0;
    for fl in f.flags() {
        out |= match fl {
            Flag::Seen => flags::SEEN,
            Flag::Flagged => flags::FLAGGED,
            Flag::Answered => flags::ANSWERED,
            Flag::Draft => flags::DRAFT,
            Flag::Deleted => flags::DELETED,
            _ => 0,
        };
    }
    out
}

fn new_message_from_fetch(f: &Fetch) -> Option<NewMessage> {
    let uid = f.uid?;
    let h = f.header().map(mime::parse_headers).unwrap_or_default();
    let internal = f.internal_date().map(|d| d.timestamp_millis());
    Some(NewMessage {
        uid,
        message_id: h.message_id,
        in_reply_to: h.in_reply_to,
        references: h.references,
        gm_msgid: f.gmail_msg_id().copied(),
        gm_thrid: None,
        subject: h.subject,
        from: h.from,
        to: h.to,
        cc: h.cc,
        reply_to: h.reply_to,
        date: internal.or(h.date).unwrap_or_else(now_ms),
        size: f.size.unwrap_or(0),
        flags: flags_of(f),
        snippet: String::new(),
        has_attachments: h.has_attachments_hint,
    })
}

/// Fetches header data for a UID set.
pub async fn fetch_headers(
    session: &mut Session,
    set: &str,
    caps: Caps,
) -> Result<Vec<NewMessage>> {
    let gm = if caps.gmail { " X-GM-MSGID" } else { "" };
    let query = format!(
        "(UID FLAGS INTERNALDATE RFC822.SIZE{gm} BODY.PEEK[HEADER.FIELDS ({HEADER_FIELDS})])"
    );
    let fetches: Vec<Fetch> = tmo(async {
        let stream = session.uid_fetch(set, &query).await?;
        stream.try_collect::<Vec<_>>().await
    })
    .await?;
    Ok(fetches.iter().filter_map(new_message_from_fetch).collect())
}

/// Fetches complete raw messages for UIDs (folder must be selected).
pub async fn fetch_bodies(session: &mut Session, uids: &[u32]) -> Result<Vec<(u32, Vec<u8>)>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set = uid_set(uids);
    let fetches: Vec<Fetch> = tmo(async {
        let stream = session.uid_fetch(&set, "(UID BODY.PEEK[])").await?;
        stream.try_collect::<Vec<_>>().await
    })
    .await?;
    Ok(fetches
        .iter()
        .filter_map(|f| Some((f.uid?, f.body()?.to_vec())))
        .collect())
}

/// Lists folders with their roles.
pub async fn list_folders(session: &mut Session) -> Result<Vec<RemoteFolder>> {
    let names: Vec<async_imap::types::Name> = tmo(async {
        let stream = session.list(Some(""), Some("*")).await?;
        stream.try_collect::<Vec<_>>().await
    })
    .await?;
    let mut out = Vec::new();
    for n in &names {
        let mut role = None;
        let mut selectable = true;
        for a in n.attributes() {
            match a {
                NameAttribute::NoSelect => selectable = false,
                NameAttribute::All => role = Some(FolderRole::All),
                NameAttribute::Archive => role = Some(FolderRole::Archive),
                NameAttribute::Drafts => role = Some(FolderRole::Drafts),
                NameAttribute::Flagged => role = Some(FolderRole::Flagged),
                NameAttribute::Junk => role = Some(FolderRole::Spam),
                NameAttribute::Sent => role = Some(FolderRole::Sent),
                NameAttribute::Trash => role = Some(FolderRole::Trash),
                NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\Important") => {
                    role = Some(FolderRole::Important)
                }
                NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\NonExistent") => {
                    selectable = false
                }
                _ => {}
            }
        }
        let name = n.name().to_string();
        if name.eq_ignore_ascii_case("INBOX") {
            role = Some(FolderRole::Inbox);
        }
        if role.is_none() {
            role = guess_role(&name);
        }
        out.push(RemoteFolder {
            name,
            delimiter: n.delimiter().map(|d| d.to_string()),
            role,
            selectable,
        });
    }
    // Only one folder per role (servers without SPECIAL-USE can match several names).
    let mut taken = HashSet::new();
    for f in out.iter_mut() {
        if let Some(r) = f.role
            && !taken.insert(r)
        {
            f.role = None;
        }
    }
    Ok(out)
}

fn guess_role(name: &str) -> Option<FolderRole> {
    let leaf = super::utf7::decode(name.rsplit(['/', '.']).next().unwrap_or(name)).to_lowercase();
    Some(match leaf.as_str() {
        "sent" | "sent items" | "sent mail" | "sent messages" | "posta inviata" | "inviata"
        | "inviati" | "elementi inviati" => FolderRole::Sent,
        "drafts" | "draft" | "bozze" => FolderRole::Drafts,
        "trash" | "deleted" | "deleted items" | "deleted messages" | "cestino"
        | "posta eliminata" | "elementi eliminati" => FolderRole::Trash,
        "junk" | "spam" | "junk email" | "junk e-mail" | "posta indesiderata" => FolderRole::Spam,
        "archive" | "archives" | "archivio" => FolderRole::Archive,
        _ => return None,
    })
}

/// Changes reported by unsolicited responses.
#[derive(Debug, Default, Clone, Copy)]
pub struct Changes {
    pub exists: bool,
    pub expunge: bool,
    pub flags: bool,
}

impl Changes {
    pub fn any(&self) -> bool {
        self.exists || self.expunge || self.flags
    }
    pub fn merge(&mut self, o: Changes) {
        self.exists |= o.exists;
        self.expunge |= o.expunge;
        self.flags |= o.flags;
    }
}

/// Classifies a raw response received during IDLE.
pub fn classify(resp: &async_imap::imap_proto::Response<'_>) -> Changes {
    use async_imap::imap_proto::{MailboxDatum, Response};
    let mut c = Changes::default();
    match resp {
        Response::MailboxData(MailboxDatum::Exists(_))
        | Response::MailboxData(MailboxDatum::Recent(_)) => c.exists = true,
        Response::Expunge(_) | Response::Vanished { .. } => c.expunge = true,
        Response::Fetch(..) => c.flags = true,
        _ => {}
    }
    c
}

/// Drains queued unsolicited responses.
pub fn drain_unsolicited(session: &mut Session) -> Changes {
    use async_imap::types::UnsolicitedResponse as U;
    let mut c = Changes::default();
    while let Ok(r) = session.unsolicited_responses.try_recv() {
        match r {
            U::Exists(_) | U::Recent(_) => c.exists = true,
            U::Expunge(_) => c.expunge = true,
            U::Other(resp) => c.merge(classify(resp.parsed())),
            _ => {}
        }
    }
    c
}

/// Selects a folder and brings the local cache up to date.
pub async fn sync_folder(
    session: &mut Session,
    store: &Store,
    folder: &FolderState,
    caps: Caps,
    cache_days: u32,
    mode: SyncMode,
) -> Result<SyncOutcome> {
    let wire = wire_name(&folder.name);
    let mbox = if caps.condstore {
        tmo(session.select_condstore(&wire)).await?
    } else {
        tmo(session.select(&wire)).await?
    };
    drain_unsolicited(session);
    let uidvalidity = mbox.uid_validity.unwrap_or(0);
    let mut folder = folder.clone();
    if folder.uidvalidity.is_some_and(|v| v != uidvalidity) {
        info!(folder = %folder.name, "UIDVALIDITY changed, resetting cache");
        store.reset_folder(folder.id)?;
        folder = store.folder_state(folder.id)?;
    }
    let initial = folder.uidvalidity.is_none() || folder.last_sync.is_none();
    let mut out = SyncOutcome {
        initial,
        ..Default::default()
    };
    let mut highest = folder.highest_uid;
    let mut oldest = None;
    // The stored HIGHESTMODSEQ may only advance when flags were fully reconciled.
    let mut flags_synced = initial || mbox.exists == 0;

    if mbox.exists == 0 {
        if !initial {
            let local: Vec<u32> = store
                .folder_uids(folder.id)?
                .into_iter()
                .map(|(u, _)| u)
                .collect();
            if !local.is_empty() {
                store.delete_uids(folder.id, &local)?;
                out.changed = true;
            }
        }
    } else if initial {
        let query = if cache_days == 0 {
            "ALL".to_string()
        } else {
            let since = now_ms() - cache_days as i64 * 86_400_000;
            oldest = Some(since);
            format!("SINCE {}", imap_date(since))
        };
        let mut uids: Vec<u32> = tmo(session.uid_search(&query)).await?.into_iter().collect();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        uids.truncate(MAX_INITIAL);
        debug!(folder = %folder.name, count = uids.len(), "initial sync");
        for chunk in uids.chunks(HEADER_CHUNK) {
            let msgs = fetch_headers(session, &uid_set(chunk), caps).await?;
            store.upsert_messages(folder.account_id, folder.id, &msgs)?;
        }
        highest = highest.max(uids.first().copied().unwrap_or(0));
        out.changed = true;
    } else {
        let from = folder.highest_uid.saturating_add(1).max(1);
        if mbox.uid_next.is_none_or(|n| n > from) {
            // "n:*" returns the last message even if its UID < n: filter.
            let msgs: Vec<NewMessage> = fetch_headers(session, &format!("{from}:*"), caps)
                .await?
                .into_iter()
                .filter(|m| m.uid >= from)
                .collect();
            if !msgs.is_empty() {
                let ids = store.upsert_messages(folder.account_id, folder.id, &msgs)?;
                highest = highest.max(msgs.iter().map(|m| m.uid).max().unwrap_or(0));
                // upsert returns ids in insertion order, matching `msgs` minus duplicates.
                let inserted: HashSet<i64> = ids.iter().copied().collect();
                out.new_uids = msgs.iter().map(|m| m.uid).collect();
                out.new_ids = ids;
                out.changed |= !inserted.is_empty();
            }
        }
        let limit = match mode {
            SyncMode::NewOnly => None,
            SyncMode::Recent => Some(Some(RECENT_WINDOW)),
            SyncMode::Full => Some(None),
        };
        if let Some(limit) = limit {
            out.changed |= resync_flags_and_expunges(
                session,
                store,
                &folder,
                caps,
                mbox.highest_modseq,
                limit,
            )
            .await?;
            flags_synced = true;
        }
    }

    // Everything below UIDNEXT has been seen (fetched or deliberately skipped
    // as older than the cache window), so new mail is always > highest.
    let highest = highest.max(mbox.uid_next.unwrap_or(1).saturating_sub(1));
    store.update_folder_sync(
        folder.id,
        uidvalidity,
        mbox.uid_next,
        highest,
        if flags_synced {
            mbox.highest_modseq
        } else {
            None
        },
        oldest,
    )?;
    Ok(out)
}

async fn resync_flags_and_expunges(
    session: &mut Session,
    store: &Store,
    folder: &FolderState,
    caps: Caps,
    server_modseq: Option<u64>,
    limit: Option<usize>,
) -> Result<bool> {
    let all = store.folder_uids(folder.id)?;
    let Some(full_low) = all.first().map(|(u, _)| *u) else {
        return Ok(false);
    };
    let mut local = all;
    if let Some(n) = limit
        && local.len() > n
    {
        local.drain(..local.len() - n);
    }
    let low = local.first().map(|(u, _)| *u).unwrap_or(full_low);
    let mut changed = false;

    // Flags: CONDSTORE lets the server send only what changed (always over the
    // whole window, so HIGHESTMODSEQ can safely advance).
    let flag_query = match (caps.condstore, folder.highest_modseq, server_modseq) {
        (true, Some(local_ms), Some(server_ms)) if server_ms == local_ms => None,
        (true, Some(local_ms), Some(_)) => Some(format!("(UID FLAGS) (CHANGEDSINCE {local_ms})")),
        _ => Some("(UID FLAGS)".to_string()),
    };
    if let Some(q) = flag_query {
        let set = if caps.condstore {
            format!("{full_low}:*")
        } else {
            format!("{low}:*")
        };
        let fetches: Vec<Fetch> = tmo(async {
            let stream = session.uid_fetch(&set, &q).await?;
            stream.try_collect::<Vec<_>>().await
        })
        .await?;
        let pairs: Vec<(u32, i64)> = fetches
            .iter()
            .filter_map(|f| Some((f.uid?, flags_of(f))))
            .collect();
        changed |= store.apply_flags(folder.id, &pairs)? > 0;
    }

    // Expunges: compare the cached UID range with what the server still has.
    let server: HashSet<u32> = tmo(session.uid_search(format!("UID {low}:*"))).await?;
    let gone: Vec<u32> = local
        .iter()
        .map(|(u, _)| *u)
        .filter(|u| !server.contains(u))
        .collect();
    if !gone.is_empty() {
        store.delete_uids(folder.id, &gone)?;
        changed = true;
    }
    Ok(changed)
}

/// Fetches up to `limit` messages older than the cache window (for scrolling back).
pub async fn load_older(
    session: &mut Session,
    store: &Store,
    folder: &FolderState,
    caps: Caps,
    before_ms: i64,
    limit: usize,
) -> Result<Vec<MessageId>> {
    tmo(session.select(wire_name(&folder.name))).await?;
    drain_unsolicited(session);
    // SEARCH BEFORE has day granularity: include the boundary day and filter locally.
    let day_after = before_ms + 86_400_000;
    let mut uids: Vec<u32> = tmo(session.uid_search(format!("BEFORE {}", imap_date(day_after))))
        .await?
        .into_iter()
        .collect();
    let known: HashSet<u32> = store
        .folder_uids(folder.id)?
        .into_iter()
        .map(|(u, _)| u)
        .collect();
    uids.retain(|u| !known.contains(u));
    uids.sort_unstable_by(|a, b| b.cmp(a));
    uids.truncate(limit);
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let msgs = fetch_headers(session, &uid_set(&uids), caps).await?;
    store.upsert_messages(folder.account_id, folder.id, &msgs)
}

/// Adds or removes a system flag on UIDs of the selected folder.
pub async fn store_flag(session: &mut Session, uids: &[u32], flag: &str, add: bool) -> Result<()> {
    if uids.is_empty() {
        return Ok(());
    }
    let op = if add {
        "+FLAGS.SILENT"
    } else {
        "-FLAGS.SILENT"
    };
    tmo(async {
        let stream = session
            .uid_store(uid_set(uids), format!("{op} ({flag})"))
            .await?;
        stream.try_collect::<Vec<_>>().await
    })
    .await?;
    Ok(())
}

/// Moves UIDs of the selected folder into `target`.
pub async fn move_uids(
    session: &mut Session,
    uids: &[u32],
    target: &str,
    caps: Caps,
) -> Result<()> {
    if uids.is_empty() {
        return Ok(());
    }
    let set = uid_set(uids);
    if caps.mv {
        tmo(session.uid_mv(&set, wire_name(target))).await?;
    } else {
        tmo(session.uid_copy(&set, wire_name(target))).await?;
        store_flag(session, uids, "\\Deleted", true).await?;
        if caps.uidplus {
            tmo(async {
                let stream = session.uid_expunge(&set).await?;
                stream.try_collect::<Vec<_>>().await
            })
            .await?;
        } else {
            tmo(async {
                let stream = session.expunge().await?;
                stream.try_collect::<Vec<_>>().await
            })
            .await?;
        }
    }
    Ok(())
}

/// Permanently deletes UIDs of the selected folder.
pub async fn expunge_uids(session: &mut Session, uids: &[u32], caps: Caps) -> Result<()> {
    store_flag(session, uids, "\\Deleted", true).await?;
    let set = uid_set(uids);
    if caps.uidplus {
        tmo(async {
            let stream = session.uid_expunge(&set).await?;
            stream.try_collect::<Vec<_>>().await
        })
        .await?;
    } else {
        tmo(async {
            let stream = session.expunge().await?;
            stream.try_collect::<Vec<_>>().await
        })
        .await?;
    }
    Ok(())
}

/// Gmail: which of `uids` belong to the Primary category.
pub async fn gmail_primary(session: &mut Session, uids: &[u32]) -> Result<HashSet<u32>> {
    if uids.is_empty() {
        return Ok(HashSet::new());
    }
    tmo(session.uid_search(format!(
        "UID {} X-GM-RAW \"category:primary\"",
        uid_set(uids)
    )))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_sets() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 9, 7, 8]), "1:3,5,7:9");
        assert_eq!(uid_set(&[4]), "4");
        assert_eq!(uid_set(&[]), "");
    }

    #[test]
    fn search_date() {
        // 2026-03-05T12:00:00Z
        assert_eq!(imap_date(1_772_712_000_000), "5-Mar-2026");
    }

    #[test]
    fn role_guessing() {
        assert_eq!(guess_role("INBOX/Sent Items"), Some(FolderRole::Sent));
        assert_eq!(guess_role("Posta eliminata"), Some(FolderRole::Trash));
        assert_eq!(guess_role("Progetti"), None);
    }
}

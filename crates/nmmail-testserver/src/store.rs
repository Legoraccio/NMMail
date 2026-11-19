//! The in-memory mail store: users, mailboxes and messages.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, FixedOffset, Local, NaiveDate, Timelike};

use crate::StoredMessage;
use crate::mime::{self, Part, SearchText};

/// System flags in the order they are reported.
pub(crate) const SYSTEM_FLAGS: [&str; 5] =
    ["\\Answered", "\\Flagged", "\\Deleted", "\\Seen", "\\Draft"];

/// Hierarchy delimiter of every mailbox name.
pub(crate) const DELIMITER: char = '/';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpecialUse {
    All,
    Archive,
    Drafts,
    Junk,
    Sent,
    Trash,
}

impl SpecialUse {
    pub(crate) fn attribute(self) -> &'static str {
        match self {
            SpecialUse::All => "\\All",
            SpecialUse::Archive => "\\Archive",
            SpecialUse::Drafts => "\\Drafts",
            SpecialUse::Junk => "\\Junk",
            SpecialUse::Sent => "\\Sent",
            SpecialUse::Trash => "\\Trash",
        }
    }
}

/// Immutable message content, shared between copies of the same message.
pub(crate) struct MessageData {
    pub raw: Vec<u8>,
    pub internal_date: DateTime<FixedOffset>,
    pub mime: Part,
    pub search: SearchText,
    pub message_id: Option<String>,
    /// Lowercase value of `X-Test-Category` (Gmail category emulation).
    pub category: Option<String>,
    /// Date of the `Date:` header (for SENTBEFORE / SENTON / SENTSINCE).
    pub sent_date: Option<NaiveDate>,
    pub gm_msgid: u64,
    pub gm_thrid: u64,
}

impl MessageData {
    fn new(
        raw: Vec<u8>,
        internal_date: DateTime<FixedOffset>,
        gm_msgid: u64,
        gm_thrid: u64,
    ) -> MessageData {
        let mime = mime::parse_message(&raw);
        let (fields, _) = mime::parse_header_block(&raw, 0, raw.len());
        let message_id = mime::header_value(&fields, "message-id")
            .and_then(|v| mime::message_ids(v).into_iter().next());
        let category = mime::header_value(&fields, "x-test-category")
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        let sent_date = mime::header_value(&fields, "date")
            .and_then(mail_parser::DateTime::parse_rfc822)
            .and_then(|d| {
                NaiveDate::from_ymd_opt(i32::from(d.year), u32::from(d.month), u32::from(d.day))
            });
        let search = mime::search_text(&raw, &fields, &mime);
        MessageData {
            raw,
            internal_date,
            mime,
            search,
            message_id,
            category,
            sent_date,
            gm_msgid,
            gm_thrid,
        }
    }
}

/// A message inside a mailbox.
#[derive(Clone)]
pub(crate) struct Message {
    pub uid: u32,
    /// Sorted, deduplicated flags (see [`sort_flags`]); never contains `\Recent`.
    pub flags: Vec<String>,
    /// Not yet reported as recent to any session.
    pub recent: bool,
    pub data: Arc<MessageData>,
}

pub(crate) struct Mailbox {
    pub name: String,
    pub special_use: Option<SpecialUse>,
    pub selectable: bool,
    pub uid_validity: u32,
    pub uid_next: u32,
    /// Sorted by UID.
    pub messages: Vec<Message>,
}

impl Mailbox {
    fn new(
        name: &str,
        special_use: Option<SpecialUse>,
        selectable: bool,
        uid_validity: u32,
    ) -> Mailbox {
        Mailbox {
            name: name.to_string(),
            special_use,
            selectable,
            uid_validity,
            uid_next: 1,
            messages: Vec::new(),
        }
    }

    pub(crate) fn get(&self, uid: u32) -> Option<&Message> {
        self.messages
            .binary_search_by_key(&uid, |m| m.uid)
            .ok()
            .map(|i| &self.messages[i])
    }

    pub(crate) fn get_mut(&mut self, uid: u32) -> Option<&mut Message> {
        match self.messages.binary_search_by_key(&uid, |m| m.uid) {
            Ok(i) => Some(&mut self.messages[i]),
            Err(_) => None,
        }
    }

    pub(crate) fn unseen_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| !has_flag(&m.flags, "\\Seen"))
            .count()
    }

    pub(crate) fn recent_count(&self) -> usize {
        self.messages.iter().filter(|m| m.recent).count()
    }
}

pub(crate) struct User {
    pub password: String,
    /// In LIST order.
    pub mailboxes: Vec<Mailbox>,
}

impl User {
    fn position(&self, name: &str) -> Option<usize> {
        if name.eq_ignore_ascii_case("INBOX") {
            self.mailboxes.iter().position(|m| m.name == "INBOX")
        } else {
            self.mailboxes.iter().position(|m| m.name == name)
        }
    }

    pub(crate) fn mailbox(&self, name: &str) -> Option<&Mailbox> {
        self.position(name).map(|i| &self.mailboxes[i])
    }

    pub(crate) fn mailbox_mut(&mut self, name: &str) -> Option<&mut Mailbox> {
        self.position(name).map(move |i| &mut self.mailboxes[i])
    }

    pub(crate) fn special(&self, special_use: SpecialUse) -> Option<&Mailbox> {
        self.mailboxes
            .iter()
            .find(|m| m.special_use == Some(special_use))
    }

    pub(crate) fn has_children(&self, name: &str) -> bool {
        let prefix = format!("{name}{DELIMITER}");
        self.mailboxes.iter().any(|m| m.name.starts_with(&prefix))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreError {
    NoSuchUser,
    NoSuchMailbox,
    NotSelectable,
    AlreadyExists,
    InvalidName,
    CannotDeleteInbox,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StoreError::NoSuchUser => "unknown user",
            StoreError::NoSuchMailbox => "mailbox does not exist",
            StoreError::NotSelectable => "mailbox is not selectable",
            StoreError::AlreadyExists => "mailbox already exists",
            StoreError::InvalidName => "invalid mailbox name",
            StoreError::CannotDeleteInbox => "INBOX cannot be deleted or renamed",
        })
    }
}

pub(crate) struct Store {
    /// Keyed by lowercase email.
    users: BTreeMap<String, User>,
    next_uidvalidity: u32,
    next_gm_id: u64,
    gmail_mode: bool,
}

fn now_seconds() -> DateTime<FixedOffset> {
    let now = Local::now().fixed_offset();
    now.with_nanosecond(0).unwrap_or(now)
}

pub(crate) fn user_key(email: &str) -> String {
    email.trim().to_lowercase()
}

impl Store {
    pub(crate) fn new(users: &[(String, String)], gmail_mode: bool) -> Store {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(1);
        let mut store = Store {
            users: BTreeMap::new(),
            // Real servers use timestamps: a restarted server presents new UIDVALIDITYs.
            next_uidvalidity: u32::try_from(secs % 3_000_000_000).unwrap_or(1).max(1),
            next_gm_id: 1_780_000_000_000_000_000 + (secs % 1_000_000) * 1_000_000,
            gmail_mode,
        };
        for (email, password) in users {
            let key = user_key(email);
            if key.is_empty() || store.users.contains_key(&key) {
                continue;
            }
            let mailboxes = store.default_mailboxes();
            store.users.insert(
                key,
                User {
                    password: password.clone(),
                    mailboxes,
                },
            );
        }
        store
    }

    fn default_mailboxes(&mut self) -> Vec<Mailbox> {
        let layout: &[(&str, Option<SpecialUse>, bool)] = if self.gmail_mode {
            &[
                ("INBOX", None, true),
                ("[Gmail]", None, false),
                ("[Gmail]/All Mail", Some(SpecialUse::All), true),
                ("[Gmail]/Drafts", Some(SpecialUse::Drafts), true),
                ("[Gmail]/Sent Mail", Some(SpecialUse::Sent), true),
                ("[Gmail]/Spam", Some(SpecialUse::Junk), true),
                ("[Gmail]/Trash", Some(SpecialUse::Trash), true),
            ]
        } else {
            &[
                ("INBOX", None, true),
                ("Sent", Some(SpecialUse::Sent), true),
                ("Drafts", Some(SpecialUse::Drafts), true),
                ("Trash", Some(SpecialUse::Trash), true),
                ("Junk", Some(SpecialUse::Junk), true),
                ("Archive", Some(SpecialUse::Archive), true),
            ]
        };
        layout
            .iter()
            .map(|&(name, special_use, selectable)| {
                Mailbox::new(name, special_use, selectable, self.new_uidvalidity())
            })
            .collect()
    }

    fn new_uidvalidity(&mut self) -> u32 {
        let value = self.next_uidvalidity;
        self.next_uidvalidity = self.next_uidvalidity.checked_add(1).unwrap_or(1);
        value
    }

    /// Lowercase key of an existing user.
    pub(crate) fn user_key(&self, email: &str) -> Option<String> {
        let key = user_key(email);
        self.users.contains_key(&key).then_some(key)
    }

    pub(crate) fn check_password(&self, email: &str, password: &str) -> Option<String> {
        let key = user_key(email);
        let user = self.users.get(&key)?;
        (user.password == password).then_some(key)
    }

    pub(crate) fn user(&self, email: &str) -> Option<&User> {
        self.users.get(&user_key(email))
    }

    pub(crate) fn user_mut(&mut self, email: &str) -> Option<&mut User> {
        self.users.get_mut(&user_key(email))
    }

    pub(crate) fn mailbox(&self, email: &str, name: &str) -> Option<&Mailbox> {
        self.user(email)?.mailbox(name)
    }

    pub(crate) fn mailbox_mut(&mut self, email: &str, name: &str) -> Option<&mut Mailbox> {
        self.user_mut(email)?.mailbox_mut(name)
    }

    /// Appends a new message; returns `(uidvalidity, uid)`.
    pub(crate) fn append(
        &mut self,
        email: &str,
        mailbox: &str,
        raw: Vec<u8>,
        flags: &[String],
        internal_date: Option<DateTime<FixedOffset>>,
        recent: bool,
    ) -> Result<(u32, u32), StoreError> {
        let user = self.user(email).ok_or(StoreError::NoSuchUser)?;
        let target = user.mailbox(mailbox).ok_or(StoreError::NoSuchMailbox)?;
        if !target.selectable {
            return Err(StoreError::NotSelectable);
        }
        let references = {
            let (fields, _) = mime::parse_header_block(&raw, 0, raw.len());
            let mut ids = Vec::new();
            for name in ["in-reply-to", "references"] {
                if let Some(value) = mime::header_value(&fields, name) {
                    ids.extend(mime::message_ids(value));
                }
            }
            ids
        };
        let thread = user
            .mailboxes
            .iter()
            .flat_map(|m| m.messages.iter())
            .find(|m| {
                m.data
                    .message_id
                    .as_ref()
                    .is_some_and(|id| references.contains(id))
            })
            .map(|m| m.data.gm_thrid);
        let gm_msgid = self.next_gm_id;
        self.next_gm_id += 1;
        let internal_date = internal_date.unwrap_or_else(now_seconds);
        let data = Arc::new(MessageData::new(
            raw,
            internal_date,
            gm_msgid,
            thread.unwrap_or(gm_msgid),
        ));
        self.append_data(email, mailbox, data, flags.to_vec(), recent)
    }

    fn append_data(
        &mut self,
        email: &str,
        mailbox: &str,
        data: Arc<MessageData>,
        flags: Vec<String>,
        recent: bool,
    ) -> Result<(u32, u32), StoreError> {
        let target = self
            .user_mut(email)
            .ok_or(StoreError::NoSuchUser)?
            .mailbox_mut(mailbox)
            .ok_or(StoreError::NoSuchMailbox)?;
        if !target.selectable {
            return Err(StoreError::NotSelectable);
        }
        let uid = target.uid_next;
        target.uid_next = target.uid_next.saturating_add(1);
        target.messages.push(Message {
            uid,
            flags,
            recent,
            data,
        });
        Ok((target.uid_validity, uid))
    }

    /// Copies messages (flags and content preserved, `\Recent` set). Returns the
    /// destination UIDVALIDITY and the `(source uid, new uid)` pairs.
    pub(crate) fn copy(
        &mut self,
        email: &str,
        source: &str,
        uids: &[u32],
        destination: &str,
    ) -> Result<(u32, Vec<(u32, u32)>), StoreError> {
        let user = self.user(email).ok_or(StoreError::NoSuchUser)?;
        let src = user.mailbox(source).ok_or(StoreError::NoSuchMailbox)?;
        let dst = user.mailbox(destination).ok_or(StoreError::NoSuchMailbox)?;
        if !dst.selectable {
            return Err(StoreError::NotSelectable);
        }
        let dst_validity = dst.uid_validity;
        let originals: Vec<Message> = uids
            .iter()
            .filter_map(|&uid| src.get(uid).cloned())
            .collect();
        let mut pairs = Vec::with_capacity(originals.len());
        for original in originals {
            let (_, new_uid) =
                self.append_data(email, destination, original.data, original.flags, true)?;
            pairs.push((original.uid, new_uid));
        }
        Ok((dst_validity, pairs))
    }

    /// Removes messages; returns how many were removed.
    pub(crate) fn remove_uids(&mut self, email: &str, mailbox: &str, uids: &[u32]) -> usize {
        let Some(target) = self.mailbox_mut(email, mailbox) else {
            return 0;
        };
        let before = target.messages.len();
        target.messages.retain(|m| !uids.contains(&m.uid));
        before - target.messages.len()
    }

    /// Replaces the flags of a message; returns whether they changed.
    pub(crate) fn replace_flags(
        &mut self,
        email: &str,
        mailbox: &str,
        uid: u32,
        flags: Vec<String>,
    ) -> bool {
        match self
            .mailbox_mut(email, mailbox)
            .and_then(|m| m.get_mut(uid))
        {
            Some(message) if message.flags != flags => {
                message.flags = flags;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn snapshot(&self, email: &str, mailbox: &str) -> Vec<StoredMessage> {
        self.mailbox(email, mailbox)
            .map(|m| {
                m.messages
                    .iter()
                    .map(|msg| StoredMessage {
                        uid: msg.uid,
                        flags: msg.flags.clone(),
                        raw: msg.data.raw.clone(),
                        internal_date: msg.data.internal_date,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn uid_validity(&self, email: &str, mailbox: &str) -> Option<u32> {
        self.mailbox(email, mailbox).map(|m| m.uid_validity)
    }

    /// Bumps UIDVALIDITY and renumbers the messages from 1.
    pub(crate) fn reset_uidvalidity(&mut self, email: &str, mailbox: &str) -> bool {
        let validity = self.new_uidvalidity();
        let Some(target) = self.mailbox_mut(email, mailbox) else {
            return false;
        };
        target.uid_validity = validity;
        let mut uid = 0;
        for message in &mut target.messages {
            uid += 1;
            message.uid = uid;
        }
        target.uid_next = uid + 1;
        true
    }

    pub(crate) fn create_mailbox(&mut self, email: &str, name: &str) -> Result<(), StoreError> {
        let name = name.trim_end_matches(DELIMITER);
        if name.is_empty()
            || name.contains(['*', '%', '\r', '\n'])
            || name.split(DELIMITER).any(str::is_empty)
        {
            return Err(StoreError::InvalidName);
        }
        let user = self.user(email).ok_or(StoreError::NoSuchUser)?;
        if user.mailbox(name).is_some() {
            return Err(StoreError::AlreadyExists);
        }
        let mut missing = Vec::new();
        let mut prefix = String::new();
        for component in name.split(DELIMITER) {
            if !prefix.is_empty() {
                prefix.push(DELIMITER);
            }
            prefix.push_str(component);
            if user.mailbox(&prefix).is_none() {
                missing.push(prefix.clone());
            }
        }
        let new: Vec<Mailbox> = missing
            .iter()
            .map(|n| Mailbox::new(n, None, true, self.new_uidvalidity()))
            .collect();
        let user = self.user_mut(email).ok_or(StoreError::NoSuchUser)?;
        user.mailboxes.extend(new);
        Ok(())
    }

    pub(crate) fn delete_mailbox(&mut self, email: &str, name: &str) -> Result<(), StoreError> {
        if name.eq_ignore_ascii_case("INBOX") {
            return Err(StoreError::CannotDeleteInbox);
        }
        let user = self.user_mut(email).ok_or(StoreError::NoSuchUser)?;
        let index = user.position(name).ok_or(StoreError::NoSuchMailbox)?;
        if user.has_children(name) {
            // RFC 3501: a mailbox with inferiors becomes \Noselect.
            if !user.mailboxes[index].selectable {
                return Err(StoreError::NotSelectable);
            }
            let mailbox = &mut user.mailboxes[index];
            mailbox.selectable = false;
            mailbox.messages.clear();
        } else {
            user.mailboxes.remove(index);
        }
        Ok(())
    }

    pub(crate) fn rename_mailbox(
        &mut self,
        email: &str,
        from: &str,
        to: &str,
    ) -> Result<(), StoreError> {
        if from.eq_ignore_ascii_case("INBOX") || to.eq_ignore_ascii_case("INBOX") {
            return Err(StoreError::CannotDeleteInbox);
        }
        let to = to.trim_end_matches(DELIMITER);
        let user = self.user(email).ok_or(StoreError::NoSuchUser)?;
        if user.mailbox(from).is_none() {
            return Err(StoreError::NoSuchMailbox);
        }
        if user.mailbox(to).is_some() {
            return Err(StoreError::AlreadyExists);
        }
        if to.is_empty() || to.contains(['*', '%']) || to.split(DELIMITER).any(str::is_empty) {
            return Err(StoreError::InvalidName);
        }
        let child_prefix = format!("{from}{DELIMITER}");
        let user = self.user_mut(email).ok_or(StoreError::NoSuchUser)?;
        for mailbox in &mut user.mailboxes {
            if mailbox.name == from {
                mailbox.name = to.to_string();
            } else if let Some(rest) = mailbox.name.strip_prefix(&child_prefix) {
                mailbox.name = format!("{to}{DELIMITER}{rest}");
            }
        }
        // Create missing parents of the new name.
        if let Some((parent, _)) = to.rsplit_once(DELIMITER) {
            let parent = parent.to_string();
            match self.create_mailbox(email, &parent) {
                Ok(()) | Err(StoreError::AlreadyExists) => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    /// Gmail labels of a message (by X-GM-MSGID) as reported by X-GM-LABELS.
    pub(crate) fn gm_labels(&self, email: &str, msgid: u64, flags: &[String]) -> Vec<String> {
        let mut labels: Vec<String> = Vec::new();
        if let Some(user) = self.user(email) {
            for mailbox in &user.mailboxes {
                if !mailbox.messages.iter().any(|m| m.data.gm_msgid == msgid) {
                    continue;
                }
                let label = match mailbox.special_use {
                    Some(SpecialUse::All | SpecialUse::Archive) => continue,
                    Some(SpecialUse::Sent) => "\\Sent".to_string(),
                    Some(SpecialUse::Drafts) => "\\Draft".to_string(),
                    Some(SpecialUse::Trash) => "\\Trash".to_string(),
                    Some(SpecialUse::Junk) => "\\Spam".to_string(),
                    None if mailbox.name == "INBOX" => "\\Inbox".to_string(),
                    None => mailbox.name.clone(),
                };
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
        }
        if has_flag(flags, "\\Flagged") {
            labels.push("\\Starred".to_string());
        }
        labels
    }
}

/// Canonical form of a flag, or `None` for flags that cannot be stored (`\Recent`,
/// unknown system flags, invalid keywords).
pub(crate) fn normalize_flag(flag: &str) -> Option<String> {
    let flag = flag.trim();
    if let Some(name) = flag.strip_prefix('\\') {
        return SYSTEM_FLAGS
            .iter()
            .find(|s| s[1..].eq_ignore_ascii_case(name))
            .map(|s| s.to_string());
    }
    let valid = !flag.is_empty()
        && flag
            .bytes()
            .all(|b| b > 0x20 && b < 0x7f && !b"(){%*\"\\]".contains(&b));
    valid.then(|| flag.to_string())
}

fn flag_rank(flag: &str) -> usize {
    SYSTEM_FLAGS
        .iter()
        .position(|s| *s == flag)
        .unwrap_or(SYSTEM_FLAGS.len())
}

/// Sorts flags (system flags first) and removes case-insensitive duplicates.
pub(crate) fn sort_flags(flags: &mut Vec<String>) {
    flags.sort_by(|a, b| {
        flag_rank(a)
            .cmp(&flag_rank(b))
            .then_with(|| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()))
    });
    flags.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
}

pub(crate) fn normalize_flags<'a>(flags: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = flags.into_iter().filter_map(normalize_flag).collect();
    sort_flags(&mut out);
    out
}

pub(crate) fn has_flag(flags: &[String], flag: &str) -> bool {
    flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_normalized() {
        assert_eq!(
            normalize_flags([
                "$Forwarded",
                "\\seen",
                "\\Recent",
                "\\FLAGGED",
                "\\Seen",
                "bad flag",
                "\\Bogus"
            ]),
            vec!["\\Flagged", "\\Seen", "$Forwarded"]
        );
    }

    #[test]
    fn append_copy_remove_and_threads() {
        let mut store = Store::new(&[("A@x.test".into(), "pw".into())], false);
        let (_, uid1) = store
            .append(
                "a@x.test",
                "inbox",
                b"Message-ID: <1@x>\r\n\r\nhi".to_vec(),
                &[],
                None,
                true,
            )
            .unwrap();
        let (_, uid2) = store
            .append(
                "a@x.test",
                "INBOX",
                b"In-Reply-To: <1@x>\r\n\r\nre".to_vec(),
                &[],
                None,
                true,
            )
            .unwrap();
        assert_eq!((uid1, uid2), (1, 2));
        let inbox = store.mailbox("a@x.test", "INBOX").unwrap();
        assert_eq!(
            inbox.messages[0].data.gm_thrid,
            inbox.messages[1].data.gm_thrid
        );
        let (_, pairs) = store.copy("a@x.test", "INBOX", &[2, 7], "Archive").unwrap();
        assert_eq!(pairs, vec![(2, 1)]);
        assert_eq!(store.remove_uids("a@x.test", "INBOX", &[1]), 1);
        assert_eq!(store.snapshot("a@x.test", "INBOX").len(), 1);
        assert_eq!(
            store.append("a@x.test", "Nope", Vec::new(), &[], None, true),
            Err(StoreError::NoSuchMailbox)
        );
    }

    #[test]
    fn mailbox_management() {
        let mut store = Store::new(&[("a@x.test".into(), "pw".into())], true);
        store.create_mailbox("a@x.test", "Work/Projects/").unwrap();
        assert!(store.mailbox("a@x.test", "Work").is_some());
        assert_eq!(
            store.create_mailbox("a@x.test", "Work"),
            Err(StoreError::AlreadyExists)
        );
        store.rename_mailbox("a@x.test", "Work", "Lavoro").unwrap();
        assert!(store.mailbox("a@x.test", "Lavoro/Projects").is_some());
        store.delete_mailbox("a@x.test", "Lavoro/Projects").unwrap();
        assert!(store.mailbox("a@x.test", "Lavoro/Projects").is_none());
        assert_eq!(
            store.delete_mailbox("a@x.test", "inbox"),
            Err(StoreError::CannotDeleteInbox)
        );
        assert!(!store.mailbox("a@x.test", "[Gmail]").unwrap().selectable);
    }
}

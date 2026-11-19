//! Domain types shared by storage, sync engine and the IPC protocol.

use serde::{Deserialize, Serialize};

pub type AccountId = i64;
pub type FolderId = i64;
pub type MessageId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Gmail,
    Outlook,
    Imap,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Gmail => "gmail",
            Provider::Outlook => "outlook",
            Provider::Imap => "imap",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gmail" => Some(Provider::Gmail),
            "outlook" => Some(Provider::Outlook),
            "imap" => Some(Provider::Imap),
            _ => None,
        }
    }
    /// Gmail and Outlook store a copy of messages sent via SMTP in the Sent
    /// folder server-side; generic IMAP servers usually don't.
    pub fn server_saves_sent(self) -> bool {
        !matches!(self, Provider::Imap)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    Tls,
    Starttls,
    None,
}

impl Security {
    pub fn as_str(self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::Starttls => "starttls",
            Security::None => "none",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tls" => Some(Security::Tls),
            "starttls" => Some(Security::Starttls),
            "none" => Some(Security::None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    OAuth2,
    Password,
}

impl AuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthKind::OAuth2 => "oauth2",
            AuthKind::Password => "password",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "oauth2" => Some(AuthKind::OAuth2),
            "password" => Some(AuthKind::Password),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NotifyFilter {
    #[default]
    All,
    Primary,
}

impl NotifyFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyFilter::All => "all",
            NotifyFilter::Primary => "primary",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "primary" {
            NotifyFilter::Primary
        } else {
            NotifyFilter::All
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

/// Persistent account configuration (secrets live in the OS credential store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountConfig {
    pub id: AccountId,
    pub provider: Provider,
    pub email: String,
    pub display_name: String,
    pub username: String,
    pub auth: AuthKind,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub notify: bool,
    pub notify_filter: NotifyFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountState {
    Connecting,
    Online,
    Offline,
    Error,
    AuthRequired,
}

/// Account as exposed over IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: AccountId,
    pub provider: Provider,
    pub email: String,
    pub display_name: String,
    pub state: AccountState,
    pub error: Option<String>,
    pub last_sync: Option<i64>,
    pub unread: u32,
    pub notify: bool,
    pub notify_filter: NotifyFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Archive,
    Spam,
    All,
    Flagged,
    Important,
}

impl FolderRole {
    pub fn as_str(self) -> &'static str {
        match self {
            FolderRole::Inbox => "inbox",
            FolderRole::Sent => "sent",
            FolderRole::Drafts => "drafts",
            FolderRole::Trash => "trash",
            FolderRole::Archive => "archive",
            FolderRole::Spam => "spam",
            FolderRole::All => "all",
            FolderRole::Flagged => "flagged",
            FolderRole::Important => "important",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "inbox" => FolderRole::Inbox,
            "sent" => FolderRole::Sent,
            "drafts" => FolderRole::Drafts,
            "trash" => FolderRole::Trash,
            "archive" => FolderRole::Archive,
            "spam" => FolderRole::Spam,
            "all" => FolderRole::All,
            "flagged" => FolderRole::Flagged,
            "important" => FolderRole::Important,
            _ => return None,
        })
    }
    /// Sort weight for sidebar ordering.
    pub fn order(role: Option<Self>) -> u8 {
        match role {
            Some(FolderRole::Inbox) => 0,
            Some(FolderRole::Flagged) => 1,
            Some(FolderRole::Important) => 2,
            Some(FolderRole::Drafts) => 3,
            Some(FolderRole::Sent) => 4,
            Some(FolderRole::Archive) => 5,
            Some(FolderRole::All) => 6,
            Some(FolderRole::Spam) => 7,
            Some(FolderRole::Trash) => 8,
            None => 9,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    pub name: String,
    pub display_name: String,
    pub role: Option<FolderRole>,
    pub parent_id: Option<FolderId>,
    pub depth: u32,
    pub unread: u32,
    pub total: u32,
    pub selectable: bool,
    /// New mail in this folder is pushed and notified (always true for INBOX).
    pub monitored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Addr {
    pub name: Option<String>,
    pub address: String,
}

impl Addr {
    pub fn new(name: Option<&str>, address: &str) -> Self {
        Addr {
            name: name
                .map(|n| n.trim().trim_matches('"').trim().to_string())
                .filter(|n| !n.is_empty()),
            address: address.trim().to_string(),
        }
    }
    /// Human-friendly label: name if present, else the address.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.address)
    }
}

/// Message flags as a compact bitmask.
pub mod flags {
    pub const SEEN: i64 = 1;
    pub const FLAGGED: i64 = 2;
    pub const ANSWERED: i64 = 4;
    pub const DRAFT: i64 = 8;
    pub const DELETED: i64 = 16;

    pub fn from_imap<'a>(names: impl IntoIterator<Item = &'a str>) -> i64 {
        let mut f = 0;
        for n in names {
            match n.to_ascii_lowercase().as_str() {
                "\\seen" => f |= SEEN,
                "\\flagged" => f |= FLAGGED,
                "\\answered" => f |= ANSWERED,
                "\\draft" => f |= DRAFT,
                "\\deleted" => f |= DELETED,
                _ => {}
            }
        }
        f
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub subject: String,
    pub from: Option<Addr>,
    pub to: Vec<Addr>,
    pub date: i64,
    pub snippet: String,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub has_attachments: bool,
    pub size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub part_id: u32,
    pub filename: String,
    pub mime: String,
    pub size: u64,
    pub inline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDetail {
    #[serde(flatten)]
    pub summary: MessageSummary,
    pub cc: Vec<Addr>,
    pub bcc: Vec<Addr>,
    pub reply_to: Vec<Addr>,
    pub message_id_header: Option<String>,
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub has_remote_content: bool,
    pub remote_allowed: bool,
}

/// Unix time in milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

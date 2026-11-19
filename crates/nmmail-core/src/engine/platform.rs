//! Hooks implemented by each OS layer of the daemon.

use anyhow::Result;

use crate::model::{AccountId, MessageId};

/// One message in a new-mail notification.
#[derive(Debug, Clone)]
pub struct NotifyItem {
    pub message_id: MessageId,
    pub from_name: String,
    pub from_address: String,
    pub subject: String,
    pub snippet: String,
}

/// A new-mail notification request.
#[derive(Debug, Clone)]
pub struct Notification {
    pub account_id: AccountId,
    pub account_email: String,
    pub account_name: String,
    /// Messages to show individually (newest first).
    pub items: Vec<NotifyItem>,
    /// Total number of new messages (may exceed `items.len()` → summary toast).
    pub total: usize,
    pub sound: bool,
    pub show_preview: bool,
    /// UI language setting: "system", "it" or "en".
    pub language: String,
}

pub trait Platform: Send + Sync + 'static {
    /// Opens a URL in the default browser.
    fn open_url(&self, url: &str);
    /// Shows a new-mail notification.
    fn notify(&self, n: &Notification);
    /// Removes notifications of messages that were read/deleted elsewhere.
    fn retract(&self, account_id: AccountId, ids: &[MessageId]);
    /// Total unread count across inboxes (tray badge / tooltip).
    fn unread_changed(&self, total: u32, problem: Option<&str>);
    /// Enables/disables start at login. `user_initiated` is true when the user
    /// just changed the setting (as opposed to the startup consistency check).
    fn set_autostart(&self, enabled: bool, user_initiated: bool) -> Result<()>;
    /// Brings the UI to the front (launching it if needed), optionally opening a message.
    /// `ui_connected` tells whether a UI client is currently attached to the daemon.
    fn show_ui(&self, message_id: Option<MessageId>, ui_connected: bool);
    /// Whether the OS lets us show notifications (diagnostics / test button).
    fn notification_status(&self) -> String {
        "unsupported".into()
    }
    /// Process memory (private bytes) for diagnostics.
    fn memory_bytes(&self) -> u64 {
        0
    }
}

/// A platform that does nothing (tests, headless runs).
pub struct NullPlatform;

impl Platform for NullPlatform {
    fn open_url(&self, _url: &str) {}
    fn notify(&self, _n: &Notification) {}
    fn retract(&self, _a: AccountId, _ids: &[MessageId]) {}
    fn unread_changed(&self, _total: u32, _problem: Option<&str>) {}
    fn set_autostart(&self, _enabled: bool, _user: bool) -> Result<()> {
        Ok(())
    }
    fn show_ui(&self, _m: Option<MessageId>, _c: bool) {}
}

//! Events pushed from the engine to IPC clients.

use serde::Serialize;

use crate::model::{Account, AccountId, FolderId, MessageId};
use crate::settings::Settings;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum Event {
    #[serde(rename = "account.status")]
    AccountStatus(Account),
    #[serde(rename = "accounts.changed")]
    AccountsChanged {},
    #[serde(rename = "folders.changed", rename_all = "camelCase")]
    FoldersChanged { account_id: AccountId },
    #[serde(rename = "messages.changed", rename_all = "camelCase")]
    MessagesChanged {
        account_id: AccountId,
        folder_ids: Vec<FolderId>,
    },
    #[serde(rename = "mail.new", rename_all = "camelCase")]
    MailNew {
        account_id: AccountId,
        message_ids: Vec<MessageId>,
    },
    #[serde(rename = "outbox.sent", rename_all = "camelCase")]
    OutboxSent { outbox_id: i64 },
    #[serde(rename = "outbox.failed", rename_all = "camelCase")]
    OutboxFailed { outbox_id: i64, error: String },
    #[serde(rename = "auth.progress")]
    AuthProgress {
        stage: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    #[serde(rename = "ui.show", rename_all = "camelCase")]
    UiShow { message_id: Option<MessageId> },
    #[serde(rename = "settings.changed")]
    SettingsChanged(Settings),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format() {
        let e = Event::MailNew {
            account_id: 1,
            message_ids: vec![42],
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"event":"mail.new","data":{"accountId":1,"messageIds":[42]}}"#
        );
        let e = Event::AccountsChanged {};
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"event":"accounts.changed","data":{}}"#
        );
    }
}

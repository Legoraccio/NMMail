//! Windows toast notifications (WinRT) for an unpackaged app.
//!
//! The AppUserModelID is registered under HKCU\Software\Classes\AppUserModelId,
//! clicks and buttons use protocol activation (`nmmail:` URIs).

use nmmail_core::Notification;
use nmmail_core::model::{AccountId, MessageId};
use tracing::warn;
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{
    NotificationSetting, ToastNotification, ToastNotificationManager,
};
use windows::core::HSTRING;

use super::registry::AUMID;
use crate::i18n::{Lang, S, t};

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn audio(sound: bool) -> &'static str {
    if sound {
        "<audio src=\"ms-winsoundevent:Notification.Mail\"/>"
    } else {
        "<audio silent=\"true\"/>"
    }
}

/// "account · folder" when the mail was moved out of the INBOX by a rule/filter.
fn attribution(n: &Notification) -> String {
    match n.folder.as_deref() {
        Some(f) if !f.trim().is_empty() => format!("{} · {}", n.account_email, clip(f, 40)),
        _ => n.account_email.clone(),
    }
}

/// Toast XML for one message.
pub fn message_xml(n: &Notification, item: &nmmail_core::NotifyItem, lang: Lang) -> String {
    let id = item.message_id;
    let (title, line1, line2) = if n.show_preview {
        let subject = if item.subject.trim().is_empty() {
            t(lang, S::NoSubject).to_string()
        } else {
            item.subject.clone()
        };
        (
            clip(&item.from_name, 80),
            clip(&subject, 120),
            clip(&item.snippet, 200),
        )
    } else {
        (
            t(lang, S::NewMail).to_string(),
            String::new(),
            String::new(),
        )
    };
    let mut texts = format!("<text hint-maxLines=\"1\">{}</text>", esc(&title));
    if !line1.is_empty() {
        texts.push_str(&format!("<text hint-maxLines=\"1\">{}</text>", esc(&line1)));
    }
    if !line2.is_empty() {
        texts.push_str(&format!("<text>{}</text>", esc(&line2)));
    }
    format!(
        "<toast launch=\"nmmail://open?msg={id}\" activationType=\"protocol\">\
<visual><binding template=\"ToastGeneric\">{texts}<text placement=\"attribution\">{acct}</text></binding></visual>\
<actions>\
<action content=\"{read}\" activationType=\"protocol\" arguments=\"nmmail://markread?msg={id}\"/>\
<action content=\"{arch}\" activationType=\"protocol\" arguments=\"nmmail://archive?msg={id}\"/>\
<action content=\"{del}\" activationType=\"protocol\" arguments=\"nmmail://delete?msg={id}\"/>\
</actions>{audio}</toast>",
        acct = esc(&attribution(n)),
        read = esc(t(lang, S::MarkRead)),
        arch = esc(t(lang, S::Archive)),
        del = esc(t(lang, S::Delete)),
        audio = audio(n.sound),
    )
}

/// Toast XML summarizing many messages.
pub fn summary_xml(n: &Notification, lang: Lang) -> String {
    let title = format!("{} {}", n.total, t(lang, S::NewMails));
    let mut texts = format!("<text>{}</text>", esc(&title));
    if n.show_preview {
        let lines: Vec<String> = n
            .items
            .iter()
            .take(3)
            .map(|i| format!("{}: {}", clip(&i.from_name, 40), clip(&i.subject, 80)))
            .collect();
        texts.push_str(&format!("<text>{}</text>", esc(&lines.join("\n"))));
    }
    format!(
        "<toast launch=\"nmmail://open\" activationType=\"protocol\">\
<visual><binding template=\"ToastGeneric\">{texts}<text placement=\"attribution\">{acct}</text></binding></visual>{audio}</toast>",
        acct = esc(&attribution(n)),
        audio = audio(n.sound),
    )
}

fn show_xml(xml: &str, tag: &str, group: &str) -> windows::core::Result<()> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    toast.SetTag(&HSTRING::from(tag))?;
    toast.SetGroup(&HSTRING::from(group))?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?;
    notifier.Show(&toast)
}

pub fn show(n: &Notification, lang: Lang) {
    let group = format!("acct{}", n.account_id);
    let result = if n.total <= 3 {
        // Oldest first so the newest ends on top.
        n.items.iter().rev().try_for_each(|item| {
            show_xml(
                &message_xml(n, item, lang),
                &format!("m{}", item.message_id),
                &group,
            )
        })
    } else {
        show_xml(&summary_xml(n, lang), "summary", &group)
    };
    if let Err(e) = result {
        warn!("toast failed: {e}");
    }
}

/// Notification permission for our AUMID plus how many toasts are in Action Center.
pub fn status() -> String {
    let notifier = match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))
    {
        Ok(n) => n,
        Err(e) => return format!("error: {e}"),
    };
    let setting = match notifier.Setting() {
        Ok(NotificationSetting::Enabled) => "enabled",
        Ok(NotificationSetting::DisabledForApplication) => "disabledForApp",
        Ok(NotificationSetting::DisabledForUser) => "disabledForUser",
        Ok(NotificationSetting::DisabledByGroupPolicy) => "disabledByPolicy",
        Ok(NotificationSetting::DisabledByManifest) => "disabledByManifest",
        Ok(_) => "unknown",
        Err(e) => return format!("error: {e}"),
    };
    let shown = ToastNotificationManager::History()
        .and_then(|h| h.GetHistoryWithId(&HSTRING::from(AUMID)))
        .and_then(|v| v.Size())
        .unwrap_or(0);
    format!("{setting}; actionCenter={shown}")
}

pub fn retract(account_id: AccountId, ids: &[MessageId]) {
    let Ok(history) = ToastNotificationManager::History() else {
        return;
    };
    let group = HSTRING::from(format!("acct{account_id}"));
    let app = HSTRING::from(AUMID);
    for id in ids {
        let _ = history.RemoveGroupedTagWithId(&HSTRING::from(format!("m{id}")), &group, &app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nmmail_core::NotifyItem;

    #[test]
    fn xml_is_escaped_and_parseable() {
        let n = Notification {
            account_id: 1,
            account_email: "me@x.it".into(),
            account_name: "Me".into(),
            items: vec![NotifyItem {
                message_id: 42,
                from_name: "Tom & \"Jerry\" <tj>".into(),
                from_address: "tj@x.it".into(),
                subject: "Ciao <b>".into(),
                snippet: "a\u{1}b".into(),
            }],
            total: 1,
            sound: true,
            show_preview: true,
            language: "it".into(),
            folder: Some("Lavoro".into()),
        };
        let xml = message_xml(&n, &n.items[0], Lang::It);
        assert!(xml.contains("Tom &amp; &quot;Jerry&quot; &lt;tj&gt;"));
        assert!(xml.contains("nmmail://markread?msg=42"));
        assert!(
            xml.contains("me@x.it · Lavoro"),
            "folder shown in the attribution"
        );
        let doc = XmlDocument::new().unwrap();
        doc.LoadXml(&HSTRING::from(xml)).unwrap();
        let doc = XmlDocument::new().unwrap();
        doc.LoadXml(&HSTRING::from(summary_xml(&n, Lang::En)))
            .unwrap();
    }
}

//! Building outgoing messages and reply/forward skeletons.

use std::path::PathBuf;

use anyhow::{Context, Result};
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use rand::RngExt as _;
use serde::{Deserialize, Serialize};

use crate::mime::Parsed;
use crate::model::{AccountConfig, Addr, MessageId};
use crate::store::MessageRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Reply,
    ReplyAll,
    Forward,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardAttachment {
    pub message_id: MessageId,
    pub part_id: u32,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub size: u64,
}

/// Pre-filled draft returned by `compose.prepare`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub account_id: i64,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub subject: String,
    pub html: String,
    pub text: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub attachments: Vec<ForwardAttachment>,
}

/// Parameters of `compose.send`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    pub account_id: i64,
    #[serde(default)]
    pub to: Vec<Addr>,
    #[serde(default)]
    pub cc: Vec<Addr>,
    #[serde(default)]
    pub bcc: Vec<Addr>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub html: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<PathBuf>,
    #[serde(default)]
    pub forward_attachments: Vec<ForwardAttachment>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub reply_to_message_id: Option<MessageId>,
}

fn strip_prefixes(subject: &str) -> &str {
    let mut s = subject.trim();
    loop {
        let lower = s.to_lowercase();
        let mut stripped = false;
        for p in ["re:", "r:", "fw:", "fwd:", "i:", "inoltro:", "aw:", "wg:", "rif:"] {
            if lower.starts_with(p) {
                s = s[p.len()..].trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            return s;
        }
    }
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn fmt_date(ms: i64, lang: &str) -> String {
    use chrono::TimeZone;
    let dt = chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(chrono::Local::now);
    if lang == "en" {
        dt.format("%a, %b %-d, %Y at %H:%M").to_string()
    } else {
        const DAYS: [&str; 7] = ["lun", "mar", "mer", "gio", "ven", "sab", "dom"];
        const MONTHS: [&str; 12] = [
            "gen", "feb", "mar", "apr", "mag", "giu", "lug", "ago", "set", "ott", "nov", "dic",
        ];
        use chrono::Datelike;
        format!(
            "{} {} {} {} alle {}",
            DAYS[dt.weekday().num_days_from_monday() as usize],
            dt.day(),
            MONTHS[dt.month0() as usize],
            dt.year(),
            dt.format("%H:%M")
        )
    }
}

fn esc(s: &str) -> String {
    html_escape::encode_text(s).into_owned()
}

fn addr_html(a: &Addr) -> String {
    match &a.name {
        Some(n) => format!("{} &lt;{}&gt;", esc(n), esc(&a.address)),
        None => esc(&a.address),
    }
}

fn addr_list(v: &[Addr]) -> String {
    v.iter().map(addr_html).collect::<Vec<_>>().join(", ")
}

/// Builds the reply / forward skeleton for a message.
pub fn prepare(
    mode: Mode,
    me: &AccountConfig,
    orig: &MessageRow,
    parsed: &Parsed,
    lang: &str,
) -> Prepared {
    let en = lang == "en";
    let from = orig.summary.from.clone();
    let reply_target: Vec<Addr> = if !orig.reply_to.is_empty() {
        orig.reply_to.clone()
    } else {
        from.clone().into_iter().collect()
    };
    let sent_by_me = from.as_ref().is_some_and(|f| same(&f.address, &me.email));

    let (to, cc) = match mode {
        Mode::Forward => (Vec::new(), Vec::new()),
        Mode::Reply => {
            if sent_by_me {
                (orig.summary.to.clone(), Vec::new())
            } else {
                (reply_target.clone(), Vec::new())
            }
        }
        Mode::ReplyAll => {
            let mut to = if sent_by_me {
                orig.summary.to.clone()
            } else {
                reply_target.clone()
            };
            let mut cc: Vec<Addr> = Vec::new();
            for a in orig.summary.to.iter().chain(orig.cc.iter()) {
                if same(&a.address, &me.email)
                    || to.iter().any(|t| same(&t.address, &a.address))
                    || cc.iter().any(|t| same(&t.address, &a.address))
                {
                    continue;
                }
                cc.push(a.clone());
            }
            if to.is_empty() {
                to = std::mem::take(&mut cc);
            }
            (to, cc)
        }
    };

    let base = strip_prefixes(&orig.summary.subject);
    let subject = match mode {
        Mode::Forward => format!("{} {base}", if en { "Fwd:" } else { "I:" }),
        _ => format!("Re: {base}"),
    };

    let date = fmt_date(orig.summary.date, lang);
    let from_label = from.as_ref().map(addr_html).unwrap_or_default();
    let quoted = crate::html::quotable_fragment(parsed);
    let body_text = parsed.text.clone().unwrap_or_default();
    let (html, text) = match mode {
        Mode::Forward => {
            let (h_title, h_from, h_date, h_subj, h_to, h_cc) = if en {
                ("Forwarded message", "From", "Date", "Subject", "To", "Cc")
            } else {
                ("Messaggio inoltrato", "Da", "Data", "Oggetto", "A", "Cc")
            };
            let mut header = format!(
                "<div class=\"nm-fwd\"><p>---------- {h_title} ----------<br>\
                 <b>{h_from}:</b> {from_label}<br><b>{h_date}:</b> {}<br><b>{h_subj}:</b> {}<br><b>{h_to}:</b> {}",
                esc(&date),
                esc(&orig.summary.subject),
                addr_list(&orig.summary.to)
            );
            if !orig.cc.is_empty() {
                header.push_str(&format!("<br><b>{h_cc}:</b> {}", addr_list(&orig.cc)));
            }
            header.push_str("</p>");
            let html = format!("<p><br></p>{header}{quoted}</div>");
            let text = format!(
                "\n\n---------- {h_title} ----------\n{h_from}: {}\n{h_date}: {date}\n{h_subj}: {}\n\n{body_text}",
                from.as_ref().map(|a| a.address.clone()).unwrap_or_default(),
                orig.summary.subject
            );
            (html, text)
        }
        _ => {
            let who = from
                .as_ref()
                .map(|a| a.label().to_string())
                .unwrap_or_default();
            let intro = if en {
                format!("On {date}, {who} wrote:")
            } else {
                format!("Il giorno {date} {who} ha scritto:")
            };
            let html = format!(
                "<p><br></p><div class=\"nm-quote\"><p>{}</p><blockquote type=\"cite\">{quoted}</blockquote></div>",
                esc(&intro)
            );
            let quoted_text: String = body_text
                .lines()
                .map(|l| format!("> {l}\n"))
                .collect();
            (html, format!("\n\n{intro}\n{quoted_text}"))
        }
    };

    // Threading headers.
    let (in_reply_to, references) = match mode {
        Mode::Forward => (None, None),
        _ => {
            let mid = orig.message_id.as_ref().map(|m| format!("<{}>", m.trim_matches(['<', '>'])));
            let mut refs = orig.references.clone().unwrap_or_default();
            if refs.is_empty()
                && let Some(irt) = &orig.in_reply_to
            {
                refs = format!("<{}>", irt.trim_matches(['<', '>']));
            }
            if let Some(m) = &mid {
                if !refs.is_empty() {
                    refs.push(' ');
                }
                refs.push_str(m);
            }
            (mid, Some(refs).filter(|r| !r.is_empty()))
        }
    };

    let attachments = if mode == Mode::Forward {
        parsed
            .attachments
            .iter()
            .filter(|a| !a.inline)
            .map(|a| ForwardAttachment {
                message_id: orig.summary.id,
                part_id: a.part_id,
                filename: a.filename.clone(),
                size: a.size,
            })
            .collect()
    } else {
        Vec::new()
    };

    Prepared {
        account_id: orig.summary.account_id,
        to,
        cc,
        subject,
        html,
        text,
        in_reply_to,
        references,
        attachments,
    }
}

fn mb_addr(a: &Addr) -> Address<'static> {
    Address::new_address(a.name.clone(), a.address.clone())
}

fn mb_list(v: &[Addr]) -> Address<'static> {
    Address::new_list(v.iter().map(mb_addr).collect())
}

fn msg_ids(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|t| t.trim_matches(['<', '>', ',']).to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Generates a Message-ID that does not leak the local host name.
pub fn new_message_id(sender: &str) -> String {
    let domain = sender.rsplit('@').next().filter(|d| !d.is_empty()).unwrap_or("nmmail.local");
    let mut rng = rand::rng();
    let a: u64 = rng.random();
    let b: u32 = rng.random();
    format!("{:016x}.{:08x}@{}", a, b, domain)
}

/// Extra attachment data resolved by the caller: (filename, mime, bytes).
pub type AttachmentData = (String, String, Vec<u8>);

/// Serializes an outgoing message. Bcc recipients are not written in headers.
pub fn build(
    from: &Addr,
    req: &SendRequest,
    extra: Vec<AttachmentData>,
) -> Result<(Vec<u8>, String)> {
    let message_id = new_message_id(&from.address);
    let mut b = MessageBuilder::new()
        .from(mb_addr(from))
        .subject(req.subject.clone())
        .message_id(message_id.clone())
        .header("X-Mailer", mail_builder::headers::text::Text::new("NM Mail"));
    if !req.to.is_empty() {
        b = b.to(mb_list(&req.to));
    }
    if !req.cc.is_empty() {
        b = b.cc(mb_list(&req.cc));
    }
    if let Some(irt) = &req.in_reply_to {
        let ids = msg_ids(irt);
        if !ids.is_empty() {
            b = b.in_reply_to(ids);
        }
    }
    if let Some(r) = &req.references {
        let ids = msg_ids(r);
        if !ids.is_empty() {
            b = b.references(ids);
        }
    }
    let text = if req.text.trim().is_empty() && !req.html.is_empty() {
        mail_parser::html_to_text(&req.html)
    } else {
        req.text.clone()
    };
    b = b.text_body(text);
    if !req.html.trim().is_empty() {
        b = b.html_body(format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>{}</body></html>",
            req.html
        ));
    }
    for path in &req.attachments {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "allegato".into());
        let mime = mime_guess::from_path(path).first_or_octet_stream().to_string();
        b = b.attachment(mime, name, bytes);
    }
    for (name, mime, bytes) in extra {
        b = b.attachment(mime, name, bytes);
    }
    Ok((b.write_to_vec()?, message_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn me() -> AccountConfig {
        AccountConfig {
            id: 1,
            provider: Provider::Imap,
            email: "me@example.com".into(),
            display_name: "Me".into(),
            username: "me@example.com".into(),
            auth: AuthKind::Password,
            imap: ServerConfig {
                host: "h".into(),
                port: 1,
                security: Security::None,
            },
            smtp: ServerConfig {
                host: "h".into(),
                port: 1,
                security: Security::None,
            },
            notify: true,
            notify_filter: NotifyFilter::All,
        }
    }

    fn orig() -> MessageRow {
        MessageRow {
            summary: MessageSummary {
                id: 7,
                account_id: 1,
                folder_id: 1,
                subject: "RE: Re: Preventivo".into(),
                from: Some(Addr::new(Some("Anna"), "anna@example.com")),
                to: vec![Addr::new(None, "me@example.com"), Addr::new(None, "bob@example.com")],
                date: 1_700_000_000_000,
                snippet: String::new(),
                seen: true,
                flagged: false,
                answered: false,
                has_attachments: false,
                size: 0,
            },
            uid: 3,
            flags: 0,
            folder_name: "INBOX".into(),
            folder_role: Some(FolderRole::Inbox),
            message_id: Some("m1@example.com".into()),
            in_reply_to: None,
            references: Some("<m0@example.com>".into()),
            cc: vec![Addr::new(None, "carl@example.com")],
            reply_to: vec![],
        }
    }

    #[test]
    fn reply_all_recipients_and_threading() {
        let parsed = Parsed {
            text: Some("Ciao\nsaluti".into()),
            ..Default::default()
        };
        let p = prepare(Mode::ReplyAll, &me(), &orig(), &parsed, "it");
        assert_eq!(p.subject, "Re: Preventivo");
        assert_eq!(p.to[0].address, "anna@example.com");
        let cc: Vec<_> = p.cc.iter().map(|a| a.address.as_str()).collect();
        assert_eq!(cc, ["bob@example.com", "carl@example.com"]);
        assert_eq!(p.in_reply_to.as_deref(), Some("<m1@example.com>"));
        assert_eq!(p.references.as_deref(), Some("<m0@example.com> <m1@example.com>"));
        assert!(p.text.contains("> Ciao"));
        assert!(p.html.contains("ha scritto"));

        let f = prepare(Mode::Forward, &me(), &orig(), &parsed, "en");
        assert_eq!(f.subject, "Fwd: Preventivo");
        assert!(f.to.is_empty());
        assert!(f.html.contains("Forwarded message"));
    }

    #[test]
    fn build_message_without_bcc_header() {
        let req = SendRequest {
            account_id: 1,
            to: vec![Addr::new(Some("Anna"), "anna@example.com")],
            cc: vec![],
            bcc: vec![Addr::new(None, "secret@example.com")],
            subject: "Ciao è".into(),
            html: "<p>Ciao <b>Anna</b></p>".into(),
            text: String::new(),
            attachments: vec![],
            forward_attachments: vec![],
            in_reply_to: Some("<m1@example.com>".into()),
            references: Some("<m0@example.com> <m1@example.com>".into()),
            reply_to_message_id: None,
        };
        let (raw, mid) = build(
            &Addr::new(Some("Me"), "me@example.com"),
            &req,
            vec![("a.txt".into(), "text/plain".into(), b"hello".to_vec())],
        )
        .unwrap();
        let s = String::from_utf8_lossy(&raw);
        assert!(!s.to_lowercase().contains("secret@example.com"));
        assert!(mid.ends_with("@example.com"));
        let parsed = crate::mime::parse_full(&raw);
        assert_eq!(parsed.headers.subject, "Ciao è");
        assert_eq!(parsed.headers.in_reply_to.as_deref(), Some("m1@example.com"));
        assert!(parsed.text.unwrap().contains("Ciao Anna"));
        assert_eq!(parsed.attachments.len(), 1);
    }
}

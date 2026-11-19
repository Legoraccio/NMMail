//! Minimal string table for tray and notifications.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    It,
    En,
}

impl Lang {
    /// Resolves the "system" | "it" | "en" setting.
    pub fn resolve(setting: &str) -> Lang {
        match setting {
            "it" => Lang::It,
            "en" => Lang::En,
            _ => crate::platform::system_lang(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum S {
    Open,
    CheckNow,
    Pause,
    Pause1h,
    PauseTomorrow,
    Resume,
    Exit,
    MarkRead,
    Archive,
    Delete,
    NewMail,
    NewMails,
    Unread,
    NoUnread,
    Paused,
    NoSubject,
}

pub fn t(lang: Lang, s: S) -> &'static str {
    use Lang::*;
    use S::*;
    match (lang, s) {
        (It, Open) => "Apri NM Mail",
        (En, Open) => "Open NM Mail",
        (It, CheckNow) => "Controlla posta adesso",
        (En, CheckNow) => "Check mail now",
        (It, Pause) => "Pausa notifiche",
        (En, Pause) => "Pause notifications",
        (It, Pause1h) => "Per 1 ora",
        (En, Pause1h) => "For 1 hour",
        (It, PauseTomorrow) => "Fino a domani",
        (En, PauseTomorrow) => "Until tomorrow",
        (It, Resume) => "Riprendi notifiche",
        (En, Resume) => "Resume notifications",
        (It, Exit) => "Esci",
        (En, Exit) => "Exit",
        (It, MarkRead) => "Segna come letto",
        (En, MarkRead) => "Mark as read",
        (It, Archive) => "Archivia",
        (En, Archive) => "Archive",
        (It, Delete) => "Elimina",
        (En, Delete) => "Delete",
        (It, NewMail) => "Nuovo messaggio",
        (En, NewMail) => "New message",
        (It, NewMails) => "nuovi messaggi",
        (En, NewMails) => "new messages",
        (It, Unread) => "non lette",
        (En, Unread) => "unread",
        (It, NoUnread) => "Nessun messaggio non letto",
        (En, NoUnread) => "No unread messages",
        (It, Paused) => "Notifiche in pausa",
        (En, Paused) => "Notifications paused",
        (It, NoSubject) => "(nessun oggetto)",
        (En, NoSubject) => "(no subject)",
    }
}

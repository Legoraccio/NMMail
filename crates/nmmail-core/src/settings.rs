//! User settings (persisted as JSON in the `settings` table).

use serde::{Deserialize, Serialize};

/// OAuth clients registered once by the publisher of NM Mail and compiled into
/// the binaries (`scripts/build.ps1` reads them from `oauth.local.toml` or from
/// the environment). End users only sign in with their own account.
pub mod builtin_oauth {
    pub const GOOGLE_CLIENT_ID: &str = match option_env!("NMMAIL_GOOGLE_CLIENT_ID") {
        Some(v) => v,
        None => "",
    };
    /// Not confidential for installed apps (Google's own wording), but never
    /// committed: it only lives in the build environment.
    pub const GOOGLE_CLIENT_SECRET: &str = match option_env!("NMMAIL_GOOGLE_CLIENT_SECRET") {
        Some(v) => v,
        None => "",
    };
    pub const MICROSOFT_CLIENT_ID: &str = match option_env!("NMMAIL_MICROSOFT_CLIENT_ID") {
        Some(v) => v,
        None => "",
    };
}

/// Which providers have a built-in OAuth client (shown by the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OAuthBuiltIn {
    pub google: bool,
    pub microsoft: bool,
}

impl OAuthBuiltIn {
    pub fn current() -> Self {
        OAuthBuiltIn {
            google: !builtin_oauth::GOOGLE_CLIENT_ID.trim().is_empty(),
            microsoft: !builtin_oauth::MICROSOFT_CLIENT_ID.trim().is_empty(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub autostart: bool,
    pub notifications_enabled: bool,
    pub notification_sound: bool,
    pub show_preview: bool,
    pub cache_days: u32,
    pub theme: String,
    pub language: String,
    pub quiet_until: Option<i64>,
    /// Optional overrides of the built-in OAuth clients (Settings → OAuth),
    /// e.g. for builds made from source. Empty = use the built-in client.
    pub google_client_id: String,
    pub google_client_secret: String,
    pub microsoft_client_id: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            autostart: true,
            notifications_enabled: true,
            notification_sound: true,
            show_preview: true,
            cache_days: 90,
            theme: "system".into(),
            language: "system".into(),
            quiet_until: None,
            google_client_id: String::new(),
            google_client_secret: String::new(),
            microsoft_client_id: String::new(),
        }
    }
}

impl Settings {
    /// Applies a partial JSON object (as sent by `settings.set`) on top of `self`.
    pub fn merged(&self, patch: &serde_json::Value) -> Result<Settings, serde_json::Error> {
        let mut base = serde_json::to_value(self)?;
        if let (Some(base), Some(patch)) = (base.as_object_mut(), patch.as_object()) {
            for (k, v) in patch {
                if base.contains_key(k) {
                    base.insert(k.clone(), v.clone());
                }
            }
        }
        let mut s: Settings = serde_json::from_value(base)?;
        s.normalize();
        Ok(s)
    }

    fn normalize(&mut self) {
        if !matches!(self.theme.as_str(), "system" | "light" | "dark") {
            self.theme = "system".into();
        }
        if !matches!(self.language.as_str(), "system" | "it" | "en") {
            self.language = "system".into();
        }
        if self.cache_days != 0 {
            self.cache_days = self.cache_days.clamp(7, 3650);
        }
        self.google_client_id = self.google_client_id.trim().to_string();
        self.google_client_secret = self.google_client_secret.trim().to_string();
        self.microsoft_client_id = self.microsoft_client_id.trim().to_string();
    }

    /// Whether notifications are currently muted ("pause" feature).
    pub fn is_quiet(&self, now_ms: i64) -> bool {
        self.quiet_until.is_some_and(|t| t > now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_partial() {
        let s = Settings::default();
        let m = s
            .merged(&serde_json::json!({"cacheDays": 30, "theme": "bogus", "unknown": 1}))
            .unwrap();
        assert_eq!(m.cache_days, 30);
        assert_eq!(m.theme, "system");
        assert!(m.autostart);
    }

    #[test]
    fn built_in_credentials_are_never_persisted_as_user_settings() {
        // Overrides start empty whatever the build embeds, so a later build
        // with different credentials is not shadowed by stale copies.
        let s = Settings::default();
        assert!(s.google_client_id.is_empty());
        assert!(s.google_client_secret.is_empty());
        assert!(s.microsoft_client_id.is_empty());
        let json = serde_json::to_value(OAuthBuiltIn::current()).unwrap();
        assert!(json.get("google").is_some() && json.get("microsoft").is_some());
    }

    #[test]
    fn built_in_flags_follow_the_build_environment() {
        let set = |v: Option<&str>| v.is_some_and(|v| !v.trim().is_empty());
        let flags = OAuthBuiltIn::current();
        eprintln!("built-in OAuth: {flags:?}");
        assert_eq!(flags.google, set(option_env!("NMMAIL_GOOGLE_CLIENT_ID")));
        assert_eq!(
            flags.microsoft,
            set(option_env!("NMMAIL_MICROSOFT_CLIENT_ID"))
        );
    }
}

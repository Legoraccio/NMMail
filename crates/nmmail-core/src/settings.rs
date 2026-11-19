//! User settings (persisted as JSON in the `settings` table).

use serde::{Deserialize, Serialize};

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
            google_client_id: option_env!("NMMAIL_GOOGLE_CLIENT_ID")
                .unwrap_or_default()
                .into(),
            google_client_secret: option_env!("NMMAIL_GOOGLE_CLIENT_SECRET")
                .unwrap_or_default()
                .into(),
            microsoft_client_id: option_env!("NMMAIL_MICROSOFT_CLIENT_ID")
                .unwrap_or_default()
                .into(),
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
}

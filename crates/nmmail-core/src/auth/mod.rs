//! Credentials management: passwords and OAuth tokens per account.

pub mod oauth;
pub mod secrets;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Result;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::{ErrorCode, coded};
use crate::imap::conn::Credentials;
use crate::model::{AccountConfig, AccountId, AuthKind, Provider, now_ms};
use crate::settings::Settings;
use secrets::SecretStore;

fn password_key(id: AccountId) -> String {
    format!("account-{id}-password")
}

fn refresh_key(id: AccountId) -> String {
    format!("account-{id}-refresh")
}

/// Picks the OAuth client: a non-empty user override wins over the built-in one.
pub fn resolve_client(
    user_id: &str,
    user_secret: &str,
    builtin_id: &str,
    builtin_secret: &str,
) -> oauth::ClientId {
    let (id, secret) = if user_id.trim().is_empty() {
        (builtin_id, builtin_secret)
    } else {
        (user_id, user_secret)
    };
    oauth::ClientId {
        id: id.trim().to_string(),
        secret: Some(secret.trim().to_string()).filter(|s| !s.is_empty()),
    }
}

/// Hands out fresh credentials, refreshing OAuth access tokens when needed.
pub struct AuthManager {
    secrets: Arc<dyn SecretStore>,
    settings: Arc<RwLock<Settings>>,
    tokens: Mutex<HashMap<AccountId, (String, i64)>>,
    /// Serializes refreshes per process (avoids refresh-token races).
    refresh_lock: AsyncMutex<()>,
}

impl AuthManager {
    pub fn new(secrets: Arc<dyn SecretStore>, settings: Arc<RwLock<Settings>>) -> Self {
        AuthManager {
            secrets,
            settings,
            tokens: Mutex::new(HashMap::new()),
            refresh_lock: AsyncMutex::new(()),
        }
    }

    /// The OAuth client to use for a provider: the user's override, if any,
    /// otherwise the one built into this binary.
    pub fn client_id(&self, p: Provider) -> oauth::ClientId {
        use crate::settings::builtin_oauth as builtin;
        let s = self.settings.read().unwrap();
        match p {
            Provider::Gmail => resolve_client(
                &s.google_client_id,
                &s.google_client_secret,
                builtin::GOOGLE_CLIENT_ID,
                builtin::GOOGLE_CLIENT_SECRET,
            ),
            _ => resolve_client(&s.microsoft_client_id, "", builtin::MICROSOFT_CLIENT_ID, ""),
        }
    }

    pub fn save_password(&self, id: AccountId, password: &str) -> Result<()> {
        self.secrets.set(&password_key(id), password)
    }

    pub fn save_oauth(&self, id: AccountId, tokens: &oauth::Tokens) -> Result<()> {
        if let Some(r) = &tokens.refresh_token {
            self.secrets.set(&refresh_key(id), r)?;
        }
        self.tokens
            .lock()
            .unwrap()
            .insert(id, (tokens.access_token.clone(), tokens.expires_at));
        Ok(())
    }

    pub fn forget(&self, id: AccountId) {
        let _ = self.secrets.delete(&password_key(id));
        let _ = self.secrets.delete(&refresh_key(id));
        self.tokens.lock().unwrap().remove(&id);
    }

    /// Drops the cached access token (e.g. after the server rejected it).
    pub fn invalidate(&self, id: AccountId) {
        self.tokens.lock().unwrap().remove(&id);
    }

    /// Current access token for an OAuth account, refreshing if it expires within 2 minutes.
    pub async fn access_token(&self, a: &AccountConfig) -> Result<String> {
        if let Some((t, exp)) = self.tokens.lock().unwrap().get(&a.id).cloned()
            && exp > now_ms() + 120_000
        {
            return Ok(t);
        }
        let _guard = self.refresh_lock.lock().await;
        if let Some((t, exp)) = self.tokens.lock().unwrap().get(&a.id).cloned()
            && exp > now_ms() + 120_000
        {
            return Ok(t);
        }
        let refresh = self
            .secrets
            .get(&refresh_key(a.id))?
            .ok_or_else(|| coded(ErrorCode::AuthRequired, "no refresh token stored"))?;
        let client = self.client_id(a.provider);
        if client.id.is_empty() {
            return Err(coded(
                ErrorCode::AuthRequired,
                "OAuth client ID not configured",
            ));
        }
        let t = oauth::refresh(a.provider, &client, &refresh).await?;
        if let Some(new_refresh) = &t.refresh_token
            && *new_refresh != refresh
        {
            // Microsoft rotates refresh tokens.
            self.secrets.set(&refresh_key(a.id), new_refresh)?;
        }
        self.tokens
            .lock()
            .unwrap()
            .insert(a.id, (t.access_token.clone(), t.expires_at));
        Ok(t.access_token)
    }

    pub async fn credentials(&self, a: &AccountConfig) -> Result<Credentials> {
        match a.auth {
            AuthKind::Password => {
                let password = self
                    .secrets
                    .get(&password_key(a.id))?
                    .ok_or_else(|| coded(ErrorCode::AuthRequired, "no password stored"))?;
                Ok(Credentials::Password {
                    user: a.username.clone(),
                    password,
                })
            }
            AuthKind::OAuth2 => Ok(Credentials::OAuth2 {
                user: a.username.clone(),
                access_token: self.access_token(a).await?,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_override_wins_over_built_in_client() {
        let c = resolve_client("", "", "builtin-id", "builtin-secret");
        assert_eq!(c.id, "builtin-id");
        assert_eq!(c.secret.as_deref(), Some("builtin-secret"));

        // An override replaces both id and secret (never mixes the two clients).
        let c = resolve_client(" mine ", "", "builtin-id", "builtin-secret");
        assert_eq!(c.id, "mine");
        assert_eq!(c.secret, None);

        let c = resolve_client("", "", "", "");
        assert!(c.id.is_empty(), "no client at all: sign-in reports it");
    }
}

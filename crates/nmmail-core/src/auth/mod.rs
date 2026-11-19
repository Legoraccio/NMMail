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

    pub fn client_id(&self, p: Provider) -> oauth::ClientId {
        let s = self.settings.read().unwrap();
        match p {
            Provider::Gmail => oauth::ClientId {
                id: s.google_client_id.clone(),
                secret: Some(s.google_client_secret.clone()).filter(|x| !x.is_empty()),
            },
            _ => oauth::ClientId {
                id: s.microsoft_client_id.clone(),
                secret: None,
            },
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

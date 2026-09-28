//! Аутентификация Codex: клеймы JWT, refresh, login.
//! Источники: codex-rs (`core/src/auth.rs`, `login/`), codex-switcher (`token_refresh.rs`),
//! OmniRoute (`tokenRefresh/providers/codex.ts` — refresh без scope).

pub mod jwt;
pub mod login;
pub mod refresh;

pub const AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

#[derive(Clone)]
pub struct Tokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    /// `chatgpt_account_id` из id_token (или явно из файла).
    pub account_id: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub last_refresh: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus { Active, Refreshing, NeedsLogin }

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("refresh token unusable ({0}); account needs login")]
    Unrecoverable(String),          // refresh_token_reused | invalid_grant | token_expired | 401 от token endpoint
    #[error("token endpoint transient error: {0}")]
    Transient(String),
    #[error("malformed token response: {0}")]
    Malformed(String),
}

/// Debug без токенов: `AccountState` деривит Debug, и без этого access/refresh/id попали бы в логи (ревью).
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("account_id", &self.account_id)
            .field("expires_at", &self.expires_at)
            .field("last_refresh", &self.last_refresh)
            .field("id_token", &"<redacted>").field("access_token", &"<redacted>").field("refresh_token", &"<redacted>")
            .finish()
    }
}

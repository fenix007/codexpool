//! Декодирование клеймов id_token / access_token без верификации подписи —
//! так же делают codex-rs, CLIProxyAPI (`jwt_parser.go`), codex-auth (`auth.zig:147-191`).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Claims {
    pub exp: Option<i64>,
    pub email: Option<String>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    pub auth: AuthClaims,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuthClaims {
    pub chatgpt_account_id: Option<String>,
    pub chatgpt_user_id: Option<String>,
    pub user_id: Option<String>,
    /// free | go | plus | prolite | pro | business | enterprise | edu
    pub chatgpt_plan_type: Option<String>,
    pub chatgpt_subscription_active_until: Option<String>,
    #[serde(default)]
    pub organizations: Vec<Organization>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Organization {
    pub id: String,
    #[serde(default)]
    pub is_default: bool,
}

pub fn decode(token: &str) -> anyhow::Result<Claims> {
    let payload = token.split('.').nth(1).ok_or_else(|| anyhow::anyhow!("jwt: no payload segment"))?;
    let bytes = URL_SAFE_NO_PAD.decode(payload)?;
    Ok(serde_json::from_slice(&bytes)?)
}

impl Claims {
    /// Значение для заголовка `chatgpt-account-id`: клейм, иначе default-организация.
    pub fn account_id(&self) -> Option<String> {
        self.auth.chatgpt_account_id.clone()
            .or_else(|| self.auth.organizations.iter().find(|o| o.is_default).map(|o| o.id.clone()))
            .or_else(|| self.auth.organizations.first().map(|o| o.id.clone()))
    }

    pub fn expires_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.exp.and_then(|e| chrono::DateTime::from_timestamp(e, 0))
    }
}

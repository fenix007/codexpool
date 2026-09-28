//! Refresh токенов. Инварианты (из аудита):
//! 1. Тело без `scope` — иначе Auth0 инвалидирует семейство refresh-токенов (OmniRoute).
//! 2. Refresh-токены одноразовые: новый refresh_token персистится ДО любой валидации id_token (codex-switcher).
//! 3. На один аккаунт — один in-flight refresh (per-account Mutex; конкуренты ждут результат).
//! 4. На 403 от upstream refresh не делаем (Cloudflare; сжигает токен).
//! 5. Проактивный refresh в фоне за `lead_s` до exp — горячий путь почти никогда не рефрешит.

use std::{sync::Arc, time::Duration};

use serde::Deserialize;

use super::{AuthError, Tokens, CLIENT_ID, TOKEN_URL};
use crate::{config::RefreshConfig, pool::{AccountId, Pool}, upstream::Upstream};

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    id_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

/// Выполнить refresh для аккаунта. Вызывающий держит per-account мьютекс (см. Pool::refresh_guard).
pub async fn refresh_tokens(http: &reqwest::Client, current: &Tokens) -> Result<Tokens, AuthError> {
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", current.refresh_token.as_str()),
        // намеренно без scope
    ];
    let mut last_err = None;
    for attempt in 0..3u32 {
        let resp = http.post(TOKEN_URL).form(&form).timeout(Duration::from_secs(10)).send().await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let tr: TokenResponse = r.json().await.map_err(|e| AuthError::Malformed(e.to_string()))?;
                let id_token = tr.id_token.unwrap_or_else(|| current.id_token.clone());
                let claims = super::jwt::decode(&tr.access_token).ok();
                let expires_at = claims.as_ref().and_then(|c| c.expires_at())
                    .or_else(|| tr.expires_in.map(|s| chrono::Utc::now() + chrono::Duration::seconds(s as i64)))
                    .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(1));
                return Ok(Tokens {
                    id_token,
                    access_token: tr.access_token,
                    // Новый refresh_token — обязателен к сохранению вызывающим до чего бы то ни было ещё.
                    refresh_token: tr.refresh_token.unwrap_or_else(|| current.refresh_token.clone()),
                    account_id: current.account_id.clone(),
                    expires_at,
                    last_refresh: Some(chrono::Utc::now()),
                });
            }
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                if status.as_u16() == 401 || body.contains("refresh_token_reused") || body.contains("invalid_grant") || body.contains("token_expired") {
                    return Err(AuthError::Unrecoverable(format!("{status}: {body}")));
                }
                last_err = Some(AuthError::Transient(format!("{status}: {body}")));
            }
            Err(e) => last_err = Some(AuthError::Transient(e.to_string())),
        }
        tokio::time::sleep(Duration::from_millis(250 * (attempt as u64 + 1))).await;
    }
    Err(last_err.unwrap_or_else(|| AuthError::Transient("exhausted retries".into())))
}

/// Фоновый цикл: каждые 30 с — найти учётки с exp - now < lead_s и обновить (под per-account мьютексом).
pub async fn background_loop(pool: Arc<Pool>, upstream: Arc<Upstream>, cfg: RefreshConfig) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        for id in pool.accounts_expiring_within(Duration::from_secs(cfg.lead_s)) {
            let pool = pool.clone();
            let http = upstream.auth_client();
            tokio::spawn(async move {
                if let Err(e) = pool.refresh_account(&id, &http).await {
                    tracing::warn!(account = %id, error = %e, "background refresh failed");
                }
            });
        }
    }
}

#[allow(dead_code)]
pub type RefreshResult = Result<Tokens, AuthError>;
#[allow(dead_code)]
fn _assert_send(_: &dyn Fn(AccountId)) {}

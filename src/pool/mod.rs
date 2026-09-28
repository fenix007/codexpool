//! Пул учёток: состояние в памяти + персист изменений в Store.
//! Окна квоты 5h (18000 с) и weekly (604800 с) — нормализуем по длительности, не по имени
//! (бэкенд иногда меняет primary/secondary местами — codex-switcher `api/usage.rs:506-526`).

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::{auth::{AuthStatus, Tokens}, config::Config, store::Store};

pub type AccountId = String;

pub const WINDOW_5H_S: u32 = 18_000;
pub const WINDOW_WEEKLY_S: u32 = 604_800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope { Codex, Spark }

impl Scope {
    pub fn parse(s: &str) -> Self { if s.eq_ignore_ascii_case("spark") { Scope::Spark } else { Scope::Codex } }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Window {
    pub used_percent: f32,
    pub limit_seconds: u32,
    pub reset_at: Option<DateTime<Utc>>,
    pub observed_at: Option<DateTime<Utc>>,
}

impl Window {
    /// Остаток с учётом пассивного сброса по reset_at.
    pub fn headroom(&self, now: DateTime<Utc>) -> f32 {
        match self.reset_at { Some(r) if now >= r => 100.0, _ => (100.0 - self.used_percent).max(0.0) }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeState {
    pub window_5h: Window,
    pub window_weekly: Window,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub cooldown_reason: Option<String>,
    pub backoff_level: u8,
    #[serde(skip)]
    pub inflight: u32,
}

#[derive(Debug, Clone)]
pub struct AccountState {
    pub id: AccountId,
    pub alias: String,
    pub email: String,
    pub plan: String,
    pub chatgpt_account_id: String,
    pub tokens: Tokens,
    pub auth_status: AuthStatus,
    pub priority: i32,
    pub enabled: bool,
    pub tags: Vec<String>,
    pub scopes: std::collections::HashMap<Scope, ScopeState>,
    pub last_error: Option<String>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl AccountState {
    pub fn ready(&self, scope: Scope, now: DateTime<Utc>) -> bool {
        if !self.enabled || self.auth_status == AuthStatus::NeedsLogin { return false; }
        let Some(s) = self.scopes.get(&scope) else { return true };
        if matches!(s.cooldown_until, Some(u) if u > now) { return false; }
        s.window_5h.headroom(now) > 0.0 && s.window_weekly.headroom(now) > 0.0
    }
}

/// Что наблюдали в ответе upstream — для обновления окон.
#[derive(Debug, Clone, Default)]
pub struct Observation {
    pub window_5h: Option<Window>,
    pub window_weekly: Option<Window>,
}

pub struct Pool {
    store: Arc<Store>,
    accounts: DashMap<AccountId, AccountState>,
    /// per-account мьютекс для refresh (singleflight).
    refresh_locks: DashMap<AccountId, Arc<tokio::sync::Mutex<()>>>,
    /// session_id → account_id (sticky), с TTL.
    sticky: DashMap<String, (AccountId, DateTime<Utc>)>,
    rr_cursor: parking_lot::Mutex<usize>,
}

impl Pool {
    pub fn load(store: Arc<Store>, _cfg: &Config) -> anyhow::Result<Self> {
        let accounts = DashMap::new();
        for a in store.load_accounts()? { accounts.insert(a.id.clone(), a); }
        Ok(Self { store, accounts, refresh_locks: DashMap::new(), sticky: DashMap::new(), rr_cursor: parking_lot::Mutex::new(0) })
    }

    pub fn len(&self) -> usize { self.accounts.len() }

    pub fn get(&self, id: &str) -> Option<AccountState> { self.accounts.get(id).map(|a| a.value().clone()) }

    pub fn snapshot(&self) -> Vec<AccountState> { self.accounts.iter().map(|e| e.value().clone()).collect() }

    /// Готовые учётки из набора (уже отфильтрованного ступенью combo), с promote_expired.
    pub fn ready_among(&self, ids: &[AccountId], scope: Scope) -> Vec<AccountState> {
        let now = Utc::now();
        ids.iter().filter_map(|id| self.accounts.get(id)).filter(|a| a.ready(scope, now)).map(|a| a.value().clone()).collect()
    }

    pub fn next_round_robin(&self) -> usize { let mut c = self.rr_cursor.lock(); *c = c.wrapping_add(1); *c }

    pub fn sticky_get(&self, session_id: &str) -> Option<AccountId> {
        let now = Utc::now();
        self.sticky.get(session_id).filter(|e| e.value().1 > now).map(|e| e.value().0.clone())
    }
    pub fn sticky_set(&self, session_id: &str, account: &AccountId) {
        self.sticky.insert(session_id.to_string(), (account.clone(), Utc::now() + chrono::Duration::hours(1)));
        // Ленивая чистка истёкших привязок, чтобы карта не росла бесконечно (ревью).
        if self.sticky.len() > 10_000 { let now = Utc::now(); self.sticky.retain(|_, v| v.1 > now); }
    }

    pub fn inflight_add(&self, id: &str, scope: Scope, delta: i32) {
        if let Some(mut a) = self.accounts.get_mut(id) {
            let s = a.scopes.entry(scope).or_default();
            s.inflight = (s.inflight as i32 + delta).max(0) as u32;
        }
    }

    /// Обновить окна по заголовкам/телу ответа и записать снапшот.
    pub fn observe(&self, id: &str, scope: Scope, obs: Observation) {
        if let Some(mut a) = self.accounts.get_mut(id) {
            let s = a.scopes.entry(scope).or_default();
            if let Some(w) = obs.window_5h { s.window_5h = w; }
            if let Some(w) = obs.window_weekly { s.window_weekly = w; }
            let snapshot = s.clone();
            a.last_used_at = Some(Utc::now());
            drop(a); // не держим shard-lock DashMap на время записи в SQLite (ревью)
            let _ = self.store.save_scope(id, scope, &snapshot); // TODO(MVP-1): отдать в фоновый writer вместо синхронной записи
        }
    }

    pub fn cooldown(&self, id: &str, scope: Scope, until: DateTime<Utc>, reason: &str, bump_backoff: bool) {
        if let Some(mut a) = self.accounts.get_mut(id) {
            let s = a.scopes.entry(scope).or_default();
            s.cooldown_until = Some(until);
            s.cooldown_reason = Some(reason.to_string());
            if bump_backoff { s.backoff_level = s.backoff_level.saturating_add(1); } else { s.backoff_level = 0; }
            let snapshot = s.clone();
            a.last_error = Some(reason.to_string());
            drop(a);
            let _ = self.store.save_scope(id, scope, &snapshot);
        }
    }

    pub fn set_auth_status(&self, id: &str, status: AuthStatus) {
        if let Some(mut a) = self.accounts.get_mut(id) { a.auth_status = status; }
        let _ = self.store.save_auth_status(id, status); // запись уже без guard
    }

    pub fn accounts_expiring_within(&self, lead: Duration) -> Vec<AccountId> {
        let deadline = Utc::now() + chrono::Duration::from_std(lead).unwrap_or_default();
        self.accounts.iter()
            .filter(|a| a.enabled && a.auth_status == AuthStatus::Active && a.tokens.expires_at <= deadline)
            .map(|a| a.id.clone()).collect()
    }

    /// Singleflight refresh: конкуренты ждут мьютекс и видят уже обновлённый токен.
    pub async fn refresh_account(&self, id: &str, http: &reqwest::Client) -> anyhow::Result<Tokens> {
        let lock = self.refresh_locks.entry(id.to_string()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone();
        let _g = lock.lock().await;
        let current = self.get(id).ok_or_else(|| anyhow::anyhow!("unknown account {id}"))?;
        // Если пока ждали — кто-то уже обновил.
        if current.tokens.expires_at > Utc::now() + chrono::Duration::minutes(5) && current.tokens.last_refresh.map_or(false, |t| Utc::now() - t < chrono::Duration::seconds(30)) {
            return Ok(current.tokens);
        }
        self.set_auth_status(id, AuthStatus::Refreshing);
        match crate::auth::refresh::refresh_tokens(http, &current.tokens).await {
            Ok(new) => {
                // persist-first: новый refresh_token на диск раньше всего остального.
                self.store.save_tokens(id, &new)?;
                if let Some(mut a) = self.accounts.get_mut(id) { a.tokens = new.clone(); a.auth_status = AuthStatus::Active; }
                self.store.save_auth_status(id, AuthStatus::Active)?;
                metrics::counter!("codexpool_refresh_total", "account" => id.to_string(), "result" => "ok").increment(1);
                Ok(new)
            }
            Err(crate::auth::AuthError::Unrecoverable(msg)) => {
                self.set_auth_status(id, AuthStatus::NeedsLogin);
                metrics::counter!("codexpool_refresh_total", "account" => id.to_string(), "result" => "needs_login").increment(1);
                Err(anyhow::anyhow!("needs login: {msg}"))
            }
            Err(e) => {
                self.set_auth_status(id, AuthStatus::Active);
                metrics::counter!("codexpool_refresh_total", "account" => id.to_string(), "result" => "transient").increment(1);
                Err(e.into())
            }
        }
    }
}

/// Фоновый опрос `GET https://chatgpt.com/backend-api/wham/usage` для активных за сутки учёток —
/// чтобы failback после сброса окна происходил сразу, а `headroom` видел реальные проценты.
pub async fn usage_poll_loop(pool: Arc<Pool>, upstream: Arc<crate::upstream::Upstream>, cfg: crate::config::RefreshConfig) {
    let mut tick = tokio::time::interval(Duration::from_secs(cfg.usage_poll_interval_s.max(60)));
    loop {
        tick.tick().await;
        for a in pool.snapshot() {
            let recent = a.last_used_at.map_or(false, |t| Utc::now() - t < chrono::Duration::hours(24));
            if !recent || a.auth_status != AuthStatus::Active { continue; }
            match upstream.fetch_usage(&a).await {
                Ok(obs) => pool.observe(&a.id, Scope::Codex, obs),
                Err(e) => tracing::debug!(account = %a.alias, error = %e, "usage poll failed"),
            }
        }
    }
}

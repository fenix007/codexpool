//! Upstream-клиент к chatgpt.com: один reqwest::Client на процесс (пул h2/TLS общий для всех учёток),
//! сборка codex-заголовков, классификация ответа, парсинг окон квоты.

use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use http::HeaderMap;

use crate::{config::UpstreamConfig, pool::{AccountState, Observation, Window, WINDOW_5H_S, WINDOW_WEEKLY_S}};

pub struct Upstream {
    cfg: UpstreamConfig,
    client: reqwest::Client,
    auth_client: reqwest::Client,
}

/// Заголовки клиента, которые пробрасываем как есть (см. CLIProxyAPI `applyCodexHeadersFromSources`).
pub const PASSTHROUGH_HEADERS: &[&str] = &[
    "originator", "user-agent", "openai-beta", "session_id", "x-codex-turn-metadata", "x-codex-turn-state",
    "x-client-request-id", "thread-id", "x-codex-window-id", "x-openai-internal-codex-responses-lite", "accept-language",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 200 и первый чанк без ошибки — стрим можно коммитить клиенту.
    Streamable,
    /// Учётка виновата — cooldown и следующая.
    RateLimited { retry_after: Option<Duration> },
    Unauthorized,          // 401 → refresh + retry на той же учётке (один раз)
    Forbidden,             // 403 → без refresh, cooldown
    UpstreamError,         // 5xx / 408 / ошибка в теле 200 до первого дельта / timeout до первого байта
    /// Запрос виноват — сразу клиенту без failover.
    ClientError(u16),
}

impl Upstream {
    pub fn new(cfg: &UpstreamConfig) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .pool_max_idle_per_host(cfg.pool_max_idle_per_host)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            // общий timeout не ставим — стрим; таймауты первого байта/idle — в proxy
            .build()?;
        let auth_client = reqwest::Client::builder().use_rustls_tls().timeout(Duration::from_secs(15)).build()?;
        Ok(Self { cfg: cfg.clone(), client, auth_client })
    }

    pub fn auth_client(&self) -> reqwest::Client { self.auth_client.clone() }

    pub fn responses_url(&self) -> String { format!("{}/responses", self.cfg.base_url.trim_end_matches('/')) }

    /// Отправить тело как есть (Bytes клонируется без копии) от имени учётки.
    pub async fn send_responses(&self, account: &AccountState, client_headers: &HeaderMap, body: Bytes, session_id: &str)
        -> reqwest::Result<reqwest::Response>
    {
        let mut req = self.client.post(self.responses_url())
            .header("authorization", format!("Bearer {}", account.tokens.access_token))
            .header("chatgpt-account-id", &account.chatgpt_account_id)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("session_id", session_id);
        let mut has_ua = false; let mut has_orig = false;
        for name in PASSTHROUGH_HEADERS {
            if let Some(v) = client_headers.get(*name) {
                req = req.header(*name, v);
                if *name == "user-agent" { has_ua = true; }
                if *name == "originator" { has_orig = true; }
            }
        }
        if !has_ua { req = req.header("user-agent", &self.cfg.default_user_agent); }
        if !has_orig { req = req.header("originator", &self.cfg.default_originator); }
        req.body(body).send().await
    }

    /// Классификация по статусу и заголовкам (тело 200 проверяется в proxy по первому чанку).
    pub fn classify_status(status: u16, headers: &HeaderMap) -> Outcome {
        match status {
            200 => Outcome::Streamable,
            429 => Outcome::RateLimited { retry_after: retry_after(headers) },
            401 => Outcome::Unauthorized,
            403 => Outcome::Forbidden,
            300..=399 | 408 | 500..=599 => Outcome::UpstreamError, // редирект WAF/auth — проблема пути к upstream, не запроса
            s => Outcome::ClientError(s),
        }
    }

    /// Окна квоты из заголовков `x-codex-5h-*` / `x-codex-7d-*` (OmniRoute `quota.ts`), нормализованные по длительности.
    pub fn observe_headers(headers: &HeaderMap) -> Observation {
        fn f(h: &HeaderMap, k: &str) -> Option<f32> { h.get(k)?.to_str().ok()?.parse().ok() }
        fn t(h: &HeaderMap, k: &str) -> Option<DateTime<Utc>> {
            let v = h.get(k)?.to_str().ok()?;
            v.parse::<i64>().ok().and_then(|s| DateTime::from_timestamp(s, 0)).or_else(|| v.parse().ok())
        }
        let now = Utc::now();
        let mk = |usage: Option<f32>, limit: Option<f32>, reset: Option<DateTime<Utc>>, secs: u32| -> Option<Window> {
            let (u, l) = (usage?, limit.filter(|l| *l > 0.0)?);
            Some(Window { used_percent: (u / l * 100.0).min(100.0), limit_seconds: secs, reset_at: reset, observed_at: Some(now) })
        };
        Observation {
            window_5h: mk(f(headers, "x-codex-5h-usage"), f(headers, "x-codex-5h-limit"), t(headers, "x-codex-5h-reset-at"), WINDOW_5H_S),
            window_weekly: mk(f(headers, "x-codex-7d-usage"), f(headers, "x-codex-7d-limit"), t(headers, "x-codex-7d-reset-at"), WINDOW_WEEKLY_S),
        }
    }

    /// `GET https://chatgpt.com/backend-api/wham/usage` — активный опрос окон (codex-switcher `api/usage.rs`).
    pub async fn fetch_usage(&self, account: &AccountState) -> anyhow::Result<Observation> {
        #[derive(serde::Deserialize)] struct W { used_percent: f32, limit_window_seconds: u32, reset_at: Option<i64> }
        #[derive(serde::Deserialize)] struct RL { primary_window: Option<W>, secondary_window: Option<W> }
        #[derive(serde::Deserialize)] struct R { rate_limit: Option<RL> }
        let r: R = self.auth_client.get("https://chatgpt.com/backend-api/wham/usage")
            .bearer_auth(&account.tokens.access_token)
            .header("chatgpt-account-id", &account.chatgpt_account_id)
            .header("user-agent", &self.cfg.default_user_agent)
            .send().await?.error_for_status()?.json().await?;
        let now = Utc::now();
        let mut obs = Observation::default();
        for w in r.rate_limit.into_iter().flat_map(|rl| [rl.primary_window, rl.secondary_window]).flatten() {
            let win = Window { used_percent: w.used_percent, limit_seconds: w.limit_window_seconds,
                               reset_at: w.reset_at.and_then(|s| DateTime::from_timestamp(s, 0)), observed_at: Some(now) };
            // нормализация по длительности, не по имени
            if w.limit_window_seconds <= 24 * 3600 { obs.window_5h = Some(win) } else { obs.window_weekly = Some(win) }
        }
        Ok(obs)
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers.get("retry-after")?.to_str().ok()?.parse::<u64>().ok().map(Duration::from_secs)
}

/// `resets_in_seconds` из тела 429 `{"error":{"type":"usage_limit_reached","resets_in_seconds":N}}`.
pub fn resets_in_from_body(body: &[u8]) -> Option<Duration> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.pointer("/error/resets_in_seconds").and_then(|x| x.as_u64()).map(Duration::from_secs)
}

//! Статистика: RequestEvent → mpsc → batch writer (никогда не блокирует горячий путь) → SQLite; Stats API отдельно.

pub mod api;

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{config::StatsConfig, pool::Scope, store::Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass { Ok, Http429, Http401, Http403, Http5xx, UpstreamTimeout, StreamError, StreamAbort, ClientAbort, PoolExhausted, BadRequest }

#[derive(Debug, Clone, Serialize)]
pub struct Attempt { pub account: String, pub status: u16, pub error_class: String, pub ms: u64 }

#[derive(Debug, Clone, Serialize)]
pub struct RequestEvent {
    pub ts: DateTime<Utc>,
    pub request_id: String,
    pub client: String,
    pub client_app: String,        // codex-cli | claude-code | openai-sdk | other
    pub client_version: Option<String>,
    pub bench_run_id: Option<String>,   // x-bench-run-id из harness
    pub endpoint: String,
    pub model: String,
    pub upstream_model: Option<String>,
    pub scope: Scope,
    pub combo: String,
    pub step: Option<usize>,
    pub account_id: Option<String>,
    pub account_alias: Option<String>,
    pub attempts: Vec<Attempt>,
    pub status: Option<u16>,
    pub error_class: ErrorClass,
    pub error_message: Option<String>,
    pub ttft_ms: Option<u64>,
    pub upstream_ttfb_ms: Option<u64>,
    pub route_ms: Option<u64>,
    pub total_ms: Option<u64>,
    pub stream: bool,
    pub input_tokens: Option<u64>, pub output_tokens: Option<u64>, pub cached_tokens: Option<u64>, pub reasoning_tokens: Option<u64>,
    pub tps: Option<f64>,
    pub request_bytes: usize, pub response_bytes: usize,
    pub session_id: String, pub sticky_hit: bool,
}

impl RequestEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn start(client: &str, headers: &http::HeaderMap, endpoint: &str, model: &str, scope: Scope, combo: &str, stream: bool, request_bytes: usize, session_id: &str) -> Self {
        let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("");
        let orig = headers.get("originator").and_then(|v| v.to_str().ok()).unwrap_or("");
        let (client_app, client_version) = detect_client(ua, orig);
        Self {
            ts: Utc::now(), request_id: uuid::Uuid::new_v4().to_string(), client: client.into(), client_app, client_version,
            bench_run_id: headers.get("x-bench-run-id").and_then(|v| v.to_str().ok()).map(str::to_string),
            endpoint: endpoint.into(), model: model.into(), upstream_model: None, scope, combo: combo.into(), step: None,
            account_id: None, account_alias: None, attempts: vec![], status: None, error_class: ErrorClass::Ok, error_message: None,
            ttft_ms: None, upstream_ttfb_ms: None, route_ms: None, total_ms: None, stream,
            input_tokens: None, output_tokens: None, cached_tokens: None, reasoning_tokens: None, tps: None,
            request_bytes, response_bytes: 0, session_id: session_id.into(), sticky_hit: false,
        }
    }
    pub fn attempt(&mut self, alias: &str, status: u16, class: &str, took: Duration) {
        self.attempts.push(Attempt { account: alias.into(), status, error_class: class.into(), ms: took.as_millis() as u64 });
    }
    pub fn committed(&mut self, account_id: &str, alias: &str, step: usize, upstream_model: &str, since_start: Duration, since_attempt: Duration, sticky_hit: bool) {
        self.account_id = Some(account_id.into()); self.account_alias = Some(alias.into()); self.step = Some(step);
        self.upstream_model = Some(upstream_model.into()); self.sticky_hit = sticky_hit;
        self.upstream_ttfb_ms = Some(since_attempt.as_millis() as u64);
        self.route_ms = Some(since_start.saturating_sub(since_attempt).as_millis() as u64);
        self.attempts.push(Attempt { account: alias.into(), status: 200, error_class: "ok".into(), ms: since_attempt.as_millis() as u64 });
    }
    pub fn finish(&mut self, class: ErrorClass, status: Option<u16>, msg: Option<String>) {
        self.error_class = class; self.status = status; self.error_message = msg;
        self.total_ms = Some((Utc::now() - self.ts).num_milliseconds().max(0) as u64);
        if let (Some(out), Some(ttft), Some(total)) = (self.output_tokens, self.ttft_ms, self.total_ms) {
            let gen_ms = total.saturating_sub(ttft).max(1) as f64;
            self.tps = Some(out as f64 / gen_ms * 1000.0);
        }
    }
}

fn detect_client(ua: &str, originator: &str) -> (String, Option<String>) {
    let ver = |s: &str| s.split('/').nth(1).map(|v| v.split_whitespace().next().unwrap_or("").to_string());
    if originator.starts_with("codex") || ua.starts_with("codex") { return ("codex-cli".into(), ver(ua)); }
    if originator.starts_with("claude-code") || ua.starts_with("claude-cli") { return ("claude-code".into(), ver(ua)); }
    if ua.starts_with("OpenAI/") || ua.contains("openai-python") || ua.contains("openai-node") { return ("openai-sdk".into(), ver(ua)); }
    ("other".into(), None)
}

pub struct Collector;

impl Collector {
    /// Возвращает sender для горячего пути и JoinHandle writer-таска.
    pub fn spawn(store: Arc<Store>, cfg: &StatsConfig) -> (mpsc::Sender<RequestEvent>, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<RequestEvent>(cfg.channel_capacity);
        let store_errors = cfg.store_errors;
        let handle = tokio::spawn(async move {
            let mut batch: Vec<RequestEvent> = Vec::with_capacity(128);
            let mut tick = tokio::time::interval(Duration::from_millis(200));
            loop {
                tokio::select! {
                    ev = rx.recv() => match ev { Some(e) => { batch.push(e); if batch.len() >= 100 { flush(&store, &mut batch, store_errors); } }, None => { flush(&store, &mut batch, store_errors); break; } },
                    _ = tick.tick() => flush(&store, &mut batch, store_errors),
                }
            }
        });
        (tx, handle)
    }
}

/// Запись батча. `block_in_place` — чтобы синхронный rusqlite не занимал рабочий поток tokio без предупреждения
/// планировщика (ревью; рантайм многопоточный, `#[tokio::main]`). При ошибке батч не выбрасывается, а копится
/// до 10 000 событий и пишется при следующем тике; сверх лимита старые события отбрасываются со счётчиком.
fn flush(store: &Store, batch: &mut Vec<RequestEvent>, store_errors: bool) {
    if batch.is_empty() { return; }
    match tokio::task::block_in_place(|| store.insert_events(batch, store_errors)) {
        Ok(()) => batch.clear(),
        Err(e) => {
            tracing::error!(error = %e, n = batch.len(), "stats flush failed, will retry");
            const CAP: usize = 10_000;
            if batch.len() > CAP {
                let dropped = batch.len() - CAP;
                batch.drain(..dropped);
                metrics::counter!("codexpool_stats_dropped_total").increment(dropped as u64);
            }
        }
    }
}

pub async fn retention_loop(store: Arc<Store>, cfg: StatsConfig) {
    let mut tick = tokio::time::interval(Duration::from_secs(24 * 3600));
    loop {
        tick.tick().await;
        if let Err(e) = store.purge_events_older_than(cfg.retention_days) { tracing::warn!(error = %e, "retention purge failed"); }
    }
}

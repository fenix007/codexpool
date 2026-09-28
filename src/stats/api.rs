//! Stats API (контракт — docs/stats-api.md). Отдельный listener, read-only.

use std::sync::Arc;

use axum::{extract::{Path, Query, State}, http::HeaderMap, response::{IntoResponse, Json, Response}, routing::get, Router};
use serde::Deserialize;

use crate::{config::Config, pool::Pool, store::Store};

#[derive(Clone)]
pub struct StatsState { pub cfg: Arc<Config>, pub store: Arc<Store>, pub pool: Arc<Pool> }

#[derive(Debug, Deserialize, Default)]
pub struct Range {
    /// ISO 8601 или относительное: -1h, -24h, -7d. По умолчанию -24h.
    pub from: Option<String>,
    pub to: Option<String>,
    pub account: Option<String>,
    pub model: Option<String>,
    pub client_app: Option<String>,
    pub combo: Option<String>,
    pub bucket: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<i64>,
    pub format: Option<String>,
}

pub fn app(state: StatsState) -> Router {
    Router::new()
        .route("/stats/summary", get(summary))
        .route("/stats/timeseries", get(timeseries))
        .route("/stats/accounts", get(accounts))
        .route("/stats/accounts/{id}/timeseries", get(account_timeseries))
        .route("/stats/models", get(models))
        .route("/stats/errors", get(errors))
        .route("/stats/requests", get(requests))
        .route("/stats/requests/{request_id}", get(request_one))
        .route("/stats/failovers", get(failovers))
        .route("/stats/bench/{run_id}", get(bench_run))
        .route("/stats/export", get(export))
        .route("/stats/stream", get(stream_sse))
        .route("/metrics", get(metrics))
        .route("/healthz", get(healthz))
        .with_state(state)
}

fn authorize(cfg: &Config, headers: &HeaderMap) -> Result<(), Response> {
    // Без admin_key Stats API допустим только на loopback — это проверяет Config::load для stats_listen.
    let Some(admin) = &cfg.admin_key else { return Ok(()) };
    let tok = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    if crate::proxy::ct_eq(tok.as_bytes(), admin.as_bytes()) { Ok(()) } else { Err((axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response()) }
}

macro_rules! guarded {
    ($st:expr, $headers:expr, $body:expr) => {{ if let Err(r) = authorize(&$st.cfg, &$headers) { return r; } $body }};
}

async fn summary(State(st): State<StatsState>, headers: HeaderMap, Query(q): Query<Range>) -> Response {
    guarded!(st, headers, match st.store.summary(&q) { Ok(v) => Json(v).into_response(), Err(e) => err(e) })
}
async fn timeseries(State(st): State<StatsState>, headers: HeaderMap, Query(q): Query<Range>) -> Response {
    guarded!(st, headers, match st.store.timeseries(&q) { Ok(v) => Json(v).into_response(), Err(e) => err(e) })
}
async fn accounts(State(st): State<StatsState>, headers: HeaderMap, Query(q): Query<Range>) -> Response {
    guarded!(st, headers, {
        // живое состояние пула + агрегаты за период
        let live = st.pool.snapshot();
        match st.store.accounts_aggregates(&q) {
            Ok(agg) => {
                let rows: Vec<_> = live.iter().map(|a| serde_json::json!({
                    "id": a.id, "alias": a.alias, "email": a.email, "plan": a.plan, "auth_status": a.auth_status,
                    "enabled": a.enabled, "priority": a.priority, "tags": a.tags, "last_error": a.last_error, "last_used_at": a.last_used_at,
                    "scopes": a.scopes, "period": agg.get(&a.id),
                })).collect();
                Json(rows).into_response()
            }
            Err(e) => err(e),
        }
    })
}
async fn account_timeseries(State(st): State<StatsState>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<Range>) -> Response {
    guarded!(st, headers, match st.store.account_timeseries(&id, &q) { Ok(v) => Json(v).into_response(), Err(e) => err(e) })
}
async fn models(State(st): State<StatsState>, headers: HeaderMap, Query(q): Query<Range>) -> Response {
    guarded!(st, headers, match st.store.models_aggregates(&q) { Ok(v) => Json(v).into_response(), Err(e) => err(e) })
}
async fn errors(State(st): State<StatsState>, headers: HeaderMap, Query(q): Query<Range>) -> Response {
    guarded!(st, headers, match st.store.recent_errors(&q) { Ok(v) => Json(v).into_response(), Err(e) => err(e) })
}
async fn requests(State(st): State<StatsState>, headers: HeaderMap, Query(q): Query<Range>) -> Response {
    guarded!(st, headers, match st.store.requests(&q) { Ok(v) => Json(v).into_response(), Err(e) => err(e) })
}
async fn request_one(State(st): State<StatsState>, headers: HeaderMap, Path(rid): Path<String>) -> Response {
    guarded!(st, headers, match st.store.request_by_id(&rid) { Ok(Some(v)) => Json(v).into_response(), Ok(None) => (axum::http::StatusCode::NOT_FOUND, "not found").into_response(), Err(e) => err(e) })
}
async fn failovers(State(st): State<StatsState>, headers: HeaderMap, Query(q): Query<Range>) -> Response {
    guarded!(st, headers, match st.store.failovers(&q) { Ok(v) => Json(v).into_response(), Err(e) => err(e) })
}
async fn bench_run(State(st): State<StatsState>, headers: HeaderMap, Path(run): Path<String>) -> Response {
    guarded!(st, headers, match st.store.bench_run(&run) { Ok(v) => Json(v).into_response(), Err(e) => err(e) })
}
async fn export(State(st): State<StatsState>, headers: HeaderMap, Query(q): Query<Range>) -> Response {
    guarded!(st, headers, todo!("stream request_events as jsonl/csv for range {:?}", q.format))
}
async fn stream_sse(State(st): State<StatsState>, headers: HeaderMap) -> Response {
    guarded!(st, headers, todo!("broadcast channel of request/account/failover events → SSE"))
}
async fn metrics(State(_st): State<StatsState>) -> Response {
    // metrics-exporter-prometheus: PrometheusBuilder::new().install_recorder() в main; здесь handle.render()
    todo!("render prometheus registry")
}
async fn healthz(State(st): State<StatsState>) -> Response {
    let snap = st.pool.snapshot();
    let ready = snap.iter().filter(|a| a.ready(crate::pool::Scope::Codex, chrono::Utc::now())).count();
    Json(serde_json::json!({"ok": ready > 0, "accounts_ready": ready, "accounts_total": snap.len()})).into_response()
}

fn err(e: anyhow::Error) -> Response { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response() }

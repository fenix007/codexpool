//! Proxy Core: axum-приложение. Горячий путь — `POST /v1/responses` passthrough с failover до первого байта.
//! Трансляции `/v1/chat/completions` и `/v1/messages` — MVP-2 (модули translate::*).

pub mod translate;

use std::{sync::Arc, time::{Duration, Instant}};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router as AxumRouter,
};
use bytes::Bytes;
use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{
    config::Config,
    pool::{Pool, Scope},
    router::Router,
    stats::{ErrorClass, RequestEvent},
    upstream::{Outcome, Upstream},
};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub pool: Arc<Pool>,
    pub router: Arc<Router>,
    pub upstream: Arc<Upstream>,
    pub stats: mpsc::Sender<RequestEvent>,
}

/// Оболочка запроса: парсим только нужные поля; остальные serde пропускает (тело уходит upstream как есть).
/// Поля — `String`, а не `&str`: заимствование ломается на строках с escape-последовательностями,
/// а `flatten` + `RawValue` в serde не поддерживается (замечания внешнего ревью).
#[derive(Deserialize)]
struct Envelope {
    model: String,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    prompt_cache_key: Option<String>,
}

pub fn app(state: AppState) -> AxumRouter {
    AxumRouter::new()
        .route("/v1/responses", post(responses))
        .route("/v1/chat/completions", post(translate::chat::handler))
        .route("/v1/messages", post(translate::messages::handler))
        .route("/v1/messages/count_tokens", post(translate::messages::count_tokens))
        .route("/v1/models", get(models))
        .route("/healthz", get(|| async { "ok" }))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

fn authorize(cfg: &Config, headers: &HeaderMap) -> Result<String, Response> {
    let tok = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    // Сравнение за константное время; в статистику идёт номер ключа, а не его префикс.
    if let Some(i) = cfg.client_keys.iter().position(|k| ct_eq(k.as_bytes(), tok.as_bytes())) { return Ok(format!("key#{i}")); }
    Err((StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":{"type":"invalid_api_key","message":"unknown client key"}}))).into_response())
}

async fn models(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = authorize(&st.cfg, &headers) { return r; }
    let mut ids: Vec<&str> = st.cfg.models.scope.keys().map(String::as_str).collect();
    ids.extend(st.cfg.models.aliases.keys().map(String::as_str));
    ids.sort(); ids.dedup();
    let data: Vec<_> = ids.iter().map(|m| serde_json::json!({"id": m, "object": "model", "owned_by": "codexpool"})).collect();
    Json(serde_json::json!({"object":"list","data":data})).into_response()
}

/// Горячий путь.
pub async fn responses(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let t0 = Instant::now();
    let client = match authorize(&st.cfg, &headers) { Ok(c) => c, Err(r) => return r };
    let env: Envelope = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => return error_json(StatusCode::BAD_REQUEST, "invalid_request", &format!("bad json: {e}")),
    };
    let stream = env.stream.unwrap_or(false);
    let session_id = env.prompt_cache_key.clone()
        .or_else(|| headers.get("session_id").and_then(|v| v.to_str().ok()).map(str::to_string))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let model = env.model.clone();
    let scope = st.router.scope_of(&model);
    let combo = headers.get("x-codexpool-combo").and_then(|v| v.to_str().ok()).unwrap_or(&st.cfg.default_combo).to_string();

    let mut ev = RequestEvent::start(&client, &headers, "responses", &model, scope, &combo, stream, body.len(), &session_id);
    let plan = st.router.plan(&combo, &model, Some(&session_id));
    if plan.is_empty() {
        ev.finish(ErrorClass::PoolExhausted, Some(429), None);
        let _ = st.stats.try_send(ev);
        return pool_exhausted(&st, scope);
    }

    let budget = Duration::from_millis(st.cfg.failover.attempt_budget_ms);
    let first_byte_timeout = Duration::from_millis(st.cfg.upstream.first_byte_timeout_ms);
    // Очередь кандидатов вместо рекурсии: после успешного refresh учётка возвращается в начало очереди.
    // Refresh — не больше одного раза на учётку за запрос (ревью: рекурсия сбрасывала флаг и могла зациклиться).
    let mut refreshed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<crate::router::Candidate> = plan.into();

    while let Some(cand) = queue.pop_front() {
        if t0.elapsed() > budget { break; }
        let Some(account) = st.pool.get(&cand.account) else { continue };
        // Тело меняем только если ступень переписывает модель или клиент просил не-стрим.
        let upstream_body = if cand.upstream_model != model || !stream { rewrite_body(&body, &cand.upstream_model) } else { body.clone() };
        st.pool.inflight_add(&account.id, scope, 1);
        let t_attempt = Instant::now();
        let sent = st.upstream.send_responses(&account, &headers, upstream_body, &session_id).await;
        let resp = match sent {
            Ok(r) => r,
            Err(e) => {
                st.pool.inflight_add(&account.id, scope, -1);
                ev.attempt(&cand.alias, 0, "connect_error", t_attempt.elapsed());
                st.pool.cooldown(&account.id, scope, chrono::Utc::now() + chrono::Duration::seconds(st.cfg.failover.cooldown_5xx_s as i64), &format!("connect: {e}"), false);
                continue;
            }
        };
        let status = resp.status().as_u16();
        let up_headers = resp.headers().clone();
        st.pool.observe(&account.id, scope, Upstream::observe_headers(&up_headers));

        match Upstream::classify_status(status, &up_headers) {
            Outcome::Streamable => {
                // Первый чанк: ждём ≤ first_byte_timeout; ошибка в теле 200 → как UpstreamError.
                let mut upstream_stream = resp.bytes_stream();
                let first = tokio::time::timeout(first_byte_timeout, upstream_stream.next()).await;
                let first_chunk = match first {
                    Ok(Some(Ok(c))) if !looks_like_error_frame(&c) => c,
                    Ok(Some(Ok(c))) => {
                        st.pool.inflight_add(&account.id, scope, -1);
                        ev.attempt(&cand.alias, 200, "stream_error_first_frame", t_attempt.elapsed());
                        st.pool.cooldown(&account.id, scope, chrono::Utc::now() + chrono::Duration::seconds(st.cfg.failover.cooldown_5xx_s as i64), &String::from_utf8_lossy(&c[..c.len().min(200)]), false);
                        continue;
                    }
                    Ok(Some(Err(_))) | Ok(None) => {
                        st.pool.inflight_add(&account.id, scope, -1);
                        ev.attempt(&cand.alias, 200, "stream_abort_before_first_byte", t_attempt.elapsed());
                        continue;
                    }
                    Err(_) => {
                        st.pool.inflight_add(&account.id, scope, -1);
                        ev.attempt(&cand.alias, 200, "first_byte_timeout", t_attempt.elapsed());
                        st.pool.cooldown(&account.id, scope, chrono::Utc::now() + chrono::Duration::seconds(st.cfg.failover.cooldown_5xx_s as i64), "first_byte_timeout", false);
                        continue;
                    }
                };
                // Коммит: с этого момента failover невозможен.
                ev.committed(&account.id, &cand.alias, cand.step, &cand.upstream_model, t0.elapsed(), t_attempt.elapsed(), cand.sticky_hit);
                st.pool.sticky_set(&session_id, &account.id);
                let idle = Duration::from_millis(st.cfg.upstream.idle_timeout_ms);
                let stats = st.stats.clone();
                let pool = st.pool.clone();
                let acct_id = account.id.clone();
                let body_stream = stream_with_accounting(first_chunk, upstream_stream, idle, ev, stats, pool, acct_id, scope, stream);
                let mut out = Response::builder().status(200)
                    .header("content-type", if stream { "text/event-stream" } else { "application/json" })
                    .header("cache-control", "no-cache")
                    .header("x-codexpool-account", HeaderValue::from_str(&cand.alias).unwrap_or(HeaderValue::from_static("?")))
                    .header("x-codexpool-attempts", (cand.step + 1).to_string());
                for k in ["x-codex-5h-usage", "x-codex-5h-limit", "x-codex-5h-reset-at", "x-codex-7d-usage", "x-codex-7d-limit", "x-codex-7d-reset-at"] {
                    if let Some(v) = up_headers.get(k) { out = out.header(k, v); }
                }
                return out.body(Body::from_stream(body_stream)).unwrap();
            }
            Outcome::Unauthorized if !refreshed.contains(&account.id) => {
                st.pool.inflight_add(&account.id, scope, -1);
                let b = resp.bytes().await.unwrap_or_default();
                // «invalidated oauth token» — refresh бесполезен, сразу needs_login (как stable 015b67465).
                let text = String::from_utf8_lossy(&b).to_ascii_lowercase();
                if text.contains("invalidated oauth token") || text.contains("token has been invalidated") {
                    ev.attempt(&cand.alias, 401, "oauth_invalidated", t_attempt.elapsed());
                    st.pool.set_auth_status(&account.id, crate::auth::AuthStatus::NeedsLogin);
                    continue;
                }
                ev.attempt(&cand.alias, 401, "unauthorized_refresh", t_attempt.elapsed());
                refreshed.insert(account.id.clone());
                if st.pool.refresh_account(&account.id, &st.upstream.auth_client()).await.is_ok() {
                    queue.push_front(cand.clone()); // один повтор на той же учётке с новым токеном
                }
                continue;
            }
            Outcome::Unauthorized => {
                st.pool.inflight_add(&account.id, scope, -1);
                ev.attempt(&cand.alias, 401, "unauthorized", t_attempt.elapsed());
                st.pool.cooldown(&account.id, scope, chrono::Utc::now() + chrono::Duration::seconds(st.cfg.failover.cooldown_needs_login_s as i64), "401 after refresh", false);
                continue;
            }
            Outcome::Forbidden => {
                st.pool.inflight_add(&account.id, scope, -1);
                ev.attempt(&cand.alias, 403, "forbidden", t_attempt.elapsed());
                st.pool.cooldown(&account.id, scope, chrono::Utc::now() + chrono::Duration::seconds(st.cfg.failover.cooldown_403_s as i64), "403", false);
                continue;
            }
            Outcome::RateLimited { retry_after } => {
                st.pool.inflight_add(&account.id, scope, -1);
                let body_bytes = resp.bytes().await.unwrap_or_default();
                let wait = retry_after.or_else(|| crate::upstream::resets_in_from_body(&body_bytes))
                    .or_else(|| st.pool.get(&account.id).and_then(|a| a.scopes.get(&scope).and_then(|s| s.window_5h.reset_at)).map(|r| (r - chrono::Utc::now()).to_std().unwrap_or_default()))
                    .unwrap_or(Duration::from_secs(st.cfg.failover.cooldown_429_default_s));
                let wait = wait.max(Duration::from_secs(10)).min(Duration::from_secs(st.cfg.failover.backoff_max_s));
                ev.attempt(&cand.alias, 429, "rate_limited", t_attempt.elapsed());
                st.pool.cooldown(&account.id, scope, chrono::Utc::now() + chrono::Duration::from_std(wait).unwrap_or_default(), "429", true);
                continue;
            }
            Outcome::UpstreamError => {
                st.pool.inflight_add(&account.id, scope, -1);
                ev.attempt(&cand.alias, status, "upstream_error", t_attempt.elapsed());
                st.pool.cooldown(&account.id, scope, chrono::Utc::now() + chrono::Duration::seconds(st.cfg.failover.cooldown_5xx_s as i64), &format!("http {status}"), false);
                continue;
            }
            Outcome::ClientError(code) => {
                st.pool.inflight_add(&account.id, scope, -1);
                let b = resp.bytes().await.unwrap_or_default();
                ev.attempt(&cand.alias, code, "client_error", t_attempt.elapsed());
                ev.finish(ErrorClass::BadRequest, Some(code), None);
                let _ = st.stats.try_send(ev);
                return Response::builder().status(code).header("content-type", "application/json").body(Body::from(b)).unwrap();
            }
        }
    }

    ev.finish(ErrorClass::PoolExhausted, Some(429), None);
    let _ = st.stats.try_send(ev);
    pool_exhausted(&st, scope)
}

/// Проброс чанков клиенту с учётом TTFT, usage из финального фрейма, idle-таймаута и client_abort.
fn stream_with_accounting(
    first: Bytes,
    rest: impl futures::Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    idle: Duration,
    ev: RequestEvent,
    stats: mpsc::Sender<RequestEvent>,
    pool: Arc<Pool>,
    account_id: String,
    scope: Scope,
    _client_wants_stream: bool,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    // Реализация: async_stream / futures::stream::unfold —
    //  * первый чанк отдаём сразу и фиксируем ttft (если в нём есть content-дельта; иначе — при первом дельта-чанке);
    //  * каждый следующий чанк ждём ≤ idle, иначе завершаем с ErrorClass::StreamAbort;
    //  * храним хвост буфера, чтобы из `event: response.completed` вытащить `usage` (парсим только этот фрейм);
    //  * при Drop стрима (клиент ушёл) — ErrorClass::ClientAbort без cooldown;
    //  * в конце: pool.inflight_add(-1), ev.finish(...), stats.try_send(ev).
    //  * если клиент просил stream=false — аккумулировать и отдать финальный `response` объектом (MVP-1.1).
    let _ = (first, idle, ev, stats, pool, account_id, scope);
    let _ = rest;
    futures::stream::empty::<Result<Bytes, std::io::Error>>() // TODO(MVP-1): заменить на реализацию выше
}

/// 200 с ошибкой в теле. Разбираем первый SSE-фрейм как SSE, а не ищем подстроку в сыром TCP-чанке
/// (ревью: фрейм может быть разрезан, а `"type": "error"` с пробелом не находился).
/// TODO(MVP-1): если в первом чанке нет полного фрейма (`\n\n`), дочитать до конца первого фрейма
/// в пределах first_byte_timeout и 64 KB, держа прочитанное в буфере, — и только потом коммитить стрим.
fn looks_like_error_frame(chunk: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&chunk[..chunk.len().min(64 * 1024)]);
    let frame = text.split("\n\n").next().unwrap_or("");
    let mut event = "";
    let mut data = String::new();
    for line in frame.lines() {
        if let Some(v) = line.strip_prefix("event:") { event = v.trim(); }
        else if let Some(v) = line.strip_prefix("data:") { data.push_str(v.trim()); }
    }
    if matches!(event, "error" | "response.failed") { return true; }
    serde_json::from_str::<serde_json::Value>(&data).ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(|t| t == "error" || t == "response.failed"))
        .unwrap_or(false)
}

/// Сравнение секретов за константное время (без внешней зависимости).
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn rewrite_body(body: &Bytes, model: &str) -> Bytes {
    // Единственный случай, когда тело пересобирается: подмена model и/или stream=true.
    let mut v: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    if let Some(o) = v.as_object_mut() { o.insert("model".into(), model.into()); o.insert("stream".into(), true.into()); }
    Bytes::from(serde_json::to_vec(&v).unwrap_or_default())
}

fn pool_exhausted(st: &AppState, scope: Scope) -> Response {
    let soonest = st.pool.snapshot().iter()
        .filter_map(|a| a.scopes.get(&scope).and_then(|s| s.cooldown_until.or(s.window_5h.reset_at)))
        .min();
    let retry = soonest.map(|t| (t - chrono::Utc::now()).num_seconds().max(1)).unwrap_or(60);
    let mut r = error_json(StatusCode::TOO_MANY_REQUESTS, "pool_exhausted", "all accounts are rate limited or unavailable");
    r.headers_mut().insert("retry-after", HeaderValue::from_str(&retry.to_string()).unwrap());
    if let Some(t) = soonest { r.headers_mut().insert("x-codexpool-resets-at", HeaderValue::from_str(&t.to_rfc3339()).unwrap()); }
    r
}

fn error_json(status: StatusCode, typ: &str, msg: &str) -> Response {
    (status, Json(serde_json::json!({"error": {"type": typ, "message": msg}}))).into_response()
}


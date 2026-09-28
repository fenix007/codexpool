//! `/v1/messages` (Anthropic Messages, клиент Claude Code) → Responses.
//! Запрос: system→instructions, messages (text/tool_use/tool_result блоки)→input items, tools(input_schema)→tools(parameters),
//! max_tokens→max_output_tokens, model → alias из конфига (`claude-*` → gpt-5.1-codex).
//! Ответ: message_start / content_block_start(text|tool_use) / content_block_delta(text_delta|input_json_delta) /
//! content_block_stop / message_delta(stop_reason, usage) / message_stop.

use axum::{extract::State, http::HeaderMap, response::{IntoResponse, Json, Response}};
use bytes::Bytes;

use crate::proxy::AppState;

pub async fn handler(State(_st): State<AppState>, _headers: HeaderMap, _body: Bytes) -> Response {
    todo!("MVP-2: translate anthropic messages → responses (SSE both ways)")
}

/// Локальная оценка (Claude Code дергает часто) — не ходим в upstream.
pub async fn count_tokens(_body: Bytes) -> Response {
    let approx = _body.len() / 4;
    Json(serde_json::json!({"input_tokens": approx})).into_response()
}

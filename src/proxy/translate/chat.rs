//! `/v1/chat/completions` → Responses. Запрос: messages→input (system→instructions), tools→tools(type=function, flat name),
//! max_tokens→max_output_tokens, stream_options убрать. Ответ: response.output_text.delta → choices[0].delta.content,
//! function_call_arguments.delta → tool_calls, response.completed.usage → usage-чанк + `data: [DONE]`.

use axum::{extract::State, http::HeaderMap, response::Response};
use bytes::Bytes;

use crate::proxy::AppState;

pub async fn handler(State(_st): State<AppState>, _headers: HeaderMap, _body: Bytes) -> Response {
    todo!("MVP-2: translate chat.completions → responses, reuse proxy::responses failover loop via internal call")
}

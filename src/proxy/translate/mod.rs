//! Трансляции форматов в Responses API (MVP-2). Референс — CLIProxyAPI `sdktranslator`
//! (FormatOpenAI→FormatCodex, FormatClaude→FormatCodex) и OmniRoute translator (с учётом его багов:
//! #14154 `encrypted_function_args`, #7821 `parallel_tool_calls`, #12996 MCP namespace).

pub mod chat;
pub mod messages;

//! `codexpool login [--device]` — PKCE и device-flow. Заимствовать из codex-rs/login
//! (Apache-2.0): параметры authorize (`codex_cli_simplified_flow=true`, `id_token_add_organizations=true`),
//! локальный callback на :1455. Для MVP-1 достаточно импорта готовых токенов — здесь заглушка.

use anyhow::Result;

use crate::{cli::LoginArgs, config::Config};

pub async fn run(_cfg: &Config, args: LoginArgs) -> Result<()> {
    if args.device {
        todo!("device-auth flow: POST /oauth/device/code → poll /oauth/token (grant urn:ietf:params:oauth:grant-type:device_code)")
    }
    todo!("PKCE flow: open AUTH_URL with S256 challenge, listen http://localhost:1455/auth/callback, exchange code, store account")
}

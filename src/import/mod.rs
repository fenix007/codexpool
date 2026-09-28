//! Импортёры учёток. Все источники сводятся к `ImportedAccount`, затем — пробный refresh (если не --no-verify) и upsert.

pub mod omniroute;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use serde::Deserialize;

use crate::{auth::jwt, config::Config};

#[derive(Debug, Clone)]
pub struct ImportedAccount {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: Option<String>,
    pub last_refresh: Option<String>,
    pub alias: Option<String>,
    pub email: Option<String>,
    pub priority: i32,
    pub source: String,
}

impl ImportedAccount {
    /// Достроить email/account_id/plan из клеймов id_token, как делают codex-auth и CLIProxyAPI.
    pub fn enrich(mut self) -> Result<(Self, jwt::Claims)> {
        let claims = jwt::decode(&self.id_token)?;
        if self.account_id.is_none() { self.account_id = claims.account_id(); }
        if self.email.is_none() { self.email = claims.email.clone(); }
        anyhow::ensure!(self.account_id.is_some(), "cannot determine chatgpt_account_id from id_token");
        Ok((self, claims))
    }
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    #[command(subcommand)]
    pub source: Source,
    /// Не делать пробный refresh после импорта.
    #[arg(long, global = true)]
    pub no_verify: bool,
    #[arg(long, global = true)]
    pub tag: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum Source {
    /// SQLite OmniRoute (DATA_DIR/storage.sqlite, прод: /app/data/storage.sqlite): provider_connections WHERE provider='codex' (расшифровка enc:v1: AES-256-GCM).
    Omniroute { #[arg(long)] db: PathBuf, #[arg(long)] env_file: Option<PathBuf>, #[arg(long, env = "STORAGE_ENCRYPTION_KEY")] key: Option<String> },
    /// Через API работающего OmniRoute: POST /api/providers/{id}/codex-auth/export по каждому соединению.
    OmnirouteApi { #[arg(long, default_value = "http://localhost:20128")] url: String, #[arg(long)] token: String },
    /// Один файл ~/.codex/auth.json (вложенный формат Codex CLI) или плоский CPA JSON — определяется автоматически.
    CodexFile { path: PathBuf, #[arg(long)] alias: Option<String> },
    /// Директория codex-auth: ~/.codex/accounts/*/auth.json + registry.json (алиасы).
    CodexDir { #[arg(default_value = "~/.codex/accounts")] path: PathBuf },
    /// Директория CLIProxyAPI: ~/.cli-proxy-api/codex-*.json.
    CpaDir { #[arg(default_value = "~/.cli-proxy-api")] path: PathBuf },
    /// codex-switcher: ~/.codex-switcher/accounts.json.
    Switcher { #[arg(default_value = "~/.codex-switcher/accounts.json")] path: PathBuf },
}

/// `~/.codex/auth.json` (Codex CLI / codex-auth / OmniRoute export).
#[derive(Debug, Deserialize)]
pub struct CodexAuthFile {
    pub auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,
    pub tokens: Option<CodexTokens>,
    pub last_refresh: Option<String>,
}
#[derive(Debug, Deserialize)]
pub struct CodexTokens { pub id_token: String, pub access_token: String, pub refresh_token: String, pub account_id: Option<String> }

/// Плоский формат CLIProxyAPI (`codex-{hash}-{email}-{plan}.json`) и `codex-auth export --cpa`.
#[derive(Debug, Deserialize)]
pub struct CpaTokenFile {
    pub id_token: String, pub access_token: String, pub refresh_token: String,
    pub account_id: Option<String>, pub last_refresh: Option<String>, pub email: Option<String>,
    #[serde(rename = "type")] pub typ: Option<String>, pub expired: Option<String>,
}

pub fn parse_token_file(raw: &str, source: &str) -> Result<ImportedAccount> {
    if let Ok(f) = serde_json::from_str::<CodexAuthFile>(raw) {
        if let Some(t) = f.tokens {
            return Ok(ImportedAccount { id_token: t.id_token, access_token: t.access_token, refresh_token: t.refresh_token,
                account_id: t.account_id, last_refresh: f.last_refresh, alias: None, email: None, priority: 0, source: source.into() });
        }
    }
    let c: CpaTokenFile = serde_json::from_str(raw)?;
    Ok(ImportedAccount { id_token: c.id_token, access_token: c.access_token, refresh_token: c.refresh_token,
        account_id: c.account_id, last_refresh: c.last_refresh, alias: None, email: c.email, priority: 0, source: source.into() })
}

pub async fn run(cfg: &Config, args: ImportArgs) -> Result<()> {
    let accounts: Vec<ImportedAccount> = match args.source {
        Source::Omniroute { db, env_file, key } => omniroute::from_sqlite(&db, key.as_deref(), env_file.as_deref())?,
        Source::OmnirouteApi { url, token } => omniroute::from_api(&url, &token).await?,
        Source::CodexFile { path, alias } => { let mut a = parse_token_file(&std::fs::read_to_string(&path)?, "codex-file")?; a.alias = alias; vec![a] }
        Source::CodexDir { path } => todo!("walk {path:?}/*/auth.json + ../registry.json for aliases"),
        Source::CpaDir { path } => todo!("glob {path:?}/codex-*.json → parse_token_file"),
        Source::Switcher { path } => todo!("parse {path:?}: accounts[].auth_data (type chat_g_p_t) → ImportedAccount, name → alias"),
    };
    let _ = (cfg, args.no_verify, args.tag);
    tracing::info!(n = accounts.len(), "parsed accounts");
    todo!("for each: enrich → optional verify refresh → store.upsert_account → print table alias/email/plan/status")
}

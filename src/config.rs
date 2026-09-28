//! Конфиг `~/.codexpool/config.toml` (см. ../config.example.toml и docs/architecture.md §2).

use std::{collections::HashMap, path::{Path, PathBuf}};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: String,
    pub stats_listen: String,
    pub client_keys: Vec<String>,
    pub admin_key: Option<String>,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    pub default_combo: String,
    #[serde(default)]
    pub upstream: UpstreamConfig,
    #[serde(default)]
    pub failover: FailoverConfig,
    #[serde(default)]
    pub refresh: RefreshConfig,
    #[serde(default)]
    pub stats: StatsConfig,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub combo: Vec<ComboConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UpstreamConfig {
    pub transport: Transport,
    pub tls_profile: TlsProfile,
    pub first_byte_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub pool_max_idle_per_host: usize,
    pub default_user_agent: String,
    pub default_originator: String,
    /// Переопределение базового URL (тесты с mock_upstream.py).
    pub base_url: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Transport { Http, Ws }

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsProfile { Native, Chrome }

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FailoverConfig {
    pub max_attempts: u32,
    pub attempt_budget_ms: u64,
    pub cooldown_429_default_s: u64,
    pub cooldown_5xx_s: u64,
    pub cooldown_403_s: u64,
    pub cooldown_needs_login_s: u64,
    pub backoff_max_s: u64,
    /// Если весь пул в cooldown и ближайший истекает в пределах N секунд — ждать (stable 1c6a46a27), иначе 429 pool_exhausted.
    pub wait_for_cooldown_s: u64,
    /// Один повтор transport-ошибки до первого байта на той же учётке (stable de2b0cdcc).
    pub same_account_transport_retry: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RefreshConfig {
    pub lead_s: u64,
    pub usage_poll_interval_s: u64,
    pub warmup: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StatsConfig {
    pub retention_days: u32,
    pub store_errors: bool,
    pub channel_capacity: usize,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ModelsConfig {
    pub default_scope: String,
    /// model id → scope ("codex" | "spark"); всё остальное — default_scope.
    pub scope: HashMap<String, String>,
    /// alias → реальная модель (например auto → gpt-5.1-codex).
    pub aliases: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComboConfig {
    pub name: String,
    #[serde(rename = "step")]
    pub steps: Vec<ComboStep>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComboStep {
    /// Алиасы учёток; "*" — все; "prefix-*" — по префиксу; "tag:xyz" — по тегу.
    pub accounts: Vec<String>,
    #[serde(default)]
    pub strategy: Strategy,
    #[serde(default)]
    pub sticky: Sticky,
    /// Ограничение/переписывание моделей для ступени (например для api-key fallback).
    #[serde(default)]
    pub models: Option<StepModels>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StepModels {
    #[serde(default)]
    pub only: Vec<String>,
    #[serde(default)]
    pub rewrite: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Порт стратегии stable-форка (codexQuotaDeadlineRouting.ts): weighted-LRU по скорости сгорания недельного остатка до дедлайна.
    #[default] QuotaDeadline,
    Priority, RoundRobin, LeastInflight, Headroom, ResetSoonest,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Sticky { None, #[default] Session }

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path.map(PathBuf::from).unwrap_or_else(|| default_data_dir().join("config.toml"));
        let raw = std::fs::read_to_string(&path).with_context(|| format!("read config {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&raw).context("parse config.toml")?;
        cfg.data_dir = expand_tilde(&cfg.data_dir);
        anyhow::ensure!(!cfg.client_keys.is_empty(), "client_keys must not be empty");
        // Проверка по разобранному адресу, а не по префиксу строки (ревью: `localhost.evil` проходил).
        for addr in [&cfg.listen, &cfg.stats_listen] {
            let sa: std::net::SocketAddr = addr.parse().with_context(|| format!("listen address must be ip:port, got {addr}"))?;
            if !sa.ip().is_loopback() {
                anyhow::ensure!(cfg.admin_key.is_some(), "admin_key is required when {addr} is not loopback");
            }
        }
        if cfg.models.default_scope.is_empty() { cfg.models.default_scope = "codex".into(); }
        Ok(cfg)
    }

    pub fn scope_for(&self, model: &str) -> &str {
        self.models.scope.get(model).map(String::as_str).unwrap_or(&self.models.default_scope)
    }

    pub fn resolve_model<'a>(&'a self, model: &'a str) -> &'a str {
        self.models.aliases.get(model).map(String::as_str).unwrap_or(model)
    }
}

fn default_data_dir() -> PathBuf {
    directories::BaseDirs::new().map(|b| b.home_dir().join(".codexpool")).unwrap_or_else(|| PathBuf::from(".codexpool"))
}

fn expand_tilde(p: &Path) -> PathBuf {
    match (p.to_str(), directories::BaseDirs::new()) {
        (Some(s), Some(b)) if s.starts_with("~/") => b.home_dir().join(&s[2..]),
        _ => p.to_path_buf(),
    }
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            transport: Transport::Http, tls_profile: TlsProfile::Native,
            first_byte_timeout_ms: 30_000, idle_timeout_ms: 60_000, pool_max_idle_per_host: 32,
            default_user_agent: "codex-cli/0.155.0 (Linux; x86_64)".into(),
            default_originator: "codex_cli_rs".into(),
            base_url: "https://chatgpt.com/backend-api/codex".into(),
        }
    }
}
impl Default for FailoverConfig {
    fn default() -> Self {
        Self { max_attempts: 4, attempt_budget_ms: 20_000, cooldown_429_default_s: 60, cooldown_5xx_s: 60,
               cooldown_403_s: 300, cooldown_needs_login_s: 1800, backoff_max_s: 1800,
               wait_for_cooldown_s: 30, same_account_transport_retry: true }
    }
}
impl Default for RefreshConfig {
    fn default() -> Self { Self { lead_s: 600, usage_poll_interval_s: 300, warmup: false } }
}
impl Default for StatsConfig {
    fn default() -> Self { Self { retention_days: 30, store_errors: true, channel_capacity: 4096 } }
}

/// Debug без секретов: `client_keys` и `admin_key` не должны попасть в логи через `?cfg` (ревью).
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("listen", &self.listen).field("stats_listen", &self.stats_listen)
            .field("client_keys", &format_args!("<{} redacted>", self.client_keys.len()))
            .field("admin_key", &self.admin_key.as_ref().map(|_| "<redacted>"))
            .field("data_dir", &self.data_dir).field("default_combo", &self.default_combo)
            .field("upstream", &self.upstream).field("failover", &self.failover)
            .field("refresh", &self.refresh).field("stats", &self.stats)
            .field("models", &self.models).field("combo", &self.combo)
            .finish()
    }
}

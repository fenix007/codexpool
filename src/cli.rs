//! CLI: `codexpool serve | import … | accounts … | login | usage | bench`.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::{config::Config, import::ImportArgs};

#[derive(Debug, Parser)]
#[command(name = "codexpool", version, about = "Fast multi-account proxy for OpenAI Codex")]
pub struct Cli {
    /// Путь к config.toml (по умолчанию ~/.codexpool/config.toml)
    #[arg(short, long, global = true, env = "CODEXPOOL_CONFIG")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Запустить proxy (:8787) и stats (:8788)
    Serve,
    /// Импорт учёток из OmniRoute / CLIProxyAPI / ~/.codex / codex-switcher
    Import(ImportArgs),
    /// Список / удаление / алиасы / экспорт учёток
    Accounts(AccountsArgs),
    /// OAuth-логин новой учётки (PKCE или --device)
    Login(LoginArgs),
    /// Показать окна квоты по учёткам (из состояния или --live через wham/usage)
    Usage(UsageArgs),
    /// Обёртка над bench/: прогнать корпус против бэкендов и вывести таблицу
    Bench(BenchArgs),
}

#[derive(Debug, Args)]
pub struct AccountsArgs {
    #[command(subcommand)]
    pub action: AccountsAction,
}

#[derive(Debug, Subcommand)]
pub enum AccountsAction {
    List { #[arg(long)] json: bool },
    Remove { alias: String },
    Alias { alias: String, new_alias: String },
    Enable { alias: String },
    Disable { alias: String },
    Priority { alias: String, priority: i32 },
    /// Экспорт в CPA-формат (совместим с CLIProxyAPI и `codex-auth import --cpa`)
    Export { alias: String, #[arg(long)] out: PathBuf, #[arg(long)] cpa: bool },
    /// Снять cooldown вручную
    ResetCooldown { alias: String },
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    #[arg(long)] pub device: bool,
    #[arg(long)] pub alias: Option<String>,
}

#[derive(Debug, Args)]
pub struct UsageArgs {
    #[arg(long)] pub live: bool,
    #[arg(long)] pub json: bool,
}

#[derive(Debug, Args)]
pub struct BenchArgs {
    /// target=base_url пары, например direct=http://…, omniroute=http://localhost:20128/v1, codexpool=http://127.0.0.1:8787/v1
    #[arg(long, required = true)] pub target: Vec<String>,
    #[arg(long)] pub model: String,
    #[arg(long, default_value_t = 5)] pub repeat: u32,
    #[arg(long, default_value_t = 1)] pub concurrency: u32,
    #[arg(long, default_value = "bench/corpus.json")] pub corpus: PathBuf,
    #[arg(long, default_value = "bench/results")] pub out: PathBuf,
}

pub async fn accounts(_cfg: &Config, args: AccountsArgs) -> Result<()> { todo!("open store, apply {:?}", args.action) }
pub async fn usage(_cfg: &Config, args: UsageArgs) -> Result<()> { todo!("table alias | plan | 5h used/reset | weekly used/reset | cooldown | status (live={})", args.live) }
pub async fn bench(_cfg: &Config, args: BenchArgs) -> Result<()> {
    // MVP: shell out в python3 bench/sse_bench.py на каждый target, затем compare.py; нативный клиент — позже.
    todo!("spawn sse_bench.py for {:?} then compare.py", args.target)
}

//! SQLite (WAL). Один writer через Mutex<Connection>; чтения для Stats API — отдельное соединение read-only.
//! Токены: refresh_token шифруется AES-256-GCM ключом CODEXPOOL_MASTER_KEY / ~/.codexpool/master.key (0600).

use std::{collections::HashMap, path::Path};

use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{params, Connection};

use crate::{auth::{AuthStatus, Tokens}, pool::{AccountState, Scope, ScopeState}, stats::{api::Range, RequestEvent}};

pub struct Store {
    w: Mutex<Connection>,
    r: Mutex<Connection>,
    cipher: Option<crate::store::crypto::Cipher>,
}

pub mod crypto {
    //! AES-256-GCM для refresh_token at rest. Ключ — 32 байта hex из env/файла.
    use aes_gcm::{aead::{Aead, KeyInit, OsRng, rand_core::RngCore}, Aes256Gcm, Nonce};
    pub struct Cipher(Aes256Gcm);
    impl Cipher {
        pub fn from_hex(k: &str) -> anyhow::Result<Self> { Ok(Self(Aes256Gcm::new_from_slice(&hex::decode(k)?).map_err(|e| anyhow::anyhow!("{e}"))?)) }
        pub fn seal(&self, plain: &[u8]) -> anyhow::Result<Vec<u8>> {
            let mut nonce = [0u8; 12]; OsRng.fill_bytes(&mut nonce);
            let mut out = nonce.to_vec();
            out.extend(self.0.encrypt(Nonce::from_slice(&nonce), plain).map_err(|e| anyhow::anyhow!("{e}"))?);
            Ok(out)
        }
        pub fn open(&self, blob: &[u8]) -> anyhow::Result<Vec<u8>> {
            anyhow::ensure!(blob.len() > 12, "blob too short");
            self.0.decrypt(Nonce::from_slice(&blob[..12]), &blob[12..]).map_err(|e| anyhow::anyhow!("{e}"))
        }
    }
}

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS accounts (
  id TEXT PRIMARY KEY, alias TEXT UNIQUE NOT NULL, email TEXT, plan TEXT, chatgpt_account_id TEXT, chatgpt_user_id TEXT,
  access_token TEXT, refresh_token_enc BLOB, id_token TEXT, expires_at INTEGER, last_refresh INTEGER,
  auth_status TEXT NOT NULL DEFAULT 'active', priority INTEGER DEFAULT 0, enabled INTEGER DEFAULT 1,
  tags TEXT DEFAULT '[]', source TEXT, created_at INTEGER, updated_at INTEGER);
CREATE TABLE IF NOT EXISTS account_scopes (
  account_id TEXT NOT NULL, scope TEXT NOT NULL, used_5h REAL, reset_5h INTEGER, used_weekly REAL, reset_weekly INTEGER,
  cooldown_until INTEGER, cooldown_reason TEXT, backoff_level INTEGER DEFAULT 0, observed_at INTEGER,
  PRIMARY KEY (account_id, scope));
CREATE TABLE IF NOT EXISTS request_events (
  id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, request_id TEXT NOT NULL, client TEXT, client_app TEXT, client_version TEXT,
  bench_run_id TEXT, endpoint TEXT NOT NULL, model TEXT NOT NULL, upstream_model TEXT, scope TEXT, combo TEXT, step INTEGER,
  account_id TEXT, attempts INTEGER NOT NULL, attempts_json TEXT, status INTEGER, error_class TEXT, error_message TEXT,
  ttft_ms INTEGER, upstream_ttfb_ms INTEGER, route_ms INTEGER, total_ms INTEGER, stream INTEGER,
  input_tokens INTEGER, output_tokens INTEGER, cached_tokens INTEGER, reasoning_tokens INTEGER, tps REAL,
  request_bytes INTEGER, response_bytes INTEGER, session_id TEXT, sticky_hit INTEGER);
CREATE INDEX IF NOT EXISTS ix_events_ts ON request_events(ts);
CREATE INDEX IF NOT EXISTS ix_events_account_ts ON request_events(account_id, ts);
CREATE INDEX IF NOT EXISTS ix_events_bench ON request_events(bench_run_id);
CREATE TABLE IF NOT EXISTS usage_snapshots (
  account_id TEXT NOT NULL, ts INTEGER NOT NULL, scope TEXT, used_5h REAL, used_weekly REAL, reset_5h INTEGER, reset_weekly INTEGER, source TEXT);
CREATE INDEX IF NOT EXISTS ix_usage_account_ts ON usage_snapshots(account_id, ts);
"#;

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let w = Connection::open(path)?;
        w.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;")?;
        let r = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let cipher = std::env::var("CODEXPOOL_MASTER_KEY").ok()
            .or_else(|| std::fs::read_to_string(path.with_file_name("master.key")).ok().map(|s| s.trim().to_string()))
            .map(|k| crypto::Cipher::from_hex(&k)).transpose()?;
        if cipher.is_none() { tracing::warn!("no CODEXPOOL_MASTER_KEY / master.key — refresh tokens stored in plaintext"); }
        Ok(Self { w: Mutex::new(w), r: Mutex::new(r), cipher })
    }

    pub fn migrate(&self) -> Result<()> { self.w.lock().execute_batch(SCHEMA)?; Ok(()) }

    pub fn load_accounts(&self) -> Result<Vec<AccountState>> { todo!("SELECT accounts JOIN account_scopes → AccountState (decrypt refresh_token)") }

    pub fn upsert_account(&self, a: &AccountState, source: &str) -> Result<()> { let _ = (a, source); todo!("INSERT OR REPLACE accounts (seal refresh_token)") }

    /// persist-first для refresh: одна транзакция, refresh_token первым полем.
    pub fn save_tokens(&self, id: &str, t: &Tokens) -> Result<()> {
        let rt: Vec<u8> = match &self.cipher { Some(c) => c.seal(t.refresh_token.as_bytes())?, None => t.refresh_token.as_bytes().to_vec() };
        self.w.lock().execute(
            "UPDATE accounts SET refresh_token_enc=?1, access_token=?2, id_token=?3, expires_at=?4, last_refresh=?5, updated_at=?5 WHERE id=?6",
            params![rt, t.access_token, t.id_token, t.expires_at.timestamp(), chrono::Utc::now().timestamp(), id])?;
        Ok(())
    }

    pub fn save_auth_status(&self, id: &str, s: AuthStatus) -> Result<()> {
        self.w.lock().execute("UPDATE accounts SET auth_status=?1, updated_at=?2 WHERE id=?3", params![serde_json::to_string(&s)?.trim_matches('"'), chrono::Utc::now().timestamp(), id])?;
        Ok(())
    }

    pub fn save_scope(&self, id: &str, scope: Scope, s: &ScopeState) -> Result<()> {
        let sc = serde_json::to_string(&scope)?.trim_matches('"').to_string();
        self.w.lock().execute(
            "INSERT INTO account_scopes(account_id,scope,used_5h,reset_5h,used_weekly,reset_weekly,cooldown_until,cooldown_reason,backoff_level,observed_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(account_id,scope) DO UPDATE SET used_5h=excluded.used_5h,reset_5h=excluded.reset_5h,used_weekly=excluded.used_weekly,
             reset_weekly=excluded.reset_weekly,cooldown_until=excluded.cooldown_until,cooldown_reason=excluded.cooldown_reason,
             backoff_level=excluded.backoff_level,observed_at=excluded.observed_at",
            params![id, sc, s.window_5h.used_percent, s.window_5h.reset_at.map(|t| t.timestamp()), s.window_weekly.used_percent,
                    s.window_weekly.reset_at.map(|t| t.timestamp()), s.cooldown_until.map(|t| t.timestamp()), s.cooldown_reason,
                    s.backoff_level, chrono::Utc::now().timestamp()])?;
        Ok(())
    }

    pub fn insert_events(&self, batch: &[RequestEvent], store_errors: bool) -> Result<()> {
        let mut w = self.w.lock();
        let tx = w.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO request_events(ts,request_id,client,client_app,client_version,bench_run_id,endpoint,model,upstream_model,scope,combo,step,
                 account_id,attempts,attempts_json,status,error_class,error_message,ttft_ms,upstream_ttfb_ms,route_ms,total_ms,stream,
                 input_tokens,output_tokens,cached_tokens,reasoning_tokens,tps,request_bytes,response_bytes,session_id,sticky_hit)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32)")?;
            for e in batch {
                st.execute(params![
                    e.ts.timestamp_millis(), e.request_id, e.client, e.client_app, e.client_version, e.bench_run_id, e.endpoint, e.model, e.upstream_model,
                    serde_json::to_string(&e.scope)?.trim_matches('"'), e.combo, e.step.map(|s| s as i64), e.account_id, e.attempts.len() as i64,
                    if store_errors { Some(serde_json::to_string(&e.attempts)?) } else { None }, e.status, serde_json::to_string(&e.error_class)?.trim_matches('"'),
                    if store_errors { e.error_message.clone() } else { None }, e.ttft_ms, e.upstream_ttfb_ms, e.route_ms, e.total_ms, e.stream as i64,
                    e.input_tokens, e.output_tokens, e.cached_tokens, e.reasoning_tokens, e.tps, e.request_bytes as i64, e.response_bytes as i64, e.session_id, e.sticky_hit as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn purge_events_older_than(&self, days: u32) -> Result<usize> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days as i64)).timestamp_millis();
        Ok(self.w.lock().execute("DELETE FROM request_events WHERE ts < ?1", params![cutoff])?)
    }

    // ---- чтения для Stats API (read-only соединение) ----
    pub fn summary(&self, q: &Range) -> Result<serde_json::Value> { let _ = (&self.r, q); todo!("aggregate: counts, error classes, percentiles via ORDER BY + OFFSET (или sqlite percentile ext)") }
    pub fn timeseries(&self, q: &Range) -> Result<serde_json::Value> { let _ = q; todo!() }
    pub fn accounts_aggregates(&self, q: &Range) -> Result<HashMap<String, serde_json::Value>> { let _ = q; todo!() }
    pub fn account_timeseries(&self, id: &str, q: &Range) -> Result<serde_json::Value> { let _ = (id, q); todo!() }
    pub fn models_aggregates(&self, q: &Range) -> Result<serde_json::Value> { let _ = q; todo!() }
    pub fn recent_errors(&self, q: &Range) -> Result<serde_json::Value> { let _ = q; todo!() }
    pub fn requests(&self, q: &Range) -> Result<serde_json::Value> { let _ = q; todo!() }
    pub fn request_by_id(&self, rid: &str) -> Result<Option<serde_json::Value>> { let _ = rid; todo!() }
    pub fn failovers(&self, q: &Range) -> Result<serde_json::Value> { let _ = q; todo!("WHERE attempts > 1") }
    pub fn bench_run(&self, run: &str) -> Result<serde_json::Value> { let _ = run; todo!("WHERE bench_run_id = ?") }
}

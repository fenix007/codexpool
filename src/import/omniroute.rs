//! Импорт из OmniRoute.
//! БД: `DATA_DIR/storage.sqlite` (прод stable: /app/data/storage.sqlite), таблица `provider_connections` (см. report-omniroute.md §1). Поля access_token/refresh_token/id_token зашифрованы:
//!   формат  `enc:v1:<iv_hex(32) = 16 байт>:<ciphertext_hex>:<authTag_hex(32)>`
//!   ключ    scrypt(secret, salt="omniroute-field-encryption-v1", N=16384, r=8, p=1, len=32)  (src/lib/db/encryption.ts, v3.7.9+)
//!   secret  STORAGE_ENCRYPTION_KEY из env → <dataDir>/.env → ./.env → ~/.hermes/.env
//! Legacy (<v3.7.9): salt = sha256(secret).slice(0,16) — пробуем вторым вариантом, если тег не сходится.

use std::path::Path;

use aes_gcm::{aead::{consts::U16, Aead, KeyInit, Payload}, aes::Aes256, AesGcm, Nonce};

/// OmniRoute шифрует AES-256-GCM с 16-байтным IV (Node `createCipheriv` допускает; J0 выводится через GHASH).
/// Стандартный `Aes256Gcm` принимает только 12-байтный nonce, поэтому нужен тип с U16.
type OmniGcm = AesGcm<Aes256, U16>;
use anyhow::{Context, Result};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::ImportedAccount;

const SALT_V1: &[u8] = b"omniroute-field-encryption-v1";

pub fn from_sqlite(db: &Path, key: Option<&str>, env_file: Option<&Path>) -> Result<Vec<ImportedAccount>> {
    let secret = key.map(str::to_string).or_else(|| env_file.and_then(read_key_from_env_file))
        .or_else(|| db.parent().and_then(|d| read_key_from_env_file(&d.join(".env"))))
        .context("STORAGE_ENCRYPTION_KEY not found: pass --key or --env-file")?;
    let keys = [derive_key(&secret, SALT_V1), derive_key(&secret, &Sha256::digest(secret.as_bytes())[..16])];

    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut st = conn.prepare(
        "SELECT id, email, access_token, refresh_token, id_token, expires_at, provider_specific_data, priority
         FROM provider_connections WHERE provider='codex' AND is_active=1")?;
    let rows = st.query_map([], |r| Ok((
        r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?,
        r.get::<_, Option<String>>(4)?, r.get::<_, Option<String>>(5)?, r.get::<_, Option<String>>(6)?, r.get::<_, Option<i64>>(7)?,
    )))?;

    let mut out = Vec::new();
    for row in rows {
        let (id, email, access, refresh, id_tok, _expires, psd, priority) = row?;
        let dec = |v: Option<String>| -> Result<String> { v.map(|s| decrypt_field(&s, &keys)).transpose()?.context("empty token field") };
        let psd: serde_json::Value = psd.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let account_id = psd.get("chatgptAccountId").or(psd.get("workspaceId")).and_then(|v| v.as_str()).map(str::to_string);
        out.push(ImportedAccount {
            id_token: dec(id_tok)?, access_token: dec(access)?, refresh_token: dec(refresh)?,
            account_id, last_refresh: None, alias: email.clone().map(|e| e.split('@').next().unwrap_or("acct").to_string()),
            email, priority: priority.unwrap_or(0) as i32, source: format!("omniroute:{id}"),
        });
    }
    Ok(out)
}

fn derive_key(secret: &str, salt: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    let params = scrypt::Params::new(14, 8, 1, 32).expect("scrypt params");
    scrypt::scrypt(secret.as_bytes(), salt, &params, &mut key).expect("scrypt");
    key
}

/// `enc:v1:<iv>:<ct>:<tag>` → plaintext. Пробует все ключи (текущая и legacy деривация).
pub fn decrypt_field(value: &str, keys: &[[u8; 32]]) -> Result<String> {
    if !value.starts_with("enc:v1:") { return Ok(value.to_string()); } // незашифрованное значение
    let parts: Vec<&str> = value.split(':').collect();
    anyhow::ensure!(parts.len() == 5, "bad enc:v1 format");
    let iv = hex::decode(parts[2])?;
    anyhow::ensure!(iv.len() == 16, "expected 16-byte IV in enc:v1, got {}", iv.len());
    let mut ct = hex::decode(parts[3])?; let tag = hex::decode(parts[4])?;
    ct.extend_from_slice(&tag); // aes-gcm crate ожидает ct||tag
    for k in keys {
        let c = OmniGcm::new_from_slice(k).map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Ok(p) = c.decrypt(Nonce::<U16>::from_slice(&iv), Payload { msg: &ct, aad: &[] }) { return Ok(String::from_utf8(p)?); }
    }
    anyhow::bail!("decrypt failed with all key derivations (wrong STORAGE_ENCRYPTION_KEY?)")
}

fn read_key_from_env_file(p: &Path) -> Option<String> {
    let s = std::fs::read_to_string(p).ok()?;
    s.lines().find_map(|l| l.trim().strip_prefix("STORAGE_ENCRYPTION_KEY=").map(|v| v.trim_matches('"').trim_matches('\'').to_string()))
}

/// Через API: GET /api/providers?provider=codex → для каждого POST /api/providers/{id}/codex-auth/export (формат ~/.codex/auth.json).
pub async fn from_api(base: &str, token: &str) -> Result<Vec<ImportedAccount>> {
    let _ = (base, token);
    todo!("list codex connections via dashboard API (session cookie/bearer), export each, parse_token_file(.., \"omniroute-api\")")
}

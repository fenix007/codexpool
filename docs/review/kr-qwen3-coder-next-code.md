# Ревью модели kr/qwen3-coder-next

## Chunk 1: Cargo.toml, config.example.toml, main.rs, cli.rs, config.rs
**Сразу оговорка**: из файлов Part 1/6 можно обнаружить **только компиляционные и архитектурные проблемные места**, поскольку логика, конкурентность, безопасность и SQL-схема проверяются по другим файлам (auth, store, pool, router и т.д.), которые пока не предоставлены. Поэтому анализ сосредоточен на **Cargo.toml и config.rs/main.rs/cli.rs** — обнаруженные **синтаксические/API-несовместимости** и **крайне подозрительные архитектурные паттерны**.

---

| № | файл:строки | проблема | категория | серьёзность | как исправить кратко |
|---|-------------|----------|-----------|-------------|----------------------|
| 1 | Cargo.toml:11 | `tls-impersonate = ["wreq"]` — feature включает крейт `wreq`, но `wreq` не имеет `tls-impersonate` как фичу; у `wreq 5` фича называется `tls` и требует `native-tls` или `rustls`. | компиляция | **блокер** | Заменить `["wreq"]` на `["wreq/tls"]` или создать отдельную feature `wreq-tls` с `wreq = { version = "5", features = ["tls"] }`. |
| 2 | Cargo.toml:12 | `wreq = { version = "5", optional = true }` — `wreq` не содержит `tls-impersonate` фичи (это библиотека TLS-клиента, не фича). Крейт `wreq` по умолчанию не включает TLS. | компиляция | **блокер** | Для TLS impersonation нужны `wreq + native-tls` или `wreq + rustls`. Переписать feature: `tls-impersonate = ["wreq/tls"]` и добавить `native-tls` или `rustls-tls` в `wreq`-зависимость. |
| 3 | main.rs:51 | `async { axum::serve(proxy_listener, proxy_app).await ... }` — axum 0.8: `axum::serve(listener, app)` требует `tower::MakeService` + `hyper::Server`, но в axum 0.8 сигнатура `axum::serve(listener, app)` возвращает `Result<Server<L, S>>`. | компиляция | **блокер** | Заменить `axum::serve(proxy_listener, proxy_app)` на `axum::serve(proxy_listener, proxy_app.into_make_service())` (если `State` в роутах) или `axum::serve(proxy_listener, proxy_app).await` без `.map_err(anyhow::Error::from)` — возвращаемое значение не `Result`, а `Server`. |
| 4 | main.rs:51-53 | `tokio::try_join!` обёрнут в `.map_err(anyhow::Error::from)` для `Server::await`, но в axum 0.8 `Server::await` возвращает `Result<!, std::io::Error>`, а `anyhow::Error::from(std::io::Error)` работает, однако `anyhow::Result<()>` не может быть `!`. | компиляция | **блокер** | Убрать `.map_err(...)` — `Server::await` уже возвращает `Result`, а `!` нельзя привести к `anyhow::Error`. |
| 5 | main.rs:20, config.rs:225 | `Config::load` не может компилироваться: `cfg.data_dir = expand_tilde(&cfg.data_dir)` — `PathBuf::from(&Path)` не компилируется; `PathBuf::from(path.as_ref())` тоже не подойдёт. | компиляция | **блокер** | Заменить на `cfg.data_dir = expand_tilde(&cfg.data_dir)` → `cfg.data_dir = expand_tilde(&*cfg.data_dir)` или `let mut cfg = ...; cfg.data_dir = expand_tilde(&cfg.data_dir); Ok(cfg)` (поскольку `cfg` не `&mut`). |
| 6 | config.rs:162 | `#[serde(default)]` + `#[serde(deny_unknown_fields)]` в одном типе противоречивы: `deny_unknown_fields` требует строгую структуру; `#[serde(default)]` вложенных структур допустим, но на `Config` уровне — запрещено. | компиляция (логика) | **важно** | Убрать `#[serde(deny_unknown_fields)]` с `Config`, если используется `#[serde(default)]` для всех полей. В `UpstreamConfig` и остальных — оставить `deny_unknown_fields`. |
| 7 | config.rs:265 | `pub fn resolve_model<'a>(&'a self, model: &'a str) -> &'a str` — аннотация `'a` излишняя и потенциально вызовет ошибки заимствования при `HashMap::get()`. | компиляция (дизайн) | **важно** | Убрать explicit lifetime: `pub fn resolve_model(&self, model: &str) -> &str`. |
| 8 | main.rs:44 | `tokio::spawn(auth::refresh::background_loop(...))` — `background_loop` ожидает `Arc<pool::Pool>`, но `pool::Pool::load` возвращает `Result<Pool>`, а в `main` он обёрнут в `Arc`. Если `Pool` в `load` не `Arc`, то таск ожидает `Arc`, а не `Pool`. | компиляция | **важно** | Убедиться, что `pool::Pool::load` возвращает `Arc<Pool>`, иначе `pool::Pool::load(...).await` или `Arc::new(Pool::load(...))`. |
| 9 | cli.rs:66 | `export cpa` → `todo!("shell out в python3 ...")`. Это не ошибка, но `BenchArgs.corpus: PathBuf` → `bench/corpus.json`, а `corpus.json` в `bench/` может не существовать → runtime-ошибка. | дизайн | **важно** | Добавить `create_dir_all(&args.out)` и валидацию `corpus.json` существует, либо заменить на `std::fs::read_to_string` + error. |
| 10 | config.rs:262 | `#[serde(default)]` на `ComboConfig` и `ComboStep` — но `ComboStep.models` — `Option<StepModels>` с `#[serde(default)]` внутри `ComboStep`. Если `models` отсутствует, `StepModels::default()` не будет применён — `Option` не принимает `#[serde(default)]`. | компиляция | **важно** | Убрать `#[serde(default)]` с `Option<StepModels>`; или добавить `#[serde(default)]` к `StepModels` (но это не поможет `Option`). Лучше: `#[serde(default)]` на `StepModels` (как уже есть), и оставить `models: Option<StepModels>` без `#[serde(default)]`. |
| 11 | Cargo.toml:30 | `rusqlite = { version = "0.32", features = ["bundled", "chrono"] }` — `bundled` + `chrono` требует `libsqlite3-sys = { features = ["chrono"] }`. Возможен конфликт версий при `bundled`. | компиляция | **важно** | Убедиться, что `libsqlite3-sys` версия не конфликтует. Лучше: `rusqlite = { version = "0.32", features = ["chrono"] }` без `bundled`, или использовать `bundled-chrono` (если есть). |
| 12 | config.rs:267 | `HashMap<&str, &str>` с `#[serde(borrow)]` не используется, но в `Config.scope` и `aliases` нет `#[serde(borrow)]`, и `HashMap<String, String>` — правильно. | компиляция | мелочь | Проверить, что `HashMap` без `&str` — уже ок. |

---

### Топ-5 блокеров до первой сборки (Part 1)

| № | Проблема |
|---|----------|
| 1 | `axum::serve(...)` в axum 0.8 требует `.into_make_service()` или не требует `.map_err(...)`. Без этого — ошибка `expected struct 'Server', found 'Result'`. |
| 2 | `wreq/tls-impersonate` — feature `tls-impersonate` не существует в `wreq 5`. Нужна фича `tls` и зависимость `native-tls` или `rustls`. |
| 3 | `Config::load`: `cfg.data_dir = expand_tilde(...)` — `PathBuf::from(&Path)` не компилируется без копирования (`to_path_buf()`). |
| 4 | `#[serde(deny_unknown_fields)]` на `Config` + `#[serde(default)]` на вложенных структурах вызовет `unknown field` ошибки в TOML. |
| 5 | `Config::resolve_model` с `&'a self, &'a str -> &'a str` вызовет "lifetime mismatch" при `HashMap::get`. |

---

### Оценка: правок до `cargo check` (Part 1)

**5–7 правок** (минимальный набор):
1. `wreq` → `wreq = { version = "5", features = ["tls"] }` + фича `tls-impersonate = ["wreq/tls"]` (или `wreq-native-tls`).
2. Убрать `.map_err(...)` из `axum::serve(...).await`.
3. Заменить `cfg.data_dir = expand_tilde(&cfg.data_dir)` → `cfg.data_dir = expand_tilde(&cfg.data_dir); Ok(cfg)` (с `let mut cfg = ...`).
4. Убрать `#[serde(deny_unknown_fields)]` с `Config`.
5. Убрать explicit `'a` из `resolve_model`.
6. Исправить `ComboStep.models` (убрать `#[serde(default)]` с `Option<StepModels>`).
7. Проверить `rusqlite` — убрать `bundled`, если `chrono` конфликтует.

Дальше — только после Part 2/6–6/6.
---
## Chunk 2: src/auth/*
| № | файл:строка(и) | проблема | категория | серьёзность | как исправить кратко |
|---|----------------|----------|-----------|-------------|---------------------|
| 1 | `src/auth/jwt.rs:33` | `super::jwt::decode(&tr.access_token)` → `decode(&tr.access_token)` — рекурсия, бесконечная компиляция/стек | логика | блокер | `let claims = super::jwt::decode(&tr.access_token).ok();` → `let claims = crate::auth::jwt::decode(&tr.access_token).ok();` **или** переименовать `mod jwt` → `mod claims` и `decode` во внешнюю |
| 2 | `src/auth/jwt.rs:42` | `or_else` в цепочке `Option::or_else` — возврат `Option<String>`, но `id` у `Organization` не `&String`, а `String` → тип `Option<&String>` | логика | блокер | `.find(|o| o.is_default).map(|o| o.id.clone())` → `.find(|o| o.is_default).map(|o| o.id.clone())` (уже.clone()), **но** `.first().map(|o| o.id.clone())` — тоже ок. Вопрос в `chatgpt_account_id: Option<String>`. Видимо всё ок. Проверить: `chatgpt_account_id` не `&str`, а `String`. Если да — всё хорошо. Пропустить. |
| 3 | `src/auth/refresh.rs:60` | `let claims = super::jwt::decode(&tr.access_token).ok();` → `decode(&tr.access_token)` → вызов `super::jwt::decode`, т.е. рекурсия в `refresh.rs:60` | логика | блокер | Завести `use crate::auth::jwt;` и `jwt::decode(...)` — чтобы не вызывать тот же `refresh`’овский `decode` |
| 4 | `src/auth/refresh.rs:53` | `id_token: Option<String>` из `TokenResponse` → `tr.id_token.unwrap_or_else(|| current.id_token.clone())` — `id_token` в `Tokens` `String`, а `Option` в `TokenResponse` — но `tr.id_token` может быть `Some("")` → пустой `id_token` | логика | важно | `tr.id_token.filter(|s| !s.is_empty()).unwrap_or_else(|| current.id_token.clone())` |
| 5 | `src/auth/refresh.rs:101` | `tokio::spawn(async move { ... pool.refresh_account(...) })` — `Pool` не `Send`? (см. `Pool` в `mod pool`) | конкурентность | важно | Убедиться, что `Pool: Send + Sync`, иначе использовать `Arc<Mutex<...>>` и `tokio::task::spawn_blocking` или `spawn` только с `Arc<dyn... + Send + Sync>` |
| 6 | `src/auth/refresh.rs:65` | `chrono::DateTime::from_timestamp(e, 0)` — `e` может быть очень большим → `None` → `unwrap_or_else` запускается | логика | мелочь | `e.checked_add(0)` не нужен, просто `e` может быть `i64::MAX`, но `from_timestamp` обрежет. Можно добавить `tracing::debug!` при `None`. Не блокер. |
| 7 | `src/auth/refresh.rs:22-24` | `form(&)` → `form([/* */])` — reqwest 0.12 требует `&[(k, v)]` или `&Form`, а не `&[(&str, &str); N]` | компиляция | блокер | `form([("grant_type", "refresh_token"), ...].as_slice())` или `Form::default().extend([("grant_type", "refresh_token"), ...])` |
| 8 | `src/auth/refresh.rs:30` | `r.status().is_success()` → `r.status().is_success()` OK, но `r.text().await` после `r.json().await` может упасть (тело уже прочитано) — при успехе `r.json().await` съедает тело, и `r.text()` возвратит пустую строку | логика | важно | После `r.json().await` не читать `r.text()`. Сохранить `body` в `let body = r.text().await.unwrap_or_default();` **до** `r.json().await()` → или читать один раз: `let body = r.text().await.unwrap_or_default(); let tr: TokenResponse = serde_json::from_str(&body)...` |
| 9 | `src/auth/refresh.rs:31` | `r.json().await.map_err(...)` — `map_err` вызовет `AuthError::Malformed`, но `e` — `serde_json::Error`, и `e.to_string()` содержит JSON. Хорошо, но лучше `body` в сообщении. | логика | мелочь | `AuthError::Malformed(format!("{}: {}", body, e))` |
| 10 | `src/auth/login.rs:16-17` | `todo!()` в `async fn` — не компилируется, если возвращаемое значение `Result<()>` (нельзя `todo!()` → `Result`) | компиляция | блокер | `todo!("..."); Ok(())` → `todo!("...");` без `Result`, или `unimplemented!()` + `Result` не нужен — но `async fn run(...) -> Result<()>` требует `Result`. Исправить: `unimplemented!("device flow"); Ok(())` |
| 11 | `src/auth/mod.rs:40` | `thiserror::Error` без `#[error(...)]` для `Transient` и `Malformed` — ошибка в `#[error(...)]` — синтаксис OK | компиляция | мелочь | Проверить: `#[error("token endpoint transient error: {0}")]` — верно. Но `Transient(String)` → `{0}` — OK. Все OK. |
| 12 | `src/auth/mod.rs:40` | `#[error(...)]` не использует `source()` для `Transient` — нет `source: Option<E>` → не ошибки `Error::source()` | дизайн | мелочь | Добавить `source` поле: `#[error("token endpoint transient error: {0}")] Transient(String, #[source] anyhow::Error)` или просто оставить. Не блокер. |

**Примечания:**

- **Критическая ошибка — №1 и №3**: `super::jwt::decode` вызывает `refresh.rs`’овский `decode`, если `refresh.rs` импортирует `super::{jwt, ...}`, но `jwt.rs` тоже определяет `pub fn decode`. В `refresh.rs:30` вызывается `super::jwt::decode`, что правильно, но **если в `refresh.rs` есть `use super::jwt;` и вызов `jwt::decode`, то это рекурсия в `refresh.rs:30/60`**. В коде: `use super::{AuthError, Tokens, CLIENT_ID, TOKEN_URL};` — не импортирует `jwt`, значит `super::jwt::decode` правильно. Однако в строке №60: `let claims = super::jwt::decode(&tr.access_token).ok();` — `tr.access_token` вместо `id_token`? Нет, в refresh используется access_token для извлечения exp. OK.

- **Вердикт по №1/3**: В `jwt.rs` определён `pub fn decode`, в `refresh.rs` вызов `super::jwt::decode(...)` — это **корректно**. Тогда №1/3 — **не блокер**.

- **Грубая ошибка — №7**: reqwest 0.12 `Form` требует `&[(AsRef<str>, AsRef<str>)]` — `&[(&str, &str); N]` OK, но `form(&[...])` — передаётся `&[(&str, &str); N]`, а метод ожидает `&Form` или `Form`. В reqwest 0.12: `post(url).form(&[("a", "b")])` — работает. Проверить: в reqwest 0.12 `form` принимает `IntoForm`. `[(&str, &str)]` имплементирует `IntoForm`. Значит, **OK**.

- **Грубая ошибка — №10**: `todo!()` в `async fn -> Result` — `todo!()` возвращает `!`, и `Result<()>` может быть `!`, но компилятор не всегда это видит. Надёжнее `unimplemented!()` + `Ok(())`, либо `todo!(); Ok(())`. В текущем виде — **не блокер**, но плохо.

- **Грубая ошибка — №8**: `r.text().await` после `r.json().await` — тело уже прочитано → `r.text()` вернёт пустую строку, но **не ошибка**, `Ok("")`. Но если `r.json().await` упадёт (например, `401` и `json` не валиден), то `r.text()` вызовется. Но `body` нужен для `401`-ветки. Значит, в текущем коде: если `r.status().is_success()`, то `r.json().await` съел тело, и в блоке `Err` или `4xx` уже не прочитать тело. В `Ok(r)` → `is_success()` — `r.json()` OK → return. В `Ok(r)` → `!is_success()` → читаем `r.text()`. Но `r.json()` не делался в не- success. Значит, `r.json()` вызывается **только** при `is_success()`, и `r.text()` только при `!is_success()` — тело не прочитано. Значит, **OK**. Но в коде:

```rust
let resp = ...send().await;
match resp {
    Ok(r) if r.status().is_success() => {
        let tr: TokenResponse = r.json().await... // съел тело
    }
    Ok(r) => {
        let body = r.text().await... // тело ещё не читали
    }
```

— **OK**. Тело не читается дважды.

---

**Итоговая таблица (после перепроверки — только очевидные блокеры):**

| № | файл:строка(и) | проблема | категория | серьёзность | как исправить кратко |
|---|----------------|----------|-----------|-------------|---------------------|
| 1 | `src/auth/jwt.rs:58` | `chatgpt_account_id.clone()` → `chatgpt_account_id` (если `Option<String>`, то OK), но `.find(...).map(|o| o.id)` вернёт `Option<&String>`, а `Option<String>` ожидается | компиляция | блокер | `.find(|o| o.is_default).map(|o| o.id.clone())` — уже есть. Всё ок. **Проверить, что `id: String`**. Если `&str` — блокер. |
| 2 | `src/auth/login.rs:16-17` | `todo!()` в `async fn -> Result<()>` — компилятор может не свести `!` к `Result<()>` | компиляция | блокер | `unimplemented!(); Ok(())` |

---

**Дополнительные замечания (не блокеры, но важные):**

- `refresh.rs:56-58` — `expires_at` может быть `chrono::Utc::now() + Duration::hours(1)` по умолчанию. Но `expires_in` — `Option<u64>`, и `Duration::seconds(s as i64)` — переполнения нет (u64 → i64), но `s` может быть `u64::MAX` → `i64::MAX`. `chrono::Duration::seconds(i64::MAX)` — undefined behavior (overflow в Rust, но `chrono` проверяет). Возможна `panic`. Лучше `tr.expires_in.map(|s| chrono::Duration::try_seconds(s as i64).unwrap_or(chrono::Duration::hours(1))).and_then(|d| Some(...))` или `min(chrono::Duration::days(365))`.

- `refresh.rs:40`: `tr.refresh_token.unwrap_or_else(|| current.refresh_token.clone())` — `refresh_token` в `TokenResponse` — `Option<String>`, но `refresh_token` в `Tokens` — `String`. Если `tr.refresh_token == None`, оставить старый. OK.

---

**Топ-5 блокеров до первой сборки (в части 2/6):**

1. `src/auth/login.rs:17` — `todo!()` в `async fn -> Result<()>`  
2. (нет других очевидных блокеров — всё компилируется)

---

**Оценка: сколько правок до `cargo check` зелёного (Part 2/6):**  
1–2 правки (только `login.rs` и, возможно, `todo!` → `unimplemented!(); Ok(())`). Остальное — OK.
---
## Chunk 3: src/pool/mod.rs, src/router/mod.rs
| № | файл:строка(и) | проблема | категория | серьёзность | как исправить кратко |
|---|----------------|----------|-----------|-------------|---------------------|
| 1 | `src/pool/mod.rs:153` | `chrono::Duration::from_std(lead)` — `from_std` возвращает `Option<Duration>`, `unwrap_or_default()` не может быть вызван на `Option`. | компиляция | блокер | `chrono::Duration::from_std(lead).ok_or_else(...)?` или `unwrap_or_default()` заменить на `unwrap_or` с default-Duration |
| 2 | `src/pool/mod.rs:154` | `a.tokens.expires_at <= deadline` — `expires_at` (Datetime<Utc>) сравнивается с `deadline` (Duration), несовместимые типы | компиляция | блокер | `a.tokens.expires_at <= (Utc::now() + chrono::Duration::from_std(lead).unwrap_or_default())` |
| 3 | `src/pool/mod.rs:170` | `current.tokens.last_refresh.map_or(false, |t| Utc::now() - t < ...)` — `Utc::now() - t` возвращает `Duration`, но `Duration < Duration` не реализован (нет `PartialOrd`) | компиляция | блокер | `<(Utc::now() - t).num_seconds() as i64 < 30` или использовать `Duration::le()` через `num_seconds()` |
| 4 | `src/pool/mod.rs:221` | `Box::pin(responses(...))` — `responses` — `async fn`, вызов внутри `Box::pin` не может быть асинхронным без `move` или `await` внутри | компиляция | блокер | `Box::pin(async move { responses(...) })` |
| 5 | `src/router/mod.rs:107` | `ready.clone_from_slice(&sorted)` — `sorted: Vec<AccountState>`, `ready: &mut [AccountState]`, но `clone_from_slice` требует `&[T]` и `T: Clone`, `sorted` — `Vec<AccountState>`, `ready` — `&mut [AccountState]`; `clone_from_slice` не работает, если размеры разные. | компиляция | блокер | `*ready = sorted` или `ready.copy_from_slice(&sorted)` (но тогда `sorted` должен быть тем же размером), безопаснее `std::mem::replace(ready, sorted)` или просто `*ready = sorted` |
| 6 | `src/pool/mod.rs:114` | `refresh_locks` возвращается по value, но `or_insert_with` добавляет `Arc<Mutex<_>>`, а `lock().await` удерживает мьютекс **после** `await`, что приведёт к дедлоку (мьютекс держится между `await` в `refresh_account`) | конкурентность | блокер | `let _guard = lock.lock().await;` — **но** тогда внутри блока нельзя делать `await`, т.к. мьютекс не будет автоматически сброшен. Нужно сначала захватить `Arc<Mutex<_>>`, потом лочить, и освободить **до** `await`, или использовать `tokio::sync::MutexGuard::map` — но проще: `let lock = lock.lock().await; let res = ...; drop(lock); res` — неудобно. Лучше: вынести проверку и обновление в отдельную функцию, не держа мьютекс между await. В текущем виде — дедлок. |
| 7 | `src/pool/mod.rs:202` | `pool.observe(...)` внутри цикла, но `observe` вызывает `self.store.save_scope(...)`, а `Store` может блокироваться — `usage_poll_loop` не должен быть `async fn` без `yield`/`interval`, но `loop { tick.tick(); for ... { awaitable } }` — `for` внутри `loop` — допустимо, но `tick.tick().await` в начале — правильно. Проблемы нет, но: `for a in pool.snapshot()` копирует **весь** пул на каждую итерацию — при большом пуле ( Thousands) — высокая нагрузка. | логика | важна | Батчить опрос или использовать `Arc<Pool>` и `Arc<Store>` для лучшей параллелизации, но не критично. |
| 8 | `src/router/mod.rs:97` | `order(..., Strategy::QuotaDeadline)` — `idx.sort_by(...)` — `sort_by` не является стабильной, и может менять порядок элементов с равными ключами, но `ready` потом копируется через `clone_from_slice` — будет дубли/пропуски. | логика | важна | Использовать `sort_by_cached_key` или `sort_by_key`, или явно стабильную сортировку — но в std `Vec::sort_by` не гарантирует стабильность. Лучше использовать `partial_cmp` + `then` с уникальным ключом (alias или id). |
| 9 | `src/router/mod.rs:95` | `ready.remove(pos)` внутри `for` цикла — но `pos` находится из `ready.iter().position(...)`, и `ready` изменяется в цикле — но в этом блоке цикла нет; проблема — `if step.sticky == Sticky::Session`, и если `sticky` — `Some`, то `ready.remove(pos)` меняет индексы, но цикла нет. Однако `sticky_hit` не влияет на `backoff`, `cooldown`, `inflight`, и **не предотвращает повторный выбор** учётки после 401/того же контекста. | логика | важна | Sticky-учётка **не гарантирует**, что она не попадёт в `Cooldown` или `Backoff` — нужно проверять `ready[i].ready(scope, now)` **после** sticky-перемещения. Сейчас: sticky-учётка может быть `cooldown_until > now`, и всё равно пойдёт первой. |
| 10 | `src/router/mod.rs:112` | `expand_accounts(...)` — `for pat in &step.accounts`, `for a in &all`, `if hit && !ids.contains(&a.id)` — `contains` за `O(n)` внутри цикла, общий `O(n*m*k)` — при больших пулах и паттернах — медленно. | логика | мелочь | Использовать `HashSet<AccountId>` вместо `Vec<AccountId>` для `ids`. |
| 11 | `src/pool/mod.rs:65` | `Window::headroom(...)` — `now >= r` не гарантирует, что `now` **значительно** > `r`, может быть `r + 1ns`, и `used_percent` не сбросился. | логика | важна | Добавить `reset_at < now - chrono::Duration::seconds(1)` или просто `now > r` (не `>=`) — но `>=` корректно, если `reset_at` — момент, когда окно **стало** доступно. Проблема в том, что `headroom` должен возвращать `100.0`, только если окно точно сбросилось (не осталось дольше `18000 - 1s`). Тут всё правильно — но `used_percent` может быть старым. Это **не** ошибка, а особенность — `used_percent` не обновляется по времени. |
| 12 | `src/pool/mod.rs:126` | `set_auth_status(id, status)` внутри `refresh_account` — но если `refresh_account` завершится с `Err`, статус останется изменённым (`Refreshing` → `Active`/`NeedsLogin`). Это **правильно**, но `AuthStatus` не инкапсулирует transition — нет валидации (например, `Refreshing` → `NeedsLogin` допустим, а `Active` → `Refreshing`? может, да). | безопасность | мелочь | Нет критичной проблемы, но `AuthStatus` лучше заменить на `enum` с `Transition`-методом или санитайзером. |
| 13 | `src/router/mod.rs:95` | Sticky-учётка может быть **отключена** (`enabled = false`), но `ready_among(...)` уже фильтрует `enabled`, так что `sticky` не будет добавлена, если не готова — всё правильно. | — | — | — |
| 14 | `src/pool/mod.rs:142` | `observe` — обновляет `last_used_at`, но **не** записывает в `Store` сразу, если `obs` пустой — только `s.window_5h`/`window_weekly`, а `last_used_at` всегда обновляется. Но `last_used_at` пишется через `save_scope(...)` — это `INSERT OR REPLACE`, и если `ScopeState` пустой — сохраняется `default()`, и `last_used_at` будет `None` или текущий. Проблема — `last_used_at` обновляется, даже если `obs` пустой. | логика | важна | Удалить `a.last_used_at = Some(...)` из `observe`, если `obs` пустой — или разделить `observe_usage` и `touch`. |
| 15 | `src/router/mod.rs:104` | `out.truncate(self.cfg.failover.max_attempts as usize)` — `max_attempts` может быть 0 или `usize::MAX`, и `as usize` — redundant, но если `i32::MAX` → `usize::MAX` — нормально. | мелочь | мелочь | `as usize` — избыточен, но безопасно. |
| 16 | `src/router/mod.rs:105` | `Candidate { sticky_hit }` — поле `sticky_hit` **никогда не используется** (ни в `proxy`, ни в логике), но оно хранится и выводится в `Debug`. | логика | мелочь | Удалить поле или добавить логику. |
| 17 | `src/pool/mod.rs:210` | `metrics::counter!(..., "result" => ...)` — `metrics 0.24` требует `&str` (не `String`) для labels, `id.to_string()` возвращает `String`. | компиляция | блокер | `"result" => &*format!("{}", result)` или `"result" => "ok"` — `&'static str` не годится, но `metrics` поддерживает `String` в `0.24` — **проверить**. В `metrics 0.24` — `counter!(name, labels...)` — labels — ` IntoIterator<Item = (&'static str, String)>`, так что `id.to_string()` — **ок**, но `"result" => "ok"` требует `&'static str`. Лучше `"result" => "ok".to_string()`. |
| 18 | `src/pool/mod.rs:183` | `refresh_account` — проверка `current.tokens.last_refresh.map_or(false, |t| ...)` — `last_refresh: Option<DateTime<Utc>>`, и сравнение с `30s`, но если `last_refresh` — `Some`, и разница < 30s — возвращаем текущий токен, **но** он мог истечь за это время. | логика | важна | Проверять `current.tokens.expires_at > now + 5m` **и** `last_refresh` < 30s — или просто проверять `expires_at`, и если < 5m — обновлять. |

---

### Топ-5 блокеров до первой сборки (для этой части)

1. **`src/pool/mod.rs:153`** — `from_std(...).unwrap_or_default()` → `unwrap_or(...)` или `ok_or(...)?`  
2. **`src/pool/mod.rs:154`** — `expires_at <= deadline` — сравнение `DateTime<Utc>` и `Duration` → добавить `+ now`  
3. **`src/pool/mod.rs:170`** — `Duration < Duration` → сравнение через `.num_seconds()`  
4. **`src/pool/mod.rs:221`** — `Box::pin(responses(...))` без `async move`  
5. **`src/router/mod.rs:107`** — `clone_from_slice` на `Vec<AccountState>` → `*ready = sorted`  

---

### Оценка: сколько правок до `cargo check` зелёного (Part 3)

**5 правок** — если только компилятор ошибки и не считать логические/конкурентные баги.
---
## Chunk 4: src/store/mod.rs, src/import/*
| № | файл:строка(и) | проблема | категория | серьёзность | как исправить кратко |
|---|---|---|---|---|---|
| 1 | `src/store/crypto.rs:3` | `use aes_gcm::aead::{aead::OsRng, ...}` — `OsRng` импортирован, но используется `rand_core::RngCore`; в `aes-gcm 0.10` `OsRng` из `aead` не экспортируется напрямую | компиляция | блокер | `use rand_core::RngCore;` и убрать `aead::` из `OsRng` |
| 2 | `src/store/crypto.rs:3` / `src/store/mod.rs:10` | `aead::KeyInit` — в `aes-gcm 0.10` интерфейс изменился: `Aes256Gcm::new()` вместо `new_from_slice`, но `new_from_slice` всё ещё существует; **однако** `KeyInit` устарел, используется `aead::Key` и `FromKey`. Если `cargo.toml` — `aes-gcm = "0.10"` без `__crypto` — может не скомпилироваться | компиляция | блокер | Использовать `Aes256Gcm::new_from_slice(k).map_err(...)` — это *ок*, если `aes-gcm` скомпилирован с фичей `__crypto` или `std`; проверить `Cargo.toml` фичи |
| 3 | `src/store/mod.rs:86` | `serde_json::to_string(&s)?.trim_matches('"')` — `trim_matches` возвращает `&str`, но `params!` требует `ToSql`. Нельзя передавать `&str` напрямую — нужно `to_string()` или `&*` | компиляция | блокер | `let sc: String = serde_json::to_string(&scope)?.trim_matches('"').to_string();` и передать `&sc` |
| 4 | `src/store/mod.rs:104` / `src/store/mod.rs:109` | `e.error_message.clone()` — `error_message: Option<String>`, но в `params!` передаётся `e.error_message.clone()` без проверки `None`; в SQL `?` не должен получать `None`, нужен `Option::as_deref().map(|s| s as &dyn ToSql)` или `if store_errors { e.error_message.as_deref() } else { None }` | компиляция | блокер | `if store_errors { Some(e.error_message.as_deref().unwrap_or("")) } else { None }` |
| 5 | `src/import/omniroute.rs:12` / `src/import/omniroute.rs:104` | `type OmniGcm = AesGcm<Aes256, U16>;` — в `aes-gcm 0.10` `AesGcm` не существует; только `Aes256Gcm` (конкретный). `AesGcm` — обобщённый тип, но `Aes256Gcm` — готовый alias; `U16` для `tag_len` не поддерживается напрямую в `0.10` | компиляция | блокер | Убрать `OmniGcm`, использовать `Aes256Gcm`, и передавать IV длиной 12 байт (норма); но NodeJS использует 16 байт IV — тогда нужен кастомный `aead::Aead` или переключиться на `aes-gcm 0.12` или `aes-gcm-simd` + `generic-array` + `aes` вручную. **Реалистично**: заменить на `Aes256Gcm` с 12-байтным IV, а в OmniRoute докерить ручную схему IV → nonce или переписать на `aes-gcm 0.12` |
| 6 | `src/import/omniroute.rs:86` | `scrypt::Params::new(14, 8, 1, 32)` — в `scrypt 0.11` сигнатура `Params::new(log_n: u8, r: u32, p: u32)` без `len`; длина ключа задаётся в вызове `scrypt(..., &mut key)` | компиляция | блокер | `let params = scrypt::Params::new(14, 8, 1)?;` — а `key` уже 32 байта, в `scrypt(..., &params, &mut key)` |
| 7 | `src/import/omniroute.rs:95` | `Nonce::<U16>` — `aes-gcm` 0.10 не поддерживает `U16` в `Nonce`; только `U12` (12 байт) | компиляция | блокер | Использовать `Nonce::from_slice(&iv[..12])`, а IV обрезать до 12 байт. Но Node.js использует 16 байт — тогда нужен `aes-gcm-simd` + `aead::Aead` на `Box<dyn Aead<Nonce = GenericArray<...>>>` или `scrypt` + `cipher` + `block-ciphers` вручную |
| 8 | `src/store/mod.rs:62` | `updated_at=?5` дублируется: `last_refresh INTEGER, last_refresh=?5, updated_at=?5` → `updated_at=?5` дважды — `updated_at=?5` → должно быть `updated_at=?6` | логика | блокер | `params![rt, t.access_token, t.id_token, t.expires_at.timestamp(), chrono::Utc::now().timestamp(), id]` → параметров 6, но в строке `?1..?6`, а `updated_at=?5` → ошибка. Исправить на `updated_at=?6` |
| 9 | `src/store/mod.rs:62` | `WHERE id=?6` — `id` в `accounts` — `TEXT`, но в `params` передаётся `id: &str`, ок. Но если `id` пустой или `None`, SQLite разрешает, но логика сломается. Не баг компиляции, но важная проверка | дизайн | важно | Проверить `!id.is_empty()` перед вызовом |
| 10 | `src/import/omniroute.rs:64` | `dec(id_tok)?` — если `id_tok` пустой `Option::None`, `.map(|s| ...)` вернёт `Ok("")`, но `.context("empty token field")` сработает. Однако `Ok("")` не `Err`, если `dec` не проверяет длину | логика | важно | В `dec` добавить проверку длины после декрипта: `if p.is_empty() { anyhow::bail!("empty token after decrypt") }` |
| 11 | `src/store/mod.rs:73` / `src/store/mod.rs:104` / `src/store/mod.rs:110` | `serde_json::to_string(&e.scope)?.trim_matches('"')` — `scope: Scope`, не `String`, и `Scope: Serialize`. В JSON `Scope` будет `{"name":"...","plan":...}` — строка, но `trim_matches('"')` обрежет `"{"` и `"}"` — получится `"{\"name\":...}"` — это **неправильно**. Должно быть `e.scope` → `serde_json::to_string(&e.scope)?`, без `trim` | логика | блокер | Убрать `.trim_matches('"')` у `e.scope` и `scope` в `account_scopes` |
| 12 | `src/store/mod.rs:110` | `serde_json::to_string(&e.error_class)?.trim_matches('"')` — тоже самое: `error_class: ErrorClass`, сериализуется в строку, `trim` портит. | логика | блокер | Убрать `trim` |
| 13 | `src/import/omniroute.rs:107` | `ct.extend_from_slice(&tag)` — `ct` уже содержит `ciphertext`, и к нему добавляется `tag`. Но в `aes-gcm` `decrypt(Payload { msg, aad })` ожидает `msg = ciphertext || tag`, а `ct` уже содержит `ciphertext`, и `tag` добавлен — правильно. Но если OmniRoute передаёт `enc:v1:<iv>:<ct_hex>:<tag_hex>`, и `ct_hex` — это raw `ciphertext` без `tag`, то всё ок. Проверить документацию OmniRoute: обычно `ciphertext` и `authTag` отдельно. Если `ct` — это raw ciphertext, ок. | дизайн | важно | Добавить комментарий `// ct = ciphertext (unpadded), tag = auth tag; aes-gcm expects ciphertext||tag` |
| 14 | `src/import/omniroute.rs:109` | `for k in keys { ... decrypt ... }` — если первый ключ не подошёл, `if let Ok(p) = ... return Ok(...)`, но `Err` не очищается. В `aes-gcm` `decrypt` возвращает `Result<_, AeadError>`, и если ключ не подошёл, он вернёт `Err`, и цикл продолжится. Это **корректно**, но можно оптимизировать. | дизайн | мелочь | Оставить как есть — логика правильная |
| 15 | `src/store/mod.rs:47` | `r: Mutex<Connection>` — `rusqlite::Connection` не `Send + Sync`, `parking_lot::Mutex` не делает его `Send`. При `SQLITE_OPEN_READ_ONLY` соединение не должно использоваться из нескольких потоков одновременно, но `Mutex` блокирует доступ. **Проблема**: `rusqlite::Connection` не `Send`, `Mutex` не помогает. Нужно `Arc<Mutex<Connection>>`, и `Connection::open_with_flags` возвращает `Connection`, который `!Send`, и его нельзя хранить в `Arc<Mutex<_>>` в `#[tokio::main]`, где `!Send` типы запрещены в `async` контексте. | конкурентность | блокер | Использовать `Arc<parking_lot::Mutex<Connection>>` и запретить `async`await между `lock()` и использованием. Но в `rusqlite` нет `Connection::clone()`. Лучшее решение: использовать `rusqlite::Connection::open_in_memory()` не подходит, т.к. файл. В `tokio` это **не безопасно**. Нужно использовать `rusqlite` в `spawn_blocking` или перейти на `sqlx` с `sqlite`. **Вариант**: создавать новое `Connection` для каждого `query` через `rusqlite::Connection::open(path)` с флагом `SQLITE_OPEN_READ_ONLY` и `SQLITE_OPEN_NO_MUTEX`, но `rusqlite` не поддерживает `SQLITE_OPEN_NO_MUTEX` для WAL. **Реалистично**: удалить `r: Mutex<Connection>` и выполнять SELECT через `spawn_blocking` с новым `Connection` на each запрос, или использовать `sqlx`. |
| 16 | `src/store/mod.rs:52` | `w: Mutex<Connection>` — аналогично, `Connection` не `Send`, `Mutex` не делает `Send`. В `tokio` `Mutex<Connection>` внутри `Arc` — `Connection` всё ещё не `Send`. | конкурентность | блокер | См. №15. |
| 17 | `src/import/omniroute.rs:58` | `psd.get("chatgptAccountId").or(psd.get("workspaceId"))` — `serde_json::Value` может быть `Null`, `.and_then(|v| v.as_str())` вернёт `None`, и `map(str::to_string)` тоже. Но `psd.get(...)` возвращает `Option<&Value>`, `.or(...)` возвращает `Option<&Value>`, `.and_then(|v| v.as_str())` — `Option<&str>`. Это ок. Но если `psd` не `Object`, `get` вернёт `None`, ок. | логика | мелочь | Добавить `map(|s| s.to_string())` — уже есть. Хорошо. |
| 18 | `src/import/omniroute.rs:62` | `alias: email.clone().map(|e| e.split('@').next().unwrap_or("acct").to_string())` — если `email` `Some("")`, `split('@').next()` → `Some("")`, `unwrap_or("acct")` не сработает, будет `""`. `alias` будет пустой строкой, но `alias TEXT UNIQUE NOT NULL`. SQLite отвергнет. | логика | блокер | `alias: email.clone().and_then(|e| if e.is_empty() { None } else { Some(e.split('@').next().unwrap_or("acct").to_string()) })` или `alias: email.clone().map(|e| e.split('@').next().unwrap_or("acct").to_string()).filter(|s| !s.is_empty())` |
| 19 | `src/import/omniroute.rs:91` | `keys = [derive_key(...), derive_key(...)]` — массив, но `derive_key` бросает `panic!` на `scrypt::Params::new` ошибку. Нужно `?` или `expect`. Но `Params::new` для `log_n=14, r=8, p=1` не должно падать. Ок, но лучше `expect("scrypt params")`. | логика | мелочь | `let params = scrypt::Params::new(14, 8, 1).expect("scrypt params valid");` и `scrypt(..., &params, &mut key)` |

---

### Топ-5 блокеров до первой сборки (Part 4)

1. **№5 / №6 / №7**: `aes-gcm 0.10` не поддерживает `U16` в `Nonce` и `AesGcm<Aes256, U16>` — нужно использовать `Aes256Gcm` с 12-байтным nonce. **Решение**: изменить `OmniGcm` на `Aes256Gcm`, и обрезать IV до 12 байт. Или обновить до `aes-gcm 0.12` + `generic-array` + `aead::Key`.

2. **№3**: `trim_matches` в `params!` требует `String`, а не `&str`. Исправить: `let sc: String = ...;` и `&sc`.

3. **№8 / №4**: `updated_at=?5` дважды — параметров 6, но `?6` не используется. Исправить на `?6`.

4. **№15 / №16**: `rusqlite::Connection` не `Send`, `Mutex` не делает `Send` — нельзя использовать `Mutex<Connection>` в `Arc` внутри `tokio::spawn`. **Решение**: отказаться от `r` или использовать `spawn_blocking` + `Connection::open` на each запрос.

5. **№11 / №12**: `trim_matches` для `scope` и `error_class` — портит JSON. Убрать `.trim_matches('"')`.

---

### Оценка: сколько правок до `cargo check` зелёного (Part 4)

6–8 правок:
- 2–3 правки для `aes-gcm` API (`U16` → `U12`, `AesGcm` → `Aes256Gcm`, IV обрезка)
- 1 правка для `trim_matches` + `String` (2 места: `account_scopes.scope`, `request_events.scope`)
- 1 правка для `Params::new` (убрать `32`)
- 1 правка для `updated_at=?6`
- 1 правка для `error_message` (Option::as_deref)
- (опционально) 1–2 правки для `Connection::Send` — но это архитектурное решение. Если оставить `Mutex`, то `Arc<Mutex<...>>` и использовать `spawn_blocking` — тогда `cargo check` пройдёт, но `panic` при `await`.

**Без исправления `Send`/`!Send` для `Connection` — `cargo check` упадёт с ошибкой `Connection` не `Send`**.
---
## Chunk 5: src/upstream/mod.rs, src/proxy/*
Вот ревью Part 5/6. Проверяю по Cargo.toml: axum 0.8, reqwest 0.12, rusqlite 0.32, aes-gcm 0.10, scrypt 0.11, metrics 0.24.

| № | файл:строка(и) | проблема | категория | серьёзность | как исправить кратко |
|---|---|---|---|---|---|
| 1 | `src/upstream/mod.rs:72` | `reqwest::ClientBuilder::use_rustls_tls()` — в reqwest 0.12 метод назван `use_rustls_tls()` **но** фича `rustls-tls` обязательна, иначе `build()` падает. | компиляция | блокер | Добавить `features = ["rustls-tls"]` в Cargo.toml для reqwest (или использовать `native-tls`). |
| 2 | `src/upstream/mod.rs:73` | `pool_idle_timeout(Duration::from_secs(90))` — `90` сек — **слишком мало** для TLS-пула с h2, особенно при нестабильной сети. | дизайн | важно | Увеличить до ≥ 300 сек или `None` (без истечения). |
| 3 | `src/proxy/mod.rs:113` | `Box::pin(responses(State(st), headers, body)).await` — **рекурсивный вызов async fn через Box** — переполнение стека при повторных 401 (например, при зацикливании refresh). | логика | блокер | Заменить на цикл `while let Some(account) = ... { if !refreshed_once { ... } }`, без рекурсии. |
| 4 | `src/proxy/mod.rs:113` | `Box::pin(responses(...)).await` внутри `async fn responses` — **нельзя использовать `Box::pin` на `async fn` без explicit `Send`**, axum 0.8 требует `Pin<Box<dyn Future<...> + Send>>`. | компиляция | блокер | Заменить на итеративный цикл. Пример: `let mut attempts = 0; while attempts < max { attempts += 1; ... if !refreshed_once { ... } }`. |
| 5 | `src/proxy/mod.rs:175` | `st.pool.sticky_set(&session_id, &account.id);` — **внешний cooldown через `st.pool.cooldown(...)` и `sticky_set` после `Streamable`** — если стрим упадёт после коммита, `sticky` останется привязанным к больной учётке. | логика | важно | Отложить `sticky_set` до первого успешного дельта-чанка (после TTFT). |
| 6 | `src/proxy/mod.rs:167` | `tokio::time::timeout(first_byte_timeout, ...).await` — **`first_byte_timeout` для стрима** — если upstream не шлёт ни байта 30 сек, то таймаут, но `inflight_add(-1)` уже выполнен в `resp.bytes_stream()`, и `_ =>`-ветка вызывает `inflight_add(-1)` ещё раз → **отрицательный inflight**. | логика | блокер | Выносить `inflight_add(-1)` из `Ok(None)` и `Err(_)` в `match`, чтобы не дублировать; лучше инкапсулировать в RAII-объект. |
| 7 | `src/proxy/mod.rs:142` | `st.pool.inflight_add(&account.id, scope, -1);` в `Outcome::Unauthorized` до `refresh_account` — **если refresh прошёл, но `responses` рекурсия вызовется снова**, то `inflight_add(-1)` уже на **той же учётке**, но она может уже измениться → утечка/перекос счётчика. | логика | важно | Не уменьшать inflight до фактического завершения (не до коммита). |
| 8 | `src/proxy/mod.rs:190` | `st.pool.cooldown(..., "first_byte_timeout", false)` — **`false` → no backoff**, но при накоплении timeout-ошибок backoff должен применяться. | логика | важно | Включить `true` или использовать `backoff_for_5xx` из конфига. |
| 9 | `src/proxy/mod.rs:205` | `Outcome::RateLimited { .. }` — `st.pool.get(&account.id)` внутри `or_else` **после уже выполненного `inflight_add(-1)`** — если `get` вернёт `None`, `reset_at` будет `None`, и будет fallback на `cooldown_429_default_s`. Но `st.pool.cooldown(...)` уже вызван, и `window_5h.reset_at` может быть устаревшим. | логика | важно | Сохранять `account` перед `inflight_add(-1)` или считать `reset_at` из заголовков/тела **до** классификации. |
| 10 | `src/proxy/mod.rs:225` | `Outcome::ClientError(code)` — `ev.finish(...)` и `stats.try_send(ev)` **до** возврата ответа, но **без `inflight_add(-1)`** — утечка inflight. | логика | блокер | Добавить `st.pool.inflight_add(&account.id, scope, -1);` до `ev.finish(...)`. |
| 11 | `src/proxy/translate/chat.rs:23`, `messages.rs:23` | `todo!()` в handler’ах — **пока не реализовано**, но `Cargo.lock` и `git history` покажут, что это MVP-2, а не баг. | дизайн | мелочь | Добавить `#[allow(dead_code)]` или `unimplemented!()` с FIXME. |
| 12 | `src/proxy/mod.rs:138` | `for k in [...] { if let Some(v) = up_headers.get(k) { out = out.header(k, v); } }` — **перебор только по 6 заголовкам**, но `observe_headers` поддерживает `x-codex-5h-reset-at` и `x-codex-7d-reset-at` в `DateTime<Utc>`, а не в `i64`, так что `HeaderMap::get` вернёт `&str`, но в `Event` нужно `i64`/`DateTime` — при копировании в заголовок ответа клиент получит строку, а не число. | логика | важно | Если цель — проксирование как есть — тогда ок. Если нужно конвертировать — отдельная функция. |
| 13 | `src/upstream/mod.rs:55` | `fn f(h: &HeaderMap, k: &str) -> Option<f32>` — **`h.get(k)?.to_str().ok()?`** — `to_str()` возвращает `Result<&str, FromUtf8Error>`, `.ok()` → `Option<&str>`. Если строка не UTF-8 — `None`, но `x-codex-*` гарантированно ASCII, так что ок. | дизайн | мелочь | Заменить на `to_str().ok()` → `Some(s) if !s.is_empty() else None`, чтобы избежать `parse::<f32>("")`. |
| 14 | `src/upstream/mod.rs:75` | `connect_timeout(Duration::from_secs(10))` — **10 сек слишком долго** для HTTP/2 соединения, обычно 3–5 сек. | дизайн | важно | Уменьшить до 5 сек. |
| 15 | `src/proxy/mod.rs:215` | `Outcome::Streamable` — `ev.committed(...)` и `sticky_set(...)` **до первого дельта-чанка**, но `ttft` измеряется от `t_attempt`, а не от начала запроса. | логика | важно | `ttft` должен быть `Instant::now() - t_attempt` в момент первого дельта. |
| 16 | `src/proxy/mod.rs:243` | `stream_with_accounting` — **пустая заглушка**, возвращает `futures::stream::empty()` — клиент получит пустой стрим, **любой body пропадёт**, даже если `first_chunk` передан. | логика | блокер | Реализовать через `async_stream::try_stream!` или `futures::stream::unfold`. |
| 17 | `src/proxy/mod.rs:236` | `looks_like_error_frame` — **`windows(14)`** — ищет `\"type\":\"error\"` (14 байт), но в `data:`-фрейме может быть `\"type\":\"error\"` без экранирования (JSON внутри SSE). Правильно: `b"\"type\":\"error\""` (14 байт), но проверка в `.windows(14)` будет искать **точно** по байтам, включая пробелы — `\"type\": \"error\"` не пройдёт. | логика | важно | Использовать `if head.contains(b"\"type\":\"error\"")` или `str::from_utf8(head).ok().map(|s| s.contains(r#""type":"error""#))`. |
| 18 | `src/proxy/mod.rs:239` | `rewrite_body` — **`o.insert("stream".into(), true.into())`** — если клиент просил `stream=false`, то после `rewrite_body` он получит `stream=true`, но `upstream_model` может быть другой, и тело будет сломано для не-stream cases. | логика | важно | `stream = env.stream.unwrap_or(false) && cand.upstream_model != model`, или вообще не менять stream, если не нужно. |
| 19 | `src/upstream/mod.rs:67` | `observe_headers` — **`Observed_at: Some(now)`** — `now` фиксируется **на момент вызова**, но `fetch_usage` использует `now` при `DateTime::from_timestamp` — `utc` не гарантирует, что `from_timestamp` вернёт `Some`, если `i64` вне диапазона. | логика | мелочь | Проверять `DateTime::from_timestamp(s, 0).ok().and_then(...)` → `None`, если ошибка. |
| 20 | `src/upstream/mod.rs:81` | `auth_client.timeout(Duration::from_secs(15))` — **без `use_rustls_tls()`** — `auth_client` не использует TLS backend по умолчанию. | компиляция | блокер | Добавить `.use_rustls_tls()` (и фичу `rustls-tls`). |

---

### Топ-5 блокеров до первой сборки

1. **reqwest**: в `Cargo.toml` должен быть `features = ["rustls-tls"]` (иначе `build()` → panic на `use_rustls_tls()`).
2. **reqwest.auth_client**: `timeout()` без `use_rustls_tls()` — тоже поломает `build()`.
3. **`Box::pin(responses(...)).await`**: рекурсивный вызов `async fn` через `Box::pin` — **нельзя**, стек переполнится при повторных 401.
4. **`stream_with_accounting`**: пустой `futures::stream::empty()` — клиент получит пустой стрим (тело теряется).
5. **`inflight_add(-1)` дублируется** на `Err(timeout)` и `Ok(None)`, + отсутствует для `ClientError` — inflight пойдёт в минус.

---

### Оценка: сколько правок до `cargo check` зелёного

Part 5 требует **5 блокеров** (reqwest фича, auth_client TLS, Box::pin, empty stream, inflight дубликат/отсутствие), плюс 2 логики (rewrite_body, looks_like_error_frame) для стабильной работы. Итого: **минимум 5 правок** до `cargo check`, 2–3 дня на полное ревью и тестирование горячего пути.
---
## Chunk 6: src/stats/*
Проверяю только файлы из **Part 6 / src/stats/**. Ищу компиляционные ошибки, логические баги и т.д. — как просили.

---

### Найденные проблемы в `src/stats/mod.rs` и `src/stats/api.rs`

| № | файл:строка(и) | проблема | категория | серьёзность | как исправить кратко |
|---|----------------|---------|-----------|-------------|---------------------|
| 1 | `mod.rs:44` | `ErrorClass` enum сериализуется как строка (`rename_all = "snake_case"`), но в `Attempt.error_class` — `String`, а не `ErrorClass`. При попытке `serde_json::to_value` или сохранении в JSON — мисконверсия/ошибка. | логика | важно | Заменить `error_class: String` → `error_class: ErrorClass` в `Attempt`, или использовать `#[serde(with = "...")]`. |
| 2 | `mod.rs:112` | `self.route_ms = Some((since_start - since_attempt).as_millis() as u64);` — `Duration::sub` не определён. `since_start - since_attempt` — компилятор выдаст ошибку. | компиляция | блокер | Использовать `since_start.saturating_sub(since_attempt)` или `Duration::from_millis` с вычитанием `as_millis()`. |
| 3 | `mod.rs:117` | `if let (Some(out), Some(ttft), Some(total)) = (...) { let gen_ms = total.saturating_sub(ttft).max(1) as f64; ... }` — `total`, `ttft` — `u64`, но `total` может быть `None` в `finish`, и `ttft_ms` может не быть вычислен. При `ttft_ms=None` — `tps=None` корректно, но логика не обрабатывает случай, когда `output_tokens` задан, а `ttft_ms` нет. | логика | важно | Добавить проверку `self.ttft_ms.is_some()` перед вычислением `tps`. |
| 4 | `api.rs:56` | `Path(id)` → `Path(rid)` в `request_one`: переменная переименована, но тип `Path` не меняется. Синтаксис OK, но не согласовано с именами. Не ошибка, но — баг-прone. | дизайн | мелочь | Привести имена в порядок: `Path(rid)` → `Path(request_id)`, или везде `id`. |
| 5 | `api.rs:79` | `st.store.accounts_aggregates(&q)` возвращает `Result<impl Serialize>`, но в коде вызывается `.get(&a.id)`. Предполагается `HashMap<String, ...>` или `HashMap<&str, ...>`. Если `&q.account` не указан — `a.id: String`, а `key` может быть `&str`. Неясно, как реализован `accounts_aggregates`. | логика | важно | Уточнить сигнатуру `store.accounts_aggregates(&q) -> Result<HashMap<String, ...>>`. Использовать `agg.get(&a.id.to_string())` или привести к `&str`. |
| 6 | `api.rs:97` | `guard!(...)` в `stream_sse`, `export`, `metrics` — заглушки. Если `todo!()` выполнится, может привести к панике или утечке ресурсов. Особенно `stream_sse` — без `BroadcastChannel` или `mpsc` → упадёт. | дизайн | важно | Реализовать или убрать `todo!`. Для SSE — `axum::response::Sse::new(...)` с `Receiver`. |
| 7 | `api.rs:42` | `Range` имеет `limit: Option<u32>`, но `store::requests()` и др., скорее всего, ожидают `Option<usize>` или `Option<i64>`. Если в `store` — `rusqlite::types::FromSql` → `i64`, то `u32` может быть слишком большим. | компиляция | важно | Привести тип `limit` → `Option<i64>` (в `rusqlite` по умолчанию) или использовать `try_into()`. |
| 8 | `mod.rs:97` | `detect_client` — не валидирует `client_version` на пустую строку. `"OpenAI/1.0".split('/') → ["OpenAI", "1.0"]`, но если `"/"` нет — `nth(1)` → `None`, а `ver` возвращает `None`. OK. Но `orig: &str` может быть `""`, и `ua: &str` тоже. Не ошибка, но может привести к `""` вместо `None`. | дизайн | мелочь | `.unwrap_or_default()` → `.filter(|s| !s.is_empty()).map(...)` или `then_some(...)`. |

---

### Топ-5 блокеров до первой сборки (только в Part 6)

| № | файл:строка(и) | проблема | как исправить |
|---|----------------|---------|---------------|
| 1 | `mod.rs:112` | `Duration - Duration` не определён | `since_start.saturating_sub(since_attempt)` |
| 2 | `mod.rs:44` | `Attempt.error_class: String` vs `ErrorClass` enum | Заменить тип `String` → `ErrorClass` или сериализовать вручную |
| 3 | `api.rs:97` | `guard!` внутри `todo!()` — упадёт при вызове | Убрать или заменить на пустую реализацию `Json(serde_json::Value::Null)` |
| 4 | `api.rs:79` | `HashMap<&str, &RawValue>`/`.get(&a.id)` — несовместимость ключей | Привести ключи: `agg.get(&a.id.to_string())` или `HashMap<String, _>` |
| 5 | `api.rs:42` | `limit: u32` vs `rusqlite` → `i64` | Заменить на `Option<i64>` |

---

### Оценка: сколько правок до `cargo check` зелёного

Для `src/stats/*` достаточно **3–4 правок** (всё в `mod.rs` + `api.rs:42`).  
Блокеры — `Duration::sub` (1), несоответствие типов `error_class` (1), и несовпадение `limit`/`HashMap` (1–2).  
Остальное — логика/дизайн.

> Всё в Part 6 **скомпилируется после 3–4 целевых правок**.
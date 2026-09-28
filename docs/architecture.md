# codexpool — архитектура

Быстрый локальный/серверный прокси для пула Codex-аккаунтов (ChatGPT OAuth). Один бинарь, один горячий путь, отдельный Stats API.

```
Codex CLI ──/v1/responses──┐
Claude Code ─/v1/messages──┤        ┌───────────────┐   ┌───────────┐
OpenAI SDK ─/v1/chat/compl─┴──▶ Proxy Core ──▶ Router ──▶ │ Account Pool │──▶│ Upstream  │──▶ chatgpt.com/backend-api/codex/responses
                              │  (axum)      (combo,       │ (state,     │   │ (reqwest  │
                              │              failover,     │  windows,   │   │  h2 pool) │
                              │              failback)     │  cooldown)  │   └───────────┘
                              │                            └──────┬──────┘
                              │ events (mpsc)                     │ refresh (per-account mutex)
                              ▼                                   ▼
                        Stats Collector ──▶ SQLite (WAL) ◀── Account Store ◀── Importers (storage.sqlite, CPA, ~/.codex, codex-switcher)
                              │
                              ▼
                        Stats API :8788  (/stats/*, /metrics)   Admin API (/admin/*)   CLI (codexpool ...)
```

## 1. Компоненты

### 1.1 Proxy Core (`proxy/`)
- axum-сервер на `listen` (по умолчанию `127.0.0.1:8787`). Аутентификация клиентов — `Authorization: Bearer <client_key>` из конфига (несколько ключей → метка `client` в статистике).
- Маршруты:
  - `POST /v1/responses` — **passthrough**. Тело читается один раз в `Bytes`; парсится только оболочка `{model, stream, instructions?, prompt_cache_key?}` через `serde_json::from_slice::<Envelope>` с `#[serde(flatten)] rest: RawValue`-подходом, чтобы не пересобирать JSON. Если `stream != true` — форсируем `stream: true` к upstream и собираем финальный объект для клиента (как CLIProxyAPI).
  - `POST /v1/chat/completions` — трансляция в Responses (MVP-2), SSE-ответ обратно в chunk-формат.
  - `POST /v1/messages` — трансляция Anthropic Messages → Responses для Claude Code (MVP-2); `/v1/messages/count_tokens` — локальная оценка.
  - `GET /v1/models` — список из конфига `models` (+ алиасы `auto`, `codex`, `spark`).
- Поток ответа: `reqwest::Response::bytes_stream()` → `axum::body::Body::from_stream()`. Первый чанк upstream **не задерживается** — но перед его пробросом в том же проходе проверяется: если чанк начинается с `event: error` / содержит `"type":"error"` при HTTP 200 — это transient error в теле, стрим считается неначатым, идёт failover (см. 1.3). Это устраняет и peek-loop, и «зависание до первого байта» из OmniRoute: на первый чанк стоит таймаут `first_byte_timeout` (по умолчанию 30 с), на последующие — `idle_timeout` (60 с).
- Заголовки ответа клиенту: `x-codexpool-account` (alias учётки), `x-codexpool-attempts`, `x-codexpool-ttft-ms` (trailer невозможен в SSE — пишем в статистику).

### 1.2 Upstream (`upstream/`)
- `reqwest::Client` один на процесс: `http2_prior_knowledge` нет — протокол согласуется через ALPN; **поддерживает ли chatgpt.com h2 — не проверено** (из sandbox проектирования не измерить, это первый шаг фазы 0 с прод-хоста). reqwest с одним клиентом переиспользует соединения и для HTTP/1.1 (пул keep-alive, один запрос на сокет), так что дизайн работает в обоих случаях; меняется только число сокетов, `pool_max_idle_per_host = 32`, `pool_idle_timeout = 90s`, `tcp_keepalive`, `connect_timeout = 10s`, без общего таймаута на тело (стрим). Один клиент = TLS-сессии и h2-соединения переиспользуются между всеми учётками (у них один host).
- Заголовки (объединение OmniRoute + CLIProxyAPI): `Authorization: Bearer`, `chatgpt-account-id`, `originator` (проброс от клиента, иначе `codex_cli_rs`), `User-Agent` (проброс от клиента, иначе `codex_cli_rs/<pinned>`), `session_id` (из `prompt_cache_key` или заголовка клиента), `OpenAI-Beta`, `x-codex-turn-metadata`, `x-codex-turn-state`, `x-client-request-id`, `Accept: text/event-stream`. Заголовок `Content-Length` пересчитывается только если тело менялось.
- Feature `tls-impersonate` — замена клиента на `wreq` с профилем Chrome/codex. По умолчанию выключено; включаем, если бенчмарк/ошибки 403 покажут необходимость.
- Опционально `transport = "ws"` — `wss://chatgpt.com/backend-api/codex/responses` (как OmniRoute и сам Codex CLI); в MVP — HTTP, WS — как эксперимент в бенчмарке.

### 1.3 Router (`router/`)
**Combo** — именованный упорядоченный список ступеней; каждая ступень — набор учёток + стратегия + фильтр моделей:

```toml
[[combo]]
name = "default"
[[combo.step]]
accounts = ["work-1", "work-2", "work-3"]
strategy = "headroom"        # выбрать учётку с наибольшим остатком 5h-окна
sticky = "session"           # пиннить session_id к учётке пока она здорова
[[combo.step]]
accounts = ["personal"]
strategy = "priority"
[[combo.step]]
accounts = ["api-key-fallback"]  # OPENAI_API_KEY режим — платный, последний
models = { rewrite = { "gpt-5.1-codex" = "gpt-5.1" } }
```

Стратегии внутри ступени: **`quota_deadline` (по умолчанию)** — порт стратегии из stable-форка (`src/sse/services/codexQuotaDeadlineRouting.ts`, коммиты `d5e638b40`, `0f092f480`): weighted-LRU, где вес `min(6, 1 + rate/20)`, а `rate` — максимальная по префиксам (по дедлайну) скорость, с которой нужно «сжигать» недельный остаток и reset credits до их сгорания (`min(reset, subscription_end)`); учётки с исчерпанным 5h-окном исключаются, с неизвестным reset — в конец, неизвестная неделя — медиана весов пула; порядок — по `(now − last_used) × weight`. Отличие от stable: вход — живые окна из заголовков последнего ответа и `wham/usage`, а не только кэш. Остальные: `priority` (порядок списка), `round_robin`, `least_inflight`, `headroom` (max `100 - used_percent` по 5h-окну, tie-break по weekly), `reset_soonest`. Модель клиента → scope (`codex` | `spark`) по таблице в конфиге; у учётки раздельные окна/cooldown на scope.

**Failover** (внутри одного запроса, до первого байта клиенту):
1. Кандидаты = учётки текущей ступени, у которых `ready(scope, model)`: нет cooldown, окно не исчерпано, токен не в состоянии `needs_login`.
2. Попытка → классификация результата:
   - `429` → cooldown = `Retry-After` / `resets_in_seconds` из тела / `x-codex-*-reset-at`, иначе backoff 10 с → 30 мин; пометить окно `used_percent = 100`; следующая учётка.
   - `401` → одиночный refresh (per-account mutex) и повтор на той же учётке; второй 401 → `needs_login`, cooldown 30 мин, следующая.
   - `403` → **без refresh**; cooldown 5 мин, следующая (Cloudflare/geo).
   - `5xx`, `408`, connect/timeout до первого байта → cooldown 60 с, следующая.
   - `400`/`404`/`422` → ошибка клиенту немедленно (проблема запроса, не учётки).
   - Ошибка в теле 200 до первого content-дельта → как 5xx.
   - transport-ошибка до первого байта (connection reset, early EOF, 502/503/504) → **один повтор на той же учётке** через 2–3 с, затем следующая (stable `de2b0cdcc`).
   - `401` с текстом «invalidated oauth token» → сразу `needs_login`, без refresh (stable `015b67465`).
3. Ступени исчерпаны → если ближайший cooldown истекает в пределах `wait_for_cooldown_s` (по умолчанию 30 с), ждать его с перепроверкой дедлайна после пробуждения (stable `1c6a46a27`, off-by-one таймера); иначе клиенту `429` с телом `{error:{type:"pool_exhausted", resets_at: <min reset>}}` и `Retry-After`.
4. Клиент отключился (`client_abort`) — cooldown не ставится (CLIProxyAPI `shouldSkipCredentialCooldown`).
5. Лимит попыток на запрос `max_attempts = 4`, общий бюджет `attempt_budget_ms = 20000`.

**Failback**:
- Cooldown — с явным `until`; при выборе истёкшие снимаются (`promote_expired`), без фонового таймера.
- Окна: при `now >= reset_at` `used_percent := 0` (пассивно), плюс фоновый опрос `wham/usage` раз в `usage_poll_interval` (по умолчанию 5 мин, только для учёток с активностью за сутки) — так учётка возвращается в ротацию сразу после сброса, а `headroom` видит реальные проценты.
- `sticky = "session"` **включён по умолчанию**: карта `session_id → account_id` с TTL 1 ч; при failover перепиннивается. Урок stable: там `codexSessionAffinityTtlMs = 0` по умолчанию, и `quota-deadline` раскидывает ходы одного диалога по учёткам, теряя prompt-cache.
- Опционально `warmup = true` — после сброса 5h-окна один минимальный запрос (как codex-switcher), чтобы окно «открылось» до реального трафика.

### 1.4 Account Pool (`pool/`)
Состояние в памяти (`DashMap<AccountId, AccountState>`), персист в SQLite при изменениях:

```rust
struct AccountState {
    id: AccountId, alias: String, email: String, plan: PlanType,
    chatgpt_account_id: String,          // из id_token → header chatgpt-account-id
    tokens: Tokens { access, refresh, id, expires_at },  // refresh хранится зашифрованно (см. Store)
    auth_status: Active | Refreshing | NeedsLogin,
    scopes: HashMap<Scope, ScopeState { window_5h: Window, window_weekly: Window, cooldown_until: Option<Instant>, backoff_level: u8, inflight: u32 }>,
    last_error: Option<ErrorSummary>, last_used_at, priority,
}
struct Window { used_percent: f32, limit_seconds: u32, reset_at: Option<SystemTime>, observed_at: SystemTime }
```

### 1.5 Auth (`auth/`)
- `jwt::claims(id_token)` → `chatgpt_account_id`, `chatgpt_user_id`, `chatgpt_plan_type`, `email`, `exp` (без верификации подписи — как во всех референсах).
- `refresh(account)`: `POST https://auth.openai.com/oauth/token`, `grant_type=refresh_token&client_id=app_EMoamEEZ73f0CkXaXp7hrann&refresh_token=…`, **без `scope`**. Per-account `tokio::Mutex`; повторный вызов во время in-flight ждёт результат (singleflight). Новый `refresh_token` записывается **до** любой проверки `id_token` (атомарно: tmp + rename + fsync каталога для файлов, транзакция для SQLite с `PRAGMA synchronous=FULL` на время этой записи — при `NORMAL` в WAL последняя транзакция может потеряться при падении ОС, а потерянный одноразовый токен означает перелогин). Ошибки `refresh_token_reused | invalid_grant` → `NeedsLogin`. Проактивный refresh: за `refresh_lead` (по умолчанию 10 мин) до `exp` фоновым таском, не в запросе.
- `login`: PKCE-flow (`authorize` с `codex_cli_simplified_flow=true`, redirect `http://localhost:1455/auth/callback`) и device-flow — заимствуем из `codex-rs/login`. Для MVP достаточно импорта готовых токенов.

### 1.6 Store и Importers (`store/`, `import/`)
- Источник истины — SQLite `codexpool.db` (таблицы `accounts`, `account_scopes`, `request_events`, `usage_snapshots`). Токены шифруются AES-256-GCM ключом из `CODEXPOOL_MASTER_KEY` / файла `~/.codexpool/master.key` (0600); без ключа — хранение в открытом виде с предупреждением (как CLIProxyAPI).
- Экспорт в директорию `auth-dir` в формате CPA (`codex-{hash}-{email}-{plan}.json`) — совместимость с CLIProxyAPI и `codex-auth import --cpa`.
- Импортёры (все сводятся к `ImportedAccount { id_token, access_token, refresh_token, account_id?, last_refresh?, alias? }`):
  - `omniroute --db /app/data/storage.sqlite [--env-file .env]` (файл `DATA_DIR/storage.sqlite`; в прод-образе stable `DATA_DIR=/app/data`, локально `~/.omniroute`) — `provider_connections WHERE provider='codex'`, расшифровка `enc:v1:` (AES-256-GCM, scrypt(secret, "omniroute-field-encryption-v1", 32)), `chatgptAccountId/workspaceId/priority` из JSON.
  - `omniroute-api --url http://localhost:20128 --token …` — через `POST /api/providers/{id}/codex-auth/export` для каждого соединения (если БД недоступна).
  - `codex-file <auth.json>`, `codex-dir ~/.codex/accounts` + `registry.json` (алиасы), `cpa-dir ~/.cli-proxy-api`, `switcher ~/.codex-switcher/accounts.json`.
  - После импорта — пробный refresh каждой учётки (как OmniRoute import) с флагом `--no-verify` для отключения.

### 1.7 Stats Collector (`stats/`)
- На каждый запрос — событие `RequestEvent` (см. `stats-api.md`), отправляется в `mpsc::channel(4096)`; writer-таск пишет батчами по 100 / 200 мс в `request_events` (WAL, `synchronous=NORMAL`). Переполнение канала → дроп с счётчиком `stats_dropped_total`, горячий путь никогда не ждёт.
- Отдельный axum-сервер на `stats_listen` (по умолчанию `127.0.0.1:8788`) — чтобы клиентский порт не открывал аналитику; `/metrics` Prometheus там же.

### 1.8 Admin API и CLI
- `/admin/accounts` (list/get/patch: alias, priority, enabled, cooldown reset), `/admin/accounts/{id}/refresh`, `/admin/accounts/{id}/usage` (живой `wham/usage`), `/admin/combos`, `/admin/reload`. Auth — отдельный `admin_key`.
- CLI: `codexpool serve`, `codexpool import <source> …`, `codexpool accounts list|remove|alias|export`, `codexpool login [--device]`, `codexpool usage [--live]`, `codexpool bench …` (обёртка над `bench/` скриптами или нативный клиент).

## 2. Конфиг (`~/.codexpool/config.toml`)

```toml
listen = "127.0.0.1:8787"
stats_listen = "127.0.0.1:8788"
client_keys = ["sk-local-codex-1"]
admin_key = "…"
data_dir = "~/.codexpool"
default_combo = "default"

[upstream]
transport = "http"            # http | ws (эксперимент)
tls_profile = "native"        # native | chrome (feature tls-impersonate)
first_byte_timeout_ms = 30000
idle_timeout_ms = 60000
pool_max_idle_per_host = 32
default_user_agent = "codex-cli/0.155.0 (Linux; x86_64)"

[failover]
max_attempts = 4
attempt_budget_ms = 20000
cooldown_429_default_s = 60
cooldown_5xx_s = 60
cooldown_403_s = 300
cooldown_needs_login_s = 1800

[refresh]
lead_s = 600
usage_poll_interval_s = 300
warmup = false

[models]
scope = { "gpt-5.1-codex" = "codex", "gpt-5.1-codex-max" = "spark" }
aliases = { auto = "gpt-5.1-codex" }

[[combo]]
name = "default"
[[combo.step]]
accounts = ["*"]              # все учётки с tag != "reserve"
strategy = "headroom"
sticky = "session"
```

## 3. Данные (SQLite)

```sql
CREATE TABLE accounts (
  id TEXT PRIMARY KEY, alias TEXT UNIQUE, email TEXT, plan TEXT, chatgpt_account_id TEXT, chatgpt_user_id TEXT,
  access_token TEXT, refresh_token_enc BLOB, id_token TEXT, expires_at INTEGER, last_refresh INTEGER,
  auth_status TEXT NOT NULL DEFAULT 'active', priority INTEGER DEFAULT 0, enabled INTEGER DEFAULT 1,
  tags TEXT, source TEXT, created_at INTEGER, updated_at INTEGER);
CREATE TABLE account_scopes (
  account_id TEXT, scope TEXT, used_5h REAL, reset_5h INTEGER, used_weekly REAL, reset_weekly INTEGER,
  cooldown_until INTEGER, backoff_level INTEGER DEFAULT 0, observed_at INTEGER, PRIMARY KEY(account_id, scope));
CREATE TABLE request_events (… см. stats-api.md …);
CREATE TABLE usage_snapshots (account_id TEXT, ts INTEGER, scope TEXT, used_5h REAL, used_weekly REAL, reset_5h INTEGER, reset_weekly INTEGER, source TEXT);
```

## 4. Последовательность одного запроса

1. Клиент → `POST /v1/responses` (Bearer client key) → `Envelope` парсится, тело сохраняется как `Bytes`.
2. `Router::plan(model, session_id)` → упорядоченный список кандидатов по ступеням/стратегии, sticky учётка первой.
3. Для кандидата: если `expires_at - now < 60s` → синхронный refresh (редко: фон делает это заранее).
4. `Upstream::send(account, headers, body.clone())` — `Bytes` клонируется без копии.
5. Заголовки ответа: не 200 → классифицировать, обновить состояние, следующий кандидат. 200 → ждать первый чанк ≤ `first_byte_timeout`; ошибка в теле → как 5xx. Иначе — зафиксировать `ttft_ms`, отправить клиенту статус и заголовки, стримить чанки как есть.
6. По завершении: из `response.completed` вытащить `usage` (парсим только последний фрейм), из заголовков — `x-codex-*` окна → `pool.observe(account, scope, windows)`; событие → `stats`.
7. Клиент отключился посередине → `client_abort`, upstream-запрос отменяется (drop future), cooldown не ставится.

## 5. Наблюдаемость и безопасность
- `tracing` с `request_id`, `account`, `attempt`; уровень `info` — одна строка на запрос.
- Prometheus: `codexpool_requests_total{account,model,status,error_class}`, `codexpool_ttft_ms` (histogram), `codexpool_tps`, `codexpool_inflight{account}`, `codexpool_window_used_percent{account,scope,window}`, `codexpool_cooldown{account,scope}`, `stats_dropped_total`.
- Клиентские ключи и admin-ключ — обязательны при `listen` не на loopback. Логи не содержат токенов и тел запросов (тела — только при `debug_dump = true` в отдельные файлы).

## 6. Сосуществование со stable-форком и владение токенами

Архитектура относится к варианту C из ADR-0001 и применяется, только если после фазы 0 и фазы 1 (бенчмарк и патчи stable) сработает порог перехода.

- **Один владелец refresh-токенов.** Токены одноразовые; мьютекс refresh в stable работает только внутри процесса. Перенос — **двухфазный** (замечание внешнего ревью): (1) остановить stable или выключить переносимые соединения (`is_active = 0`); (2) импорт в codexpool **без** refresh (`--no-verify`); (3) первый refresh делает только codexpool, новый токен сохраняется до всего остального; (4) подтверждение. Откат — только через экспорт текущего токена из codexpool (`accounts export --cpa`), потому что старый токен stable после шага 3 недействителен. Флаг `--deactivate-source` выполняет шаг 1 транзакцией в `storage.sqlite`, только когда stable остановлен.
- **Топология.** Клиенты Codex ходят в codexpool напрямую. Stable сохраняет остальные провайдеры; если нужен единый endpoint, в stable заводится OpenAI-совместимое соединение `codex-via-codexpool` на `http://codexpool:8787/v1` — тогда stable логирует вызовы для ai-router как раньше, а codexpool отвечает за пул.
- **Учёт для ai-router.** Если клиенты ходят в codexpool мимо stable, `request_events` нужно отдавать в формате, который понимает `lib/omniroute-call-log-sync.ts` (эндпоинт `/stats/export?format=omniroute-call-log`), — уточнить формат по коду ai-router перед MVP-2.
- **Бенчмарк.** Два пула не могут делить учётки: либо разные учётки у stable и codexpool, либо прогоны по очереди с переносом учёток.

## 7. Вехи
- **Прототип (недели 1–2)**: passthrough и пул без импорта и метрик. **MVP-1 (недели 3–4, оценка скорректирована после внешнего ревью)**: `/v1/responses` passthrough, пул со стратегией `quota_deadline`, failover/failback, `import omniroute` (`storage.sqlite`) `|codex-file|cpa-dir`, события в SQLite, `/stats/summary`, Prometheus. Бенчмарк против omniroute-stable (после его патчей фазы 1) и прямого вызова.
- **MVP-2 (неделя 3)**: `/v1/chat/completions`, `/v1/messages` (Claude Code), sticky-сессии, `wham/usage` опрос, Admin API, `login`.
- **MVP-3**: warmup, WS-транспорт как опция, tls-impersonate, экспорт в CPA, systemd/launchd юниты, Docker.

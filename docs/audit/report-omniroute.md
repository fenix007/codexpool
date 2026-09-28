# Аудит OmniRoute — путь Codex

Ревизия: release/v3.8.51 (HEAD 30a83ee8, 22.09.2026). Код: `/agent/workspace/codexpool-audit/diegosouzapw_OmniRoute`. Ключевые утверждения (WS-транспорт, peek-loop, await записи в SQLite на горячем пути) перепроверены grep'ом по коду.

> **Исправление от 23.09.2026.** Раньше здесь было сказано, что у форка `fenix007/omniroute` нет собственных правок, — это неверно: я смотрел ветку `main`, которая только отслеживает upstream. Продакшен пользователя — ветка **`stable`**: upstream v3.8.48 + 104 коммита (30 бэкпортов, 56 собственных, 18 служебных), 25 релизов `3.8.48-fork.N`. См. `report-fork-ledger.md` (журнал форка) и `report-stable-hotpath.md` (горячий путь Codex в stable — **это основной отчёт для решений**). Ниже — аудит upstream v3.8.51 с исправлениями по пунктам 2 и 4, где первоначальные формулировки оказались неточными.

## 1. Хранение учёток Codex

SQLite, таблица `provider_connections` (`src/lib/db/migrations/001_initial_schema.sql`):

```sql
CREATE TABLE provider_connections (
  id TEXT PRIMARY KEY,
  provider TEXT NOT NULL,        -- 'codex'
  auth_type TEXT,                -- 'oauth'
  email TEXT,
  access_token TEXT,             -- enc:v1:<iv>:<ct>:<tag>
  refresh_token TEXT,            -- enc:v1:...
  id_token TEXT,                 -- enc:v1:...
  expires_at TEXT, token_expires_at TEXT,
  provider_specific_data TEXT,   -- JSON
  priority INTEGER DEFAULT 0, is_active INTEGER DEFAULT 1,
  backoff_level INTEGER DEFAULT 0, rate_limited_until TEXT, ...
);
```

`provider_specific_data`: `{"chatgptAccountId","workspaceId","chatgptPlanType","codex_child_cooldowns":{"codex":..,"spark":..}}`.

Шифрование (`src/lib/db/encryption.ts`): AES-256-GCM, формат `enc:v1:<iv_hex32>:<ct_hex>:<tag_hex32>`, ключ `scryptSync(secret, "omniroute-field-encryption-v1", 32)` (статичная соль с v3.7.9). Secret: env `STORAGE_ENCRYPTION_KEY` → `<dataDir>/.env` → `./.env` → `~/.hermes/.env`. До v3.7.9 соль была динамической (`sha256(secret).slice(0,16)`) — ломало расшифровку и давало CPU-спайки; есть автомиграция.

Экспорт `POST /api/providers/{id}/codex-auth/export` (`src/lib/oauth/utils/codexAuthFile.ts`) отдаёт файл, совместимый с `~/.codex/auth.json` (`auth_mode`, `OPENAI_API_KEY: null`, `tokens.{id_token,access_token,refresh_token,account_id}`, `last_refresh`). Импорт `POST /api/oauth/codex/import` принимает плоский и вложенный форматы и делает пробный refresh перед сохранением.

**Импорт в codexpool** — *исправлено 23.09:* файл БД называется `storage.sqlite` и лежит в `DATA_DIR` (в прод-образе stable `DATA_DIR=/app/data`, по умолчанию `~/.omniroute`), а не `omniroute.db`. Два пути: (a) через API экспорта работающего OmniRoute, по одному соединению; (b) напрямую из `storage.sqlite`: `SELECT ... FROM provider_connections WHERE provider='codex' AND is_active=1` + расшифровка AES-256-GCM тем же `STORAGE_ENCRYPTION_KEY`. Путь (б) быстрее для десятков учёток и не требует живого сервера.

## 2. Вызов upstream и refresh

**Транспорт** — *исправлено 23.09*. По умолчанию HTTP POST `https://chatgpt.com/backend-api/codex/responses` + SSE (`open-sse/config/providers/registry/codex/index.ts:17`). WebSocket `wss://…` (`codex.ts:145`, через `wreq-js`) — только opt-in: `isCodexResponsesWebSocketRequired()` (`codex.ts:437-451`) возвращает true лишь при `codexTransport === "websocket"` у соединения; в коде прямо сказано, что WS за HTTP-прокси ломается (403 на upgrade). Первоначальное утверждение «новое WS-соединение на каждый запрос» относится только к этому opt-in режиму. **Реальная проблема транспорта** — HTTP-диспетчер undici: 32 агента с `connections: 1, pipelining: 0` и round-robin (`open-sse/utils/proxyDispatcher.ts:140-165`); `pipelining: 0` в undici отключает keep-alive, поэтому почти каждый запрос платит TCP+TLS-handshake. Это намеренный фикс #4580 против очереди SSE-стримов за одним сокетом — подробно в `report-stable-hotpath.md`, P1.

**Заголовки** (`codex.ts:1150-1167`, `open-sse/config/codexIdentity.ts`): `Authorization: Bearer`, `chatgpt-account-id` (= `workspaceId`), `originator: codex-cli-rs`, `session_id` (UUID, prompt-cache affinity), `User-Agent: codex-cli/<ver> (...)` — версия пробрасывается из UA клиента (бэкенд гейтит модели по версии), `x-codex-installation-id`, `x-codex-turn-metadata`.

**Refresh** (`open-sse/services/tokenRefresh/providers/codex.ts`): `POST https://auth.openai.com/oauth/token`, `grant_type=refresh_token`, **без `scope`** — комментарий в коде: со scope Auth0 трактует запрос как re-scope и инвалидирует семейство refresh-токенов других аккаунтов. Lead 5 мин до истечения. Сериализация (`tokenRefresh.ts:186-573`): `Map<connectionId, Promise>` — конкурентные вызовы ждут один in-flight; fallback-ключ `"codex:"+sha256(refreshToken)`; персист внутри мьютекса + CAS-guard.

**Ошибки**: `refresh_token_reused | invalid_grant | token_expired` и 401 от token endpoint → `unrecoverable_refresh_error` (нужен re-login). 429 upstream → сброс quota-cache, cooldown в `providerSpecificData`. Provider-level circuit breaker: 5 ошибок → 30 мин. *Исправлено 23.09:* утверждение «только in-memory» не подтвердилось — в stable (унаследовано из v3.8.48) состояние breaker персистится в SQLite (`domain_circuit_breakers`), cooldown аккаунтов тоже; реальная проблема обратная — лишние записи на каждый успешный запрос.

**Квота**: заголовки `x-codex-5h-usage|limit|reset-at`, `x-codex-7d-*` → `CodexQuotaSnapshot` (`open-sse/executors/codex/quota.ts`); cooldown планируется по ближайшему reset.

## 3. Combo / fallback / failback

- Каждое соединение → 3 виртуальных аккаунта: parent, child `codex`, child `spark` (раздельные квоты, cooldown per-scope; `open-sse/services/codexAccount/index.ts`).
- Стратегии для одинаковых учёток: `priority`, `quota-weighted` (PR #12789 — взвешенный выбор по остатку квоты и in-flight, пиннинг существующих разговоров к аккаунту), остальные 16 — общие для мультипровайдера и на однородный пул Codex почти не влияют.
- Fallback на другой аккаунт — только **до первого байта**. После commit — `streamRecovery.ts` (mid-stream continuation с prefilled context; работает не для всех клиентов).
- **Peek loop** (`open-sse/executors/codex/bodyTimeout.ts`, `peekCodexSseTransientError()` в `codex.ts`): читает первые байты SSE-тела 200-ответа, чтобы поймать transient error внутри тела, и только потом коммитит поток клиенту → добавляет к TTFT время до первого upstream-чанка.
- Failback: автоматический — когда истёк `rate_limited_until`; health-probe для Codex нет.

## 4. Горячий путь (один стриминговый `/v1/responses`)

| # | Шаг | Стоимость / проблема |
|---|---|---|
| 1 | Next.js App Router middleware (auth, CSRF, routing) | 0.5–3 мс |
| 2 | `handleChat()` (`src/sse/handlers/chat.ts`) — резолв combo | кэш, дёшево |
| 3 | `request.json()` — **полная буферизация и парсинг тела** | O(размер тела); на 100–300 KB контекстах ощутимо |
| 4 | `sanitizeChatRequestBody()` | мутация объекта |
| 5 | Резолв соединений, `getAccessToken()` | возможный сетевой refresh 100–500 мс под мьютексом |
| 6 | Трансляция форматов (Chat→Responses) | ещё один stringify+parse |
| 7 | RTK/Caveman компрессия | worker-thread pool (раньше блокировала event loop) |
| 8 | Сборка identity-заголовков | дёшево |
| 9 | **HTTP без keep-alive** (undici `pipelining: 0`, 32 агента round-robin); WS через wreq — только opt-in | +10–60 мс напрямую, +100–400 мс через прокси на запрос |
| 10 | **Peek loop** до первого чанка | + время до первого upstream-байта к TTFT |
| 11 | **`await persistCodexChildQuotaResponse()`** (`chatCore.ts:3240`) — запись квоты в SQLite **до** проброса тела | 1–5 мс синхронно в hot path |
| 12 | Passthrough тела (`buildCodexTimeoutSafePassthroughBody`) | честный passthrough, без per-chunk parse |
| 13 | `safeLogEvents`, `saveCallLog`, `saveRequestUsage` | fire-and-forget, не блокируют |

Итог: overhead OmniRoute на TTFT складывается из (9)+(10)+(11)+(3)+(1); TPS практически не страдает (passthrough).

## 5. Статистика

`usage_history`: `tokens_input/output/cache_read/cache_creation/reasoning`, `service_tier`, `status`, `success`, `latency_ms`, `ttft_ms`, `error_code`, `timestamp`. `call_logs`: provider, model, connection_id, duration, combo_name, combo_step_id, source/target format, error_summary, опционально артефакты запроса/ответа. TTFT измеряется (`chatCore.ts:5518, 5561, 5871, 5992`), tokens/s — заголовок `X-OmniRoute-Tokens-Per-Second` и `usage.tokens_per_second`. Записи асинхронные. Отдельного API статистики по аккаунтам нет — только дашборд и внутренние роуты.

## 6. Известные проблемы

Открытые issues (поиск `codex` в репозитории, сентябрь 2026):

| # | Статус | Суть |
|---|---|---|
| #12996 | open | Responses streaming теряет MCP namespace после первого tool call → Codex: `unsupported call` |
| #14330 | open | Синтезированные keepalive/failure-фреймы на `/v1/responses` невалидны — клиенты падают |
| #14486 | open | chatgpt-web-codex cdp-proxy fail-open без `CDP_PROXY_TOKEN` |
| #14154 | closed | Responses translator не выставлял `encrypted_function_args: []` → sub-agents без задачи |
| #14298 | closed | OAuth device-flow: неверный poll route, 404/405 |
| #13933 | closed | `codex/gpt-5.6-{sol,terra,luna}` числились image-моделями, терялся context_length |

Повторяющиеся баги по CHANGELOG: регрессия деривации ключа шифрования (v3.7.9, decrypt failures + CPU 50%); зависшие стримы на peek loop (200 без первого байта висел до 15 мин → per-read таймаут, #8020); `structuredClone`-спам в логгере (~9800 лишних клонов на стрим, PR #12241); Responses Lite вырезал `parallel_tool_calls` (#7821); инвалидация семейства refresh-токенов Auth0 (смягчено, не устранено); квоты codex/spark не разделялись (#1129).

## 7. Выводы

**Импортировать**: строки `provider_connections` (SQLite + AES-GCM) либо файлы экспорта; refresh без `scope`; набор identity-заголовков с проброской версии клиента; пары окон 5h/7d из `x-codex-*`.

**Сделать лучше**:

| Проблема OmniRoute | codexpool |
|---|---|
| HTTP без keep-alive (фикс #4580) | Persistent HTTP/2 к chatgpt.com: мультиплексирование даёт и переиспользование соединения, и независимость стримов |
| Peek loop до первого байта | Пробрасывать первый чанк немедленно; ошибку внутри 200-тела ловить в том же проходе (это одно и то же чтение — не нужно ждать «на всякий случай») и, если клиент ещё ничего не получил, переключать аккаунт |
| `await` записи в SQLite в hot path | События в канал → батч-писатель в отдельном потоке |
| Полный parse тела | Для `/v1/responses` от Codex CLI — passthrough байтов с минимальным парсом (model, stream); полный парс только при трансляции формата |
| Next.js + 26k строк универсального роутера | Один бинарь, один hot path, нет провайдерной абстракции |
| 3–6 синхронных записей SQLite на запрос (breaker, lastUsedAt, квота) | Состояние в памяти, персист батчами в фоне; запись breaker только при смене состояния |
| Нет API статистики | Отдельный Stats API с per-request событиями, per-account окнами, TTFT/TPS |

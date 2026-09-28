# Аудит codex-auth (loongphy) и codex-switcher (Lampese)

Источники: `/agent/workspace/codexpool-audit/loongphy_codex-auth` (JS + Zig), `/agent/workspace/codexpool-audit/Lampese_codex-switcher` (Tauri, Rust в `src-tauri`). Оба — управляющие утилиты, не прокси: переключают аккаунт перезаписью `~/.codex/auth.json`.

## 1. Формат `~/.codex/auth.json`

Режим ChatGPT OAuth (`auth/auth.zig:27-37`, `types.rs:317-341`):

```json
{
  "auth_mode": "chatgpt",
  "OPENAI_API_KEY": null,
  "tokens": {
    "id_token": "<JWT>",
    "access_token": "<JWT>",
    "refresh_token": "<opaque>",
    "account_id": "org-XXXXXXXXXX"
  },
  "last_refresh": "2026-09-22T10:00:00Z"
}
```

Режим API-ключа — только `OPENAI_API_KEY`.

JWT-клеймы `id_token` (namespace `https://api.openai.com/auth`; `auth.zig:147-191`, `types.rs:291-308`):

| клейм | смысл |
|---|---|
| `chatgpt_account_id` | ID ChatGPT-аккаунта — идёт в заголовок `chatgpt-account-id` |
| `chatgpt_user_id` / `user_id` | ID пользователя |
| `chatgpt_plan_type` | `free`, `go`, `plus`, `prolite`, `pro`, `business`, `enterprise`, `edu` |
| `chatgpt_subscription_active_until` | RFC3339 |
| `organizations[].id`, `organizations[].is_default` | fallback для организационных аккаунтов |
| корневой `email` | email пользователя |

Ключ записи в codex-auth: `chatgpt_user_id::chatgpt_account_id` (`auth.zig:55-61`).

## 2. Реестр codex-auth и CPA-формат

```
~/.codex/
├── auth.json                    ← активный аккаунт
├── registry.json                ← реестр, schema v4 (min v2)
├── accounts/<record_key>/auth.json
└── sessions/<session_id>/rollout-<N>.jsonl
```

`AccountRecord` (`common.zig:93-107`): `account_key`, `chatgpt_account_id`, `chatgpt_user_id`, `email`, `alias`, `account_name`, `plan`, `auth_mode`, `created_at`, `last_used_at`, `last_usage` (RateLimitSnapshot), `last_local_rollout`.

codex-switcher хранит `~/.codex-switcher/accounts.json` (`AccountsStore` v1): массив `StoredAccount` (UUID id, имя, email, plan_type, `auth_data` с type-тегом `chat_g_p_t` / `api_key`), `active_account_id`, `masked_account_ids`.

CPA-формат (совместим с CLIProxyAPI; `auth.zig:39-45`) — плоский:

```json
{ "id_token": "...", "access_token": "...", "refresh_token": "...", "account_id": "...", "last_refresh": "..." }
```

`import --cpa` → `convertCpaAuthJson` (`auth.zig:246-276`): account_id извлекается из JWT, если пуст; добавляются `auth_mode: "chatgpt"`, `OPENAI_API_KEY: null`. `export --cpa` → `convertStandardAuthJsonToCpa` (`auth.zig:278-308`): flatten `tokens.*`.

## 3. HTTP-эндпоинты OpenAI / ChatGPT

### Refresh (`token_refresh.rs:16, 289-295`)
```
POST https://auth.openai.com/oauth/token
Content-Type: application/x-www-form-urlencoded
grant_type=refresh_token&refresh_token=<url-encoded>&client_id=app_EMoamEEZ73f0CkXaXp7hrann
→ { "access_token", "id_token", "refresh_token" }   (id_token и refresh_token опциональны)
```
3 попытки, backoff 250/500 мс, timeout 10 с.

### Usage (`usage.zig:6`, `api/usage.rs:353-358`)
```
GET https://chatgpt.com/backend-api/wham/usage
Authorization: Bearer <access_token>
chatgpt-account-id: <id>
User-Agent: Mozilla/5.0 ... Chrome/136.0.0.0 ...   (browser-like, против Cloudflare)
Origin/Referer: https://chatgpt.com, sec-fetch-*: empty/cors/same-origin
```
```json
{
  "plan_type": "plus",
  "rate_limit": {
    "primary_window":   { "used_percent": 27.5, "limit_window_seconds": 18000,  "reset_at": 1760100000 },
    "secondary_window": { "used_percent": 42.0, "limit_window_seconds": 604800, "reset_at": 1760600000 }
  },
  "credits": { "has_credits": true, "unlimited": false, "balance": "10.50" },
  "rate_limit_reset_credits": { "available_count": 2 }
}
```

### Прочие
- `GET https://chatgpt.com/backend-api/accounts` → `{ items: [{id, name}] }` (`account.zig:4`)
- `GET https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27` → `{ accounts: { <id>: { account: {plan_type}, entitlement: {expires_at} } } }` (`api/usage.rs:22`)
- `GET https://chatgpt.com/backend-api/wham/profiles/me` (UA `codex-cli/1.0.0`) → lifetime/daily статистика (`account_stats.rs:13`)
- `GET https://chatgpt.com/backend-api/wham/rate-limit-reset-credits` с `openai-beta: codex-1`, `originator: Codex Desktop` (`account_stats.rs:14-15, 260-278`)

### Warm-up (`api/usage.rs:23, 377-393`)
```
POST https://chatgpt.com/backend-api/codex/responses
{ "model": "gpt-5.6-luna", "instructions": "You are Codex.",
  "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":"Thanks"}]}],
  "tools": [], "reasoning": {"effort":"low"}, "store": false, "stream": true }
```
Ответ — SSE. Используется для «прогрева» 5-часового окна и CDN-сессии.

## 4. Безопасная ротация токенов (codex-switcher, Rust)

- Глобальный мьютекс `AUTH_OPERATION_LOCK: tokio::sync::Mutex<()>` (`auth/mod.rs:9-10`) сериализует все рефреши и переключения.
- Перед рефрешем — reconciliation (`token_refresh.rs:86-101`, `storage.rs:13-65`): читается live `auth.json`, если `chatgpt_account_id` совпадает и токены отличаются — Codex CLI сам ротировал, свежие токены копируются в хранилище. Иначе переключение «назад» вернёт мёртвый снимок.
- Refresh-токены одноразовые (комментарий `token_refresh.rs:131`): «Persist a rotated replacement before reporting an unusable ID token». `merge_refresh_response` (`token_refresh.rs:256-274`) сохраняет новый refresh_token первым, даже если id_token битый.
- 401 → рефреш и один повтор. **403 = Cloudflare challenge, рефреш не делать**: «refreshing on 403 burns the refresh token and causes a refresh_token_reused error» (`api/usage.rs:149-153`).
- Детект запущенного Codex (`commands/process.rs:444-545`): `ps -axo pid=,tty=,command=` на Unix, PowerShell `Get-CimInstance Win32_Process` на Windows; IDE-плагины и `codex app-server` игнорируются.

## 5. Семантика rate-limit

| Окно | `limit_window_seconds` | константа |
|---|---|---|
| Сессионное (5 ч) | 18000 | `SESSION_WINDOW_SECONDS` |
| Недельное | 604800 | `WEEKLY_WINDOW_SECONDS` |

Бэкенд иногда меняет primary/secondary местами — codex-switcher нормализует по `limit_window_seconds` (`api/usage.rs:506-526`). `remaining = 100 - used_percent`; при `now >= reset_at` окно считается сброшенным.

Offline-источник (`session.zig:49-77`) — `~/.codex/sessions/*/rollout-*.jsonl`, строки:
```json
{ "timestamp": "...", "type": "event_msg",
  "payload": { "type": "token_count",
    "rate_limits": { "primary": {"used_percent": 27.5, "window_minutes": 300, "resets_at": 1760100000},
                     "secondary": {"used_percent": 42.0, "window_minutes": 10080, "resets_at": 1760600000},
                     "credits": {...}, "plan_type": "plus" } } }
```
Читаются с конца через mmap, кэш 15 с; часто `rate_limits: null` — данные отстают на часы.

## 6. Авто-переключение при 429

В обоих репозиториях реализации перехвата 429 нет (форк `codext` — отдельный пакет). Переключение — перезапись `auth.json` до старта CLI. Это подтверждает нишу codexpool: единственный способ бесшовного переключения без перезапуска клиента — прокси.

## 7. Выводы для codexpool

1. Импортёры: `~/.codex/auth.json`, `~/.codex/accounts/*/auth.json` + `registry.json` (codex-auth), `~/.codex-switcher/accounts.json`, плоский CPA JSON — все сводятся к одной внутренней записи `{id_token, access_token, refresh_token, account_id, last_refresh}` + клеймы из JWT.
2. Refresh: единый мьютекс на аккаунт, атомарная запись (tmp + rename), новый refresh_token сохраняем **до** любой валидации; на 403 не рефрешим; upfront-refresh за N минут до `exp` из access_token.
3. Квоты: два окна (18000 / 604800 с) из `wham/usage` + пассивно из заголовков/тела 429; нормализация окон по длительности, а не по имени.
4. Warm-up окна — опциональная фича планировщика (как в codex-switcher), пригодится для «failback на основную учётку после сброса».

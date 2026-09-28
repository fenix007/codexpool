# Аудит CLIProxyAPI — Codex/OpenAI-OAuth путь

> Ревизия: сентябрь 2026. Версия тега в репо: v7.3.12.
> Цель: документировать эталонную архитектуру для создания быстрого пула аккаунтов Codex.

---

## 1. Формат токен-файла (AUTH FILE FORMAT)

### Структура JSON

Файл: `internal/auth/codex/token.go:19-40`

```go
type CodexTokenStorage struct {
    IDToken      string         `json:"id_token"`
    AccessToken  string         `json:"access_token"`
    RefreshToken string         `json:"refresh_token"`
    AccountID    string         `json:"account_id"`
    LastRefresh  string         `json:"last_refresh"`
    Email        string         `json:"email"`
    Type         string         `json:"type"`          // всегда "codex"
    Expire       string         `json:"expired"`       // RFC3339
    Metadata     map[string]any `json:"-"`             // не сериализуется напрямую
}
```

Пример итогового JSON (Metadata расплющивается через MergeMetadata):

```json
{
  "id_token": "eyJ...",
  "access_token": "eyJ...",
  "refresh_token": "eyJ...",
  "account_id": "user-chatgpt-XXXX",
  "last_refresh": "2026-09-22T10:00:00Z",
  "email": "user@example.com",
  "type": "codex",
  "expired": "2026-09-23T10:00:00Z"
}
```

### Расположение директории и именование файлов

- **Директория по умолчанию**: `~/.cli-proxy-api` (`internal/config/config_defaults.go:6`)
- **Поле в YAML-конфиге**: `auth-dir` (`internal/config/config.go:35`)
- **Алгоритм именования** (`internal/auth/codex/filename.go:12-31`):
  - С хэшем аккаунта и планом: `codex-{hashAccountID}-{email}-{plan}.json`
  - Без плана: `codex-{hashAccountID}-{email}.json`
  - Легаси без хэша: `codex-{email}-{plan}.json` / `codex-{email}.json`
  - Флаг `includeProviderPrefix=true` добавляет `codex-` в начало

### Загрузка и мониторинг файлов

- `github.com/fsnotify/fsnotify` (`internal/watcher/watcher.go:12, 47`)
- `Watcher` слушает `authDir` и `configPath`
- При изменении файла — дедупликация по SHA-хэшу (`lastAuthHashes map[string]string`), затем диспетчеризация через `dispatchCond` (`watcher.go:46,60`)
- Снимки в памяти: `currentAuths map[string]*coreauth.Auth`

### Хранение срока действия

Поле `expired` (RFC3339), записывается как:
```go
time.Now().Add(time.Duration(tokenResp.ExpiresIn) * time.Second).Format(time.RFC3339)
```
(`openai_auth.go:175, 279`).

---

## 2. OAuth-логин и рефреш токена

### Endpoints и константы

```go
// internal/auth/codex/openai_auth.go:25-29
AuthURL             = "https://auth.openai.com/oauth/authorize"
TokenURL            = "https://auth.openai.com/oauth/token"
ClientID            = "app_EMoamEEZ73f0CkXaXp7hrann"
RedirectURI         = "http://localhost:1455/auth/callback"
codexRefreshTimeout = 30 * time.Second
```

### PKCE (S256) Authorization Code Flow

Параметры GET к `AuthURL` (`openai_auth.go:72-83`):
```
client_id=app_EMoamEEZ73f0CkXaXp7hrann
response_type=code
redirect_uri=http://localhost:1455/auth/callback
scope=openid email profile offline_access
state={random}
code_challenge={SHA256(code_verifier), base64url}
code_challenge_method=S256
prompt=login
id_token_add_organizations=true
codex_cli_simplified_flow=true
```

Обмен кода (`openai_auth.go:108-114`):
```
POST https://auth.openai.com/oauth/token
Content-Type: application/x-www-form-urlencoded

grant_type=authorization_code&client_id=app_EMoamEEZ73f0CkXaXp7hrann
&code={code}&redirect_uri=...&code_verifier={verifier}
```

Device-auth flow: `sdk/auth/codex_device.go` — при `LoginOptions.DeviceFlow=true`.

### Рефреш токена

```
POST https://auth.openai.com/oauth/token
Content-Type: application/x-www-form-urlencoded

grant_type=refresh_token&client_id=app_EMoamEEZ73f0CkXaXp7hrann
&refresh_token={token}&scope=openid profile email
```
(`openai_auth.go:214-219`)

### Упреждающий рефреш (pre-expiry margin)

`sdk/auth/codex.go:34-36`:
```go
func (a *CodexAuthenticator) RefreshLead() *time.Duration {
    return new(24 * time.Hour)
}
```
**RefreshLead = 24 часа** — рефреш начинается за 24 часа до истечения токена (`conductor_refresh.go:164-168`).

Фоновый цикл: каждые `refreshCheckInterval = 5 * time.Second` (`conductor_refresh.go:26`).

### Сериализация конкурентных рефрешей

`singleflight.Group` (`openai_auth.go:39, 198-210`):
```go
var codexRefreshGroup singleflight.Group

result, err, _ := codexRefreshGroup.Do(refreshToken, func() (interface{}, error) {
    refreshCtx, cancelRefresh := context.WithTimeout(..., codexRefreshTimeout)
    defer cancelRefresh()
    return o.refreshTokensSingleFlight(refreshCtx, refreshToken)
})
```
Ключ дедупликации — сам `refreshToken`.

### Атомарность сохранения ротированного refresh_token

`SaveTokenToFile` (`token.go:57-84`) использует `os.Create` (перезапись без `rename`) — **риск частичной записи при сбое питания**. Ротированный `refresh_token` сохраняется немедленно после успеха рефреша.

---

## 3. Upstream-запрос к Codex

### URL

```go
// internal/runtime/executor/codex_executor_execute.go:33, 79
baseURL = "https://chatgpt.com/backend-api/codex"
url = strings.TrimSuffix(baseURL, "/") + "/responses"
// Итог: POST https://chatgpt.com/backend-api/codex/responses
```

Live/Realtime: `https://chatgpt.com/backend-api/codex/realtime/calls` (`live.go:31`).

### Заголовки upstream-запроса

`applyCodexHeadersFromSources` (`codex_executor_request.go:321-369`):

```
Authorization:              Bearer {access_token}
Content-Type:               application/json
User-Agent:                 codex-tui/0.154.0 (Mac OS 26.5.2; arm64) iTerm.app/3.6.11 (codex-tui; 0.154.0)
Originator:                 codex-tui  (или прокидывается от клиента)
Chatgpt-Account-Id:         {account_id из JWT claims}  — только для OAuth, не API-key
Session-Id:                 {prompt_cache_key / uuid}
OpenAI-Beta:                {прокидывается от клиента}
Accept:                     text/event-stream (stream) / application/json
Connection:                 Keep-Alive
X-Codex-Turn-Metadata:      {прокидывается}
X-Codex-Turn-State:         {прокидывается}
X-Client-Request-Id:        {прокидывается}
Thread-Id:                  {прокидывается}
X-Codex-Window-Id:          {прокидывается}
X-OpenAI-Internal-Codex-Responses-Lite: {прокидывается}
```

### Источник account_id

JWT `id_token` парсится без верификации подписи (`jwt_parser.go:58`):
```go
func (c *JWTClaims) GetAccountID() string {
    return c.CodexAuthInfo.ChatgptAccountID  // поле: https://api.openai.com/auth.chatgpt_account_id
}
```
В runtime: `auth.Metadata["account_id"]` (`codex_executor_request.go:358-362`).

### Трансформация тела запроса

`codex_executor_execute.go:38-79` (упрощённо):
1. `sdktranslator.TranslateRequest(from, "codex", model, payload, stream)` — перевод из OpenAI/Claude в Codex Responses API
2. `stream: true` — принудительно (`SetBoolIfDifferent`)
3. Удаляются поля: `previous_response_id`, `generate`, `prompt_cache_retention`, `safety_identifier`, `stream_options`
4. `instructions: ""` инжектируется если отсутствует
5. `store: false` в шаблоне image-generation запроса (`codex_openai_images.go:884`)
6. Инструмент `image_generation` добавляется если не отключён конфигом

### SSE-проксирование

`responsesSSEFramer` (`sdk/api/handlers/openai/openai_responses_handlers.go:51`):
- Буферизует чанки в `pending []byte` до полного SSE-фрейма (граница `\n\n` / `\r\n\r\n`)
- Не построчная обработка — ждёт полный фрейм
- Фильтрует приватные события `codex.*`, `responsesapi.*`
- `Flusher.Flush()` после каждого фрейма

### Трансляция форматов в Codex Responses API

| Входной формат клиента | Путь |
|---|---|
| `POST /v1/chat/completions` (OpenAI) | `FormatOpenAI → FormatCodex` |
| `POST /v1/messages` (Claude) | `FormatClaude → FormatCodex` |
| `POST /v1/responses` (Responses API) | `FormatOpenAIResponse → FormatCodex` |

---

## 4. Маршрутизация по аккаунтам (MULTI-ACCOUNT ROUTING)

### Алгоритм выбора

`sdk/cliproxy/auth/scheduler.go:19-25` — три стратегии:
```go
schedulerStrategyRoundRobin         schedulerStrategy = 1  // по умолчанию
schedulerStrategyFillFirst          schedulerStrategy = 2
schedulerStrategyWeightedRoundRobin schedulerStrategy = 3
```

**Round-Robin** (`scheduler.go:1647-1665`):
```go
func (v *readyView) pickRoundRobin(predicate func(*scheduledAuth) bool) *scheduledAuth {
    start := scheduledSuccessorIndex(v.flat, v.lastPicked)  // следующий после lastPicked
    for offset := 0; offset < len(v.flat); offset++ {
        index := (start + offset) % len(v.flat)
        entry := v.flat[index]
        if predicate != nil && !predicate(entry) { continue }
        v.lastPicked = entry.auth.ID
        return entry
    }
    return nil
}
```
Записи в `flat` отсортированы по `auth.ID` (детерминированный порядок). Приоритет аккаунтов учитывается (`priorityOrder`).

### Маркировка 429/quota-ошибок

`conductor_cooldown.go:MarkResult` (~строка 869):

```go
case 429:
    if result.RetryAfter != nil {
        cooldown := *result.RetryAfter
        if cooldown < minQuotaCooldownFloor { cooldown = minQuotaCooldownFloor }
        next = now.Add(cooldown).Round(0)
    } else {
        next, backoffLevel = quotaCooldownAfterFailure(state.Quota, now)
        // exponential: quotaBackoffBase << backoffLevel, max quotaBackoffMax
    }
    state.NextRetryAfter = next
```

При `result.CredentialScope=true` — охлаждаются все модели аккаунта (`auth.Quota.Reason = "credential_quota"`).

Cooldown-таблица:
- **401/402/403** → 30 мин
- **404** → 12 часов
- **429** → Retry-After || exponential [1s .. 30min]
- **408/500/502/503/504/520-526** → 1 минута

Константы (`conductor_refresh.go:35-38`):
```go
minQuotaCooldownFloor  = 10 * time.Second
transientErrorCooldown = time.Minute
quotaBackoffBase       = time.Second
quotaBackoffMax        = 30 * time.Minute
```

x-codex-* заголовки квоты парсятся в `quota_signals.go:154-163`.

### Retry и failover

- Context cancel / WebSocket close / EOF → не охлаждают (`shouldSkipCredentialCooldown`)
- Retry на следующем аккаунте: **только до отправки первого байта клиенту**
- `promoteExpiredLocked` автоматически разблокирует аккаунты с истёкшим `nextRetryAt` при каждом выборе

### Per-model routing

Каждый аккаунт имеет `supportedModelSet` из registry. Шардирование `authScheduler` по `(provider, model)`. Аккаунт выбирается только при поддержке запрошенной модели.

---

## 5. Обработка ошибок

| Сценарий | Поведение |
|---|---|
| context.Canceled | `isConnectionLifecycleError` → не охлаждает |
| WebSocket close 1000/1001/1006 | lifecycle error → не охлаждает |
| io.EOF / io.ErrUnexpectedEOF | transient transport → не охлаждает |
| DNS timeout / net timeout | transient transport → не охлаждает |
| HTTP 429 | cooldown с backoff |
| HTTP 401-403 | cooldown 30 мин |
| HTTP 404 | cooldown 12 ч (model_not_found) |
| HTTP 5xx/52x | cooldown 1 мин |
| `isNonRetryableRefreshErr` | строка "refresh_token_reused" → без retry |

Таймаут рефреша: 30 сек (`codexRefreshTimeout`). Таймаут OAuth callback: 5 мин (`sdk/auth/codex.go:113`).

---

## 6. Management API и статистика использования

### Endpoint'ы `/v0/management/`

Требуется `remote-management.secret-key` в конфиге. Неполный список:
```
GET      /auth-files                 — список аккаунтов с cooldown/status
GET      /auth-files/models          — поддерживаемые модели по аккаунту
GET      /auth-files/download        — скачать JSON-файл
POST     /auth-files                 — загрузить новый файл аккаунта
DELETE   /auth-files                 — удалить
PATCH    /auth-files/status          — включить/отключить/cooldown
PATCH    /auth-files/fields          — изменить label, proxy_url и т.д.
POST     /auth-files/refresh         — принудительный рефреш токена
GET/PUT  /config.yaml                — YAML конфигурация
GET      /api-key-usage              — использование по API-ключам
POST     /oauth-callback             — OAuth redirect endpoint
GET      /plugins, POST /plugins/*   — управление плагинами
```

### Per-request данные и CPA Usage Keeper

После v6.10 встроенный сборщик удалён. Данные через:
1. **Hook** `auth.Hook.OnResult(ctx, Result)` (`conductor_cooldown.go:1006`) — вызывается после каждого запроса
2. **Plugin SDK** (`sdk/cliproxy/usage/accounting.go`) — `UsageAccounting`
3. CPA-Manager-Plus/CPA-Usage-Keeper подключаются как SDK-плагины

Поля `Result`: `AuthID`, `Model`, `Provider`, `Success`, `RetryAfter`, `CredentialScope`, `Error`, `RouteModel`, `SkipQuotaObservation`.

---

## 7. Производительность (hot path)

### HTTP transport

`helps/utls_client.go` — **uTLS** для маскировки TLS fingerprint под Chrome при запросах к `chatgpt.com`. Новый `http.Client` создаётся **per-request** — потенциальный overhead (нет connection pool reuse).

### Аллокации

- `handlers.ReadRequestBody` → `io.ReadAll` → полная буферизация тела в `[]byte`
- `responsesSSEFramer.pending` → `append` на каждый чанк (аллокации при росте)
- `gjson.GetBytes` + `sjson.SetBytes` → аллоцируют новый `[]byte` при каждой мутации
- `scheduler.mu.Lock()` единственный мьютекс на весь `authScheduler` — hotspot при N > 100 аккаунтов

### Middleware chain (Gin)

Recovery → RequestLogging → ManagementAvailability → Handler.

### Потенциально медленное

1. Новый `utlsHTTPClient` per request
2. Двойная gjson/sjson трансформация тела при compat-режиме
3. Глобальный мьютекс `scheduler.mu` при выборе аккаунта
4. `responsesSSEFramer` без `sync.Pool` — аллоцирует на каждый запрос

---

## 8. Статистика кодовой базы

| Параметр | Значение |
|---|---|
| Go version | 1.26.0 (`go.mod:3`) |
| HTTP framework | `gin-gonic/gin v1.10.1` (не fasthttp) |
| TLS | `refraction-networking/utls v1.8.2` (uTLS/Chrome fingerprint) |
| JSON | `tidwall/gjson v1.18.0` + `tidwall/sjson v1.2.5` |
| File watcher | `fsnotify/fsnotify v1.9.0` |
| Concurrency | `golang.org/x/sync` (singleflight) |
| WebSocket | `gorilla/websocket v1.5.3` |
| Лицензия | MIT (Luis Pater / Router-For.ME) |
| Строк в Codex-executor (prod, без тестов) | ~2500 |
| Строк в scheduler (prod) | ~1732 |
| Строк в conductor_cooldown (prod) | ~2297 |

---

## Сводная таблица ключевых констант

| Константа | Значение | Файл:строка |
|---|---|---|
| `ClientID` | `app_EMoamEEZ73f0CkXaXp7hrann` | `openai_auth.go:27` |
| `AuthURL` | `https://auth.openai.com/oauth/authorize` | `openai_auth.go:25` |
| `TokenURL` | `https://auth.openai.com/oauth/token` | `openai_auth.go:26` |
| RefreshLead | 24 часа | `sdk/auth/codex.go:35` |
| `refreshCheckInterval` | 5 секунд | `conductor_refresh.go:26` |
| `codexRefreshTimeout` | 30 секунд | `openai_auth.go:29` |
| `minQuotaCooldownFloor` | 10 секунд | `conductor_refresh.go:37` |
| `transientErrorCooldown` | 1 минута | `conductor_refresh.go:38` |
| `quotaBackoffMax` | 30 минут | `conductor_refresh.go:36` |
| Upstream URL | `POST https://chatgpt.com/backend-api/codex/responses` | `codex_executor_execute.go:33,79` |
| `codexUserAgent` | `codex-tui/0.154.0 (Mac OS 26.5.2; arm64)...` | `codex_executor_request.go:26` |
| DefaultAuthDir | `~/.cli-proxy-api` | `config_defaults.go:6` |

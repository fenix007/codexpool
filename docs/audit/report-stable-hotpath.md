# Горячий путь Codex в stable-форке fenix007/OmniRoute

Ветка `stable`, HEAD `0a5f7f423` (22.09.2026); база upstream v3.8.48 `7ee5bbc64`. Версии: next 16.3.4, undici 8.10.2, wreq-js 2.3.1. SQLite в режиме WAL с `synchronous=NORMAL` (`src/lib/db/core.ts:1120-1128`). Пути даны относительно корня репозитория. Ключевые утверждения (раздел 2: P1, P3, P10, affinity) перепроверены вручную 23.09.2026.

## 1. Цепочка вызовов `/v1/responses`

### 1.1 HTTP (по умолчанию: POST + SSE)

| # | Слой | Файл:строка | Что происходит |
|---|---|---|---|
| 0 | Node HTTP-сервер | `scripts/dev/standalone-server-ws.mjs:165-200` (в образе `server-ws.mjs`) | патч `createServer`, отвод WS-upgrade в WS-прокси, запуск `runRuntimeDiagnostics()` |
| 1 | Next 16 standalone router | — | App Router, конвертация в Web `Request` |
| 2 | Next middleware | `src/proxy.ts:4` → `src/server/authz/pipeline.ts:251` | классификация маршрута, настройки, `validateApiKey` (кэш по хэшу) |
| 3 | Route handler | `src/app/api/v1/responses/route.ts:111` | `withInjectionGuard(postHandler)` |
| 4 | Injection guard | `src/middleware/promptInjectionGuard.ts:66-67` | `request.clone().json()` — единственный полный `JSON.parse` тела; скан 16 KB + PII |
| 5 | postHandler | `route.ts:78-108` | `withCodexPreferredModel` (при переписывании модели — stringify и новый Request), `withEarlyStreamKeepalive(handleChat)` |
| 6 | Keepalive | `open-sse/utils/earlyStreamKeepalive.ts:235-279` | гонка обработчика с таймером 4000 мс |
| 7 | `handleChat` | `src/sse/handlers/chat.ts:189` | валидация; `structuredClone(body)` (`:313-315`); политика ключа; guardrails pre-call (второй проход); hooks; роутинг; `getComboForModel` |
| 7a | Combo | `chat.ts:775` → `open-sse/services/combo.ts:693` → `combo/targetTimeoutRunner.ts` | |
| 8 | `handleSingleModelChat` | `chat.ts:965` | модель, pipeline gates, цикл по аккаунтам (`:1220-1851`) |
| 9 | Выбор аккаунта | `chat.ts:1234` → `src/sse/services/auth.ts:1742` | некэшированные `getSettings()` и `getProviderConnections()` с расшифровкой каждой строки; стратегия / affinity; `await updateProviderConnection(lastUsedAt)`; опционально preflight — **синхронный** сетевой запрос к usage API при выборе аккаунта, если кэш этого аккаунта старше 60 с (`CACHE_TTL_MS = 60_000`, `codexQuotaFetcher.ts:44, 218-247`; не путать с 5-минутным кэшем, который читает `quota-deadline`) |
| 10 | Токен | `open-sse/services/tokenRefresh.ts:1021` | проактивный refresh, мьютекс только внутри процесса (`:85,1738-1782`) |
| 11 | Dispatch | `chat.ts:1402` → `chatDispatch.ts:37` → `chatHelpers.ts:376` | `breaker.execute(runWithProxyContext(handleChatCore))` |
| 12 | `handleChatCore` | `open-sse/handlers/chatCore.ts:367` | idempotency, rate limit, логгер, semantic cache, memory, компрессия, `prepareUpstreamBody`, семафор, `executor.execute` (`:2420`) |
| 13 | CodexExecutor | `open-sse/executors/codex.ts:792` | WS не требуется (`:448`) → `BaseExecutor.execute`: заголовки, `transformRequest`, `JSON.stringify` → `fetch` |
| 14 | Транспорт | `open-sse/utils/proxyFetch.ts:413` | undici `getDefaultDispatcher` / proxy-dispatcher / wreq-сессия при `ENABLE_TLS_FINGERPRINT` |
| 15 | Peek | `codex.ts:818-857, 649-724` | чтение до первого `output_text.delta` / `completed` или 8192 символов |
| 16 | Ответ | `chatCore.ts:2498` (429-ротация), `:3100-3265` (401/refresh), **`:3271 await persistCodexQuotaState`**, `:4290 ensureStreamReadiness` (второй peek) | |
| 17 | Стрим | `stream.ts:1140-1520` (`JSON.parse` каждого `data:`), `streamingPipeline.ts:51-100` (PII → progress → heartbeat → model-echo) | |
| 18–19 | Возврат и конец | `chat.ts:1443`, `:1460 breaker._onSuccess()`; `onStreamComplete` → `saveRequestUsage` (асинхронно) | |

На критическом пути около 20 модулей; горячие файлы: `chatCore.ts` 4639 строк, `stream.ts` 2760, `auth.ts` 2418, `chat.ts` 1852, `codex.ts` 1483. До первого байта upstream выполняется: один `JSON.parse`, одна-две полные копии тела, один `JSON.stringify`, 10–15 синхронных операций SQLite, 0 или 1 новое TCP+TLS-соединение.

### 1.2 WS-путь (сделан в форке и уже «тонкий»)

`scripts/dev/responses-ws-proxy.mjs` на путях `/v1/responses`, `/responses`, `/api/v1/responses`. Первый `response.create` вызывает `POST /api/internal/codex-responses-ws` с `action:"prepare"` (auth, policy, выбор аккаунта с preflight, refresh, transformRequest). Затем **один upstream WS через wreq на всю клиентскую сессию** (`:651`), переиспользуется на каждом ходе (`:702-708`). Нет chatCore, peek, failover и записей в SQLite на каждом ходе. Коммиты: `ed0819032` (fidelity CLIProxyAPI, санитайзер ID, проброс `x-codex-*`), `8fc1b924b` (снятие token limits на каждом ходе), `bd2f872d7` (`OpenAI-Beta: responses_websockets=2026-02-06`), `0187e6954` (упаковка в Docker). По сути это готовый прототип выделенного Codex-пути внутри форка.

## 2. Проверка проблем P1–P10 (ранее найдены в upstream)

| # | Статус в stable | Доказательства |
|---|---|---|
| P1 | **Формулировка была неверной, проблема реальная.** WS не используется по умолчанию ни в stable, ни в upstream. Реальная проблема: HTTP-диспетчер отключает keep-alive | `proxyDispatcher.ts:148-162`, `proxyDispatcherCache.ts:24-36`, `codex.ts:448-462` |
| P2 | **Присутствует, ограничен** (бэкпорты `a9a3b0106`, `a0b4b8745`, `70cfe1c91`, `35287b5a6`); второй peek — `streamReadiness.ts:379` | `codex.ts:604, 649-724, 828-856` |
| P3 | **Присутствует** как `persistCodexQuotaState` (унаследован из базы, форк не трогал) | `chatCore.ts:3271`, `providers.ts:553-596` |
| P4 | **Присутствует** | `promptInjectionGuard.ts:66-67`, `chat.ts:313-315`, `stream.ts:1195-1230` |
| P5 | **Присутствует** для HTTP; WS-путь идёт мимо Next | `src/proxy.ts`, `route.ts` |
| P6 | **Присутствует; для Responses неустранимо в принципе** | `chat.ts:1457-1849`, `streamRecovery.ts:242-254` |
| P7 | **Неверно для stable**: breaker и cooldown персистятся в SQLite. Реальная проблема — лишние записи | `circuitBreaker.ts:173-222, 265-267, 351` |
| P8 | **Присутствует**: ~20 модулей, три системы отказа, пять видов повторов | раздел 1 |
| P9 | **Та же схема**, фикс v3.7.9 есть | `encryption.ts:28-217` |
| P10 | **Присутствует: у стримов `ttft_ms` = `latency_ms`** | `usageHistory.ts:680-686`, `stream.ts:109` |

**P1 — транспорт.** Диспетчер (унаследован, в upstream 3.8.51 такой же):

```ts
const perAgentOptions = { ...baseOptions, connections: 1, pipelining: 0 };
const dispatchers = Array.from({ length: connectionLimit /*32*/ }, () => new Agent(perAgentOptions));
return createRoundRobinDispatcher(dispatchers);   // keepAliveTimeout = 4000
```

В undici `pipelining: 0` отключает keep-alive: для HTTP/1.1 каждый запрос открывает новое TCP+TLS (`connection: close`), а сверх 32 стримов запросы встают в очередь за SSE-стримом на агенте с `connections: 1`. Для h2 (ALPN разрешён) сессия закрывается через 4 с простоя, а round-robin по 32 агентам даёт переиспользование только выше ~8 req/s. Через прокси — keepalive 1 мс, CONNECT + TLS на каждый запрос. Реально держит соединение только режим `ENABLE_TLS_FINGERPRINT=true` (одна wreq-сессия, без прокси); по умолчанию он выключен. Какой протокол согласуется с chatgpt.com — подтвердить в бенчмарке (счёт SYN / `ss -tni`).

**Проверка по первоисточнику (после внешнего ревью).** Оба ревьюера Airouter возразили, что `pipelining: 0` якобы не отключает keep-alive. Исходник undici v8.10.2 подтверждает аудит: `docs/api/Client.md:80-84` — *«Set to `0` to disable keep-alive connections. Has no effect once HTTP/2 is negotiated»*; `lib/dispatcher/client-h1.js:681-699` — при `kPipelining` = 0 сокет помечается `kReset`, `:1324-1328` — заголовок `connection: close`; `keepAliveTimeout` в этой ветке не читается. Значит, всё сказанное верно **для HTTP/1.1**. Если с chatgpt.com согласуется h2, `pipelining` не действует и картина другая (сессия живёт до 4 с простоя, round-robin по 32 агентам). **Какой протокол реально согласуется — не проверено**: из sandbox это не измерить (egress перехватывается TLS-прокси sandbox, `issuer=CN=whoami-sandbox-ca`, там всегда HTTP/1.1). Это первый шаг фазы 0 с прод-хоста.

**Почему так сделано (важно для патча).** `pipelining: 0` и веер из агентов с одним соединением — намеренный фикс upstream #4580 (комментарий в `getDefaultDispatcherOptions`): при дефолтном `pipelining: 1` длинный SSE-стрим монополизировал единственный пуловый сокет к origin, и следующие Codex-стримы вставали за ним в очередь, в том числе в мульти-соединительном Agent (в прод-трейсах — ожидание trailers предыдущего стрима). Поэтому патч P1 не может просто вернуть keep-alive: он обязан сохранить независимость стримов. Вариант патча выбирается по результату проверки протокола: **если h2** — один Client к chatgpt.com с мультиплексированием и keepalive 30–60 с, без round-robin; **если HTTP/1.1** — Pool с большим `connections` и `pipelining: 1` (keep-alive на сокете, но не больше одного запроса в полёте, поэтому SSE-стрим не блокирует следующий запрос) с проверкой, что запрос не ставится на занятый сокет; в обоих случаях альтернатива — единая wreq-сессия (как в режиме `ENABLE_TLS_FINGERPRINT`). Любой вариант требует нагрузочной проверки с 8–32 параллельными стримами — это входит в серию B бенчмарка.

**P2 — peek и keepalive.** Peek выполняется для всех HTTP 200 SSE-ответов: лимит 8192 символа, ранний выход на `output_text.delta` / `completed`, таймаут чтения `FETCH_BODY_TIMEOUT_MS` = 10 мин. Первый текстовый токен он не задерживает, но задерживает предшествующие события (`response.created`, reasoning, дельты tool call). У Codex CLI первое событие, **предположительно**, больше 8 KB (instructions/tools повторяются в lifecycle-событиях), и тогда peek заканчивается сразу. Это гипотеза, а не замер: harness пишет размер первого SSE-события (`first_event_bytes`), проверить в фазе 0. У клиентов с коротким промптом события задерживаются до первой текстовой дельты — это секунды reasoning; ход только из tool call может потерять стриминг. Коммит `f96a5c159` (окно 2000 → 4000 мс, `OMNIROUTE_KEEPALIVE_THRESHOLD_MS`) — **гонка, а не задержка**: контент приходит не позже, зато ошибки первых 4 с отдаются с настоящим HTTP-статусом. Цена — первый keepalive-байт при медленном upstream приходит на 4-й секунде.

**P3 — записи в SQLite.** `await persistCodexQuotaState` на каждом Codex-ответе с `x-codex-*` — после peek, до возврата Response. `updateProviderConnection`: SELECT *, merge, шифрование, UPDATE, `backupDbFile` (с `existsSync`/`statSync`), **`invalidateDbCache("connections")`** (сброс кэша соединений и версии каталога моделей), `bumpProxyConfigGeneration`. Плюс `lastUsedAt`, upsert affinity, две записи breaker. Итого 3–6 записей, **не меньше** 1–5 мс нагрузки на event loop на запрос. Верхняя граница не оценена: сброс `invalidateDbCache("connections")` заставляет следующий запрос заново читать и расшифровывать все соединения, и под конкуренцией это может давать десятки мс лага — проверить CPU-профилем в фазе 0 (замечание внешнего ревью).

**P4.** Запрос: один parse, всегда `structuredClone` (`chat.ts:313-315`), ещё один при `retryCodexAccountOnTimeout` (`targetTimeoutRunner.ts:89`), санитайзеры `input[]`, один stringify. На тело 0,1–2 МБ — 3–20 мс CPU. Ответ: `JSON.parse` каждого `data:`, 3–4 TransformStream.

**P6.** Fallback работает только пока `result.success === false`, то есть до возврата Response. После коммита стрима клиент получает `response.failed` in-band, аккаунт помечается только для следующих запросов. Продолжить ход на другом аккаунте нельзя в принципе: encrypted reasoning и состояние tool calls выданы и расшифровываются в контексте конкретного аккаунта, другой аккаунт их не примет, а повтор с начала — это уже новый ход со второй оплатой токенов. Язык реализации на это не влияет.

**P7.** Breaker: `_restoreFromDb` / `_persistToDb` → `domain_circuit_breakers`; на успешный запрос две записи (`breaker.execute` + `chat.ts:1460`), причём `execute` считает успехом любой resolve. Cooldown: `rate_limited_until`, `codexScopeRateLimitedUntil`. Бэкпорты `0a7e404c1` (HALF_OPEN), `e1ea352e3` (сеть не открывает breaker), `cc589387d` (сброс `quota_exhausted`).

**P9 — хранилище (важно для импорта).** `enc:v1:<iv>:<ct>:<tag>`, AES-256-GCM, IV 16 байт, ключ `scryptSync(key, "omniroute-field-encryption-v1", 32)`; legacy-соль — только в стартовой миграции. В отличие от 3.8.51 нет автозагрузки ключа из `.env`. Без `STORAGE_ENCRYPTION_KEY` — открытый текст. **Файл БД: `DATA_DIR/storage.sqlite`** (`core.ts:88`); в проде `DATA_DIR=/app/data` (`Dockerfile:130`, `docker-compose.prod.yml:66`), по умолчанию `~/.omniroute`. Таблица `provider_connections` (`core.ts:190-234`): шифрованные `access_token`, `refresh_token`, `id_token`, `api_key`; открытые `rate_limited_until`, `last_used_at`, `email`; JSON `provider_specific_data` — `workspaceId`, `workspacePlanType`, `codexQuotaState`, `codexScopeRateLimitedUntil`, `subscriptionActiveUntil`, `codexTransport`.

**P10 — статистика.** `usage_history`: токены, `latency_ms`, `ttft_ms`, status, `connection_id`, `api_key`. **Дефект:** в стримах TTFT не передаётся в `onComplete`, и `saveRequestUsage` подставляет `latency_ms` — для бенчмарка `ttft_ms` непригоден. Есть API: `/api/usage/{history,call-logs,analytics,request-logs,provider-limits,quota,utilization,codex-reset-credit}`, `/api/provider-stats`, `/api/provider-metrics`, `/api/telemetry/summary` (p50/p95 по фазам parse / validate / policy / resolve / connect / finalize; фаза connect охватывает весь chatCore).

## 3. Механики Codex-роутинга, созданные в форке

**3.1 Стратегия `quota-deadline`** (`d5e638b40`, `397811109`, `0f092f480`). Включение: `settings.providerStrategies.codex.fallbackStrategy = "quota-deadline"` (`auth.ts:1491-1496`), в UI — только для codex. Вход (`codexQuotaDeadlineRouting.ts:133-202`): 5h и недельное окна (из quota-кэша или `getCachedCodexQuota` не старше 5 мин), их reset, reset credits со сроками, `subscriptionActiveUntil`. Скоринг:

```ts
buckets = [
  { amount: weeklyKnown ? max(weeklyRemaining - 5, 0) : 0, deadline: min(weeklyReset, subscriptionEnd) },
  { amount: 95 * max(credits - 1, 0),                      deadline: min(creditExpiry,  subscriptionEnd) } ]
rate   = max по префиксам (сортировка по дедлайну) cumulative / daysUntil   // null → 7 д, min 0.25 д
weight = min(6, 1 + rate / 20)
```

`sessionBlocked` (5h окно исчерпано при известном reset) — исключается; `sessionDepleted` — в конец; неизвестная неделя — медиана весов пула. Выбор: сортировка по `deficit = (now − lastUsedAt) × weight` (неиспользованный = 24 ч простоя) — по сути **weighted-LRU, «сжигай то, что раньше сгорит»**. Свежий `codexQuotaState` из заголовков ответа стратегия не читает; `getQuotaCache` не проверяет возраст. **Последствие:** при устаревшем кэше стратегия может выбрать уже исчерпанную учётку, получить 429 и уйти в failover — лишний RTT и шум в error-rate. **При `codexSessionAffinityTtlMs = 0` (дефолт, `settings.ts:143`) ходы одного диалога раскидываются по аккаунтам и теряют prompt-cache.**

**3.2 Same-account transport retry** (`de2b0cdcc`, `sameAccountTransportRetry.ts`): один повтор, задержка 2000 + 0…1000 мс; на 502/503/504/507, `connection reset`, `early eof`, `socket hang up`, `und_err_socket`, `STREAM_EARLY_EOF`; не на 429/401/400/квоту/контекст, и не после начала вывода.

**3.3 Invalidated OAuth** (`015b67465`, `chatCore/codexAuthFailure.ts`): 401 с «invalidated oauth token» сразу ставит `expired`/`isActive:false` без повторов refresh (раньше ~90 с).

**3.4 Другой аккаунт до фолбэка combo** (`0a5f7f423`): `retryCodexAccountOnTimeout: true`; после локального 524 — один повтор с `excludeConnectionIds` и новым дедлайном; таргет 120 с может занять до 240 с.

**3.5 Ожидание cooldown** (`1c6a46a27`, `cooldownAwareRetry.ts:130-169`): таймер, сработавший раньше дедлайна, перепланируется; по умолчанию 3 повтора, до 30 с ожидания; только когда весь пул в cooldown; для combo выключено.

**3.6 Терминальные ошибки** (`b45c4bd24`, merge `98dd69474`): failed / cancelled / incomplete → ошибка до трансляции; incomplete по max_output_tokens с текстом без tool call — успех (length); токены неуспешных учитываются.

**3.7 Бюджет пустых ответов**: 3 попытки на (provider, model) на весь клиентский запрос, общий для повторов аккаунтов и dispatch combo.

**3.8 Stickiness** (`feb12f9c3`): хэш первого user-сообщения в неймспейсе combo — привязка к таргету combo, не к аккаунту.

**3.9 Пересчёт плана** (`a39b546fa`): `workspacePlanType` из id_token при refresh, `free` больше не маскируется.

**3.10 Refresh**: без `scope` (намеренно, `tokenRefresh.ts:1031-1041`); мьютекс только внутри процесса. **Любой внешний рефрешер тех же учёток будет конфликтовать с форком (`refresh_token_reused`).**

**3.11 Identity** (`43fd688a1`): `codex-cli/0.155.0 (Windows 10.0.26200; x64)`, переопределяется `CODEX_CLIENT_VERSION` / `CODEX_USER_AGENT`; `originator: codex_cli_rs`; проброс `X-Codex-*`, `Thread-Id`, `Session-Id`.

**3.12 Скрытие quota-заголовков** (`e7e9d0ee4`, `e56bf9ab7`) для combo и моделей `coding*`; событие `codex.rate_limits` остаётся в теле, если не задан `OMNIROUTE_CODEX_DROP_NONSTANDARD_EVENTS`.

## 4. Профилирование

- `c67c00480` (есть в прод-образе): `OMNIROUTE_RUNTIME_DIAGNOSTICS=1`, задержка `OMNIROUTE_DIAGNOSTICS_DELAY_SECONDS` (120), длительность `OMNIROUTE_DIAGNOSTICS_DURATION_SECONDS` (180). CPU-профиль через in-process inspector (шаг 1 мс), раз в секунду — загрузка и лаг event loop (p50/p95/p99/max), CPU, heap, GC. Результат — `/tmp/omniroute-diagnostics-*/{telemetry.json,cpu.cpuprofile}`. TTFT по запросам не даёт, зато отвечает на вопрос «упирается ли шлюз в Node».
- `OMNIROUTE_TRACE=true` → `STAGE_TRACE` с метками `pre_executor`, `pre/post_semaphore`, `inside_rate_limit`, `post_executor`; `post_executor − inside_rate_limit` ≈ TTFB upstream + peek.
- `/api/telemetry/summary` — фазы.

## 5. Что чинится внутри форка и какой ценой

TTFT самого Codex: 0,5–3 с без reasoning, 5–30 с с reasoning.

| # | Цена в stable | Порядок | Чинится в форке? | Объём |
|---|---|---|---|---|
| P1 | handshake почти на каждый запрос; через прокси ещё и CONNECT | +10–60 мс напрямую, +100–400 мс через прокси | **да, с оговоркой**: h2 keep-alive Client для chatgpt.com или единая wreq-сессия; нельзя вернуть регрессию #4580 (очередь SSE-стримов за одним сокетом) — нужна нагрузочная проверка | 40–100 LOC + тест на параллельные стримы |
| P2 | около 0 для Codex CLI; у других клиентов — задержка промежуточных событий | 0–50 мс | **да**: выход после первого не-lifecycle события | 10–30 LOC |
| P3 | слабая по TTFT, заметная по CPU | 1–5 мс | **да**: запись без await с объединением; не сбрасывать кэш connections на quota-only; breaker — только при смене состояния; `lastUsedAt` в памяти | 80–200 LOC |
| P4 | CPU | 3–20 мс | частично: ленивый clone, один проход guardrails | 50–120 LOC |
| P5 | Next + middleware | 1–5 мс | частично и дорого: fast-path в `server-ws.mjs` по образцу WS-прокси | 300–800 LOC |
| P6 | свойство протокола | — | нет (и в Rust нет) | — |
| P7 | лишние записи | < 1 мс | да | ~10 LOC |
| P8 | хвосты повторов при деградации | секунды–минуты | частично: конфиг + точечные патчи | 50–150 LOC |
| P10 | нет TTFT в статистике | — | **да**: замер первого не-lifecycle события в `stream.ts` | 20–40 LOC |
| + | affinity 0 по умолчанию + quota-deadline → потеря prompt-cache | сотни мс – секунды на каждом ходе | **да, конфигом** `codexSessionAffinityTtlMs` | 0 LOC |
| + | синхронный preflight usage API | +200–800 мс в p99 | **да**: вынести в фон | 30–60 LOC |

Вывод: крупные устранимые потери TTFT (keep-alive, affinity, preflight, хвосты повторов) не связаны с языком и чинятся в форке примерно за 300–600 LOC. Переписывание на Rust выигрывает миллисекунды CPU (P4/P5) и не решает P6. В форке уже есть тонкий WS-путь — естественная основа для выделения Codex-пути. Главный риск любого выноса — владение refresh-токенами: у каждой учётки должен быть ровно один процесс-владелец.

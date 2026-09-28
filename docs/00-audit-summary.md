# Сводка аудита: stable-форк OmniRoute, проблемы Codex-пути и матрица заимствований

Редакция 2 (23.09.2026). Первая редакция описывала upstream и ошибочно считала, что у форка нет собственных правок. Теперь база сравнения — **продакшен пользователя: fenix007/omniroute ветка `stable`** (upstream v3.8.48 + 104 коммита, релизы `3.8.48-fork.N`).

Подробности: `report-fork-ledger.md` (журнал форка), `report-stable-hotpath.md` (горячий путь Codex в stable — главный отчёт), `report-omniroute.md` (upstream v3.8.51, с исправлениями), `report-cliproxyapi.md`, `report-codex-tools.md`.

## 1. Что уже сделано в stable

Это не «OmniRoute как есть», а зрелый собственный дистрибутив: процесс форка с тегами, собственный образ `ghcr.io/fenix007/omniroute`, quality gates (23 310 тестов, покрытие 81 % строк), регулярные ревью upstream с адресными бэкпортами. Для Codex-пути в форке уже есть:

| Механика | Коммиты | Суть |
|---|---|---|
| Стратегия `quota-deadline` | `d5e638b40`, `397811109`, `0f092f480` | weighted-LRU: вес растёт со скоростью «сгорания» недельного остатка и reset credits до дедлайна; исчерпанное 5h-окно исключает аккаунт |
| Same-account transport retry | `de2b0cdcc` | один повтор через 2–3 с на transport-ошибки до начала вывода |
| Немедленный failover инвалидированных OAuth | `015b67465` | без ~90 с бесполезных refresh |
| Другой аккаунт до фолбэка combo на провайдера | `0a5f7f423` | `retryCodexAccountOnTimeout` |
| Ожидание конца cooldown, когда весь пул занят | `1c6a46a27` | до 3 × 30 с, исправлен off-by-one таймера |
| Корректные статусы и формы ошибок в SSE | `f96a5c159`, `098261b47`, `792285c91`, `77184aca1`, `b45c4bd24` | ошибки первых 4 с — с настоящим HTTP-статусом; `response.failed` на `/v1/responses`; правильный terminal marker (до фикса 171 из 176 ошибок в логах были ложными 499 с нулём токенов) |
| Тонкий WS-путь Codex | `ed0819032`, `8fc1b924b`, `bd2f872d7`, `0187e6954` | одно upstream WS-соединение на клиентскую сессию, мимо chatCore и SQLite — готовый прототип выделенного Codex-пути |
| Identity Codex 0.155.0, fidelity запросов как у CLIProxyAPI | `43fd688a1`, `ed0819032` | |
| Приватное профилирование рантайма | `c67c00480` | CPU-профиль, лаг event loop, GC — без изменения кода |

## 2. Проблемы Codex-пути в stable

Статус проверен по коду stable (HEAD `0a5f7f423`).

| # | Проблема | Где | Влияние | Статус в stable | Чинится в форке |
|---|---|---|---|---|---|
| P1 | HTTP без keep-alive (для HTTP/1.1 подтверждено исходником undici v8.10.2; согласуется ли с chatgpt.com h2 — проверить с прод-хоста, это первый шаг фазы 0): undici `pipelining: 0`, 32 агента с одним соединением, round-robin, keepalive 4 с; через прокси — 1 мс. Намеренный фикс #4580 против очереди SSE-стримов за одним сокетом | `open-sse/utils/proxyDispatcher.ts:140-165` | +10–60 мс на запрос напрямую, +100–400 мс через прокси | присутствует | да, h2 keep-alive или единая wreq-сессия, 40–100 LOC + нагрузочный тест на параллельные стримы |
| — | Affinity сессии выключена по умолчанию (`codexSessionAffinityTtlMs: 0`), а `quota-deadline` раскидывает ходы диалога по аккаунтам | `src/lib/db/settings.ts:143` | потеря prompt-cache — сотни мс – секунды на каждом ходе | присутствует (проверить значение в проде) | да, конфигом, 0 LOC |
| — | Синхронный preflight usage API при выборе аккаунта | `codexQuotaFetcher.ts:44, 218-247` | +200–800 мс в p99 | присутствует | да, фон, 30–60 LOC |
| P2 | Peek первых 8 KB до коммита стрима; второй peek в `ensureStreamReadiness` | `codex.ts:604, 649-724`; `streamReadiness.ts:379` | ~0 для Codex CLI; у других клиентов задерживает события до первой текстовой дельты | присутствует, ограничен | да, 10–30 LOC |
| P3 | `await persistCodexQuotaState` до возврата ответа + сброс кэша connections; всего 3–6 записей SQLite на запрос | `chatCore.ts:3271`, `providers.ts:553-596` | 1–5 мс CPU на запрос | присутствует (из базы) | да, 80–200 LOC |
| P4 | `JSON.parse` + `structuredClone` + stringify тела, `JSON.parse` каждого SSE-события | `promptInjectionGuard.ts:66`, `chat.ts:313`, `stream.ts:1195` | 3–20 мс CPU | присутствует | частично, 50–120 LOC |
| P5 | Next App Router + middleware на HTTP-пути | `src/proxy.ts`, `route.ts` | 1–5 мс | присутствует (WS-путь обходит) | дорого, 300–800 LOC |
| P6 | Fallback только до коммита стрима | `chat.ts:1457-1849` | обрыв виден клиенту | свойство протокола Responses | нет, и в Rust нет |
| P7 | ~~Breaker только in-memory~~ | `circuitBreaker.ts` | — | **не подтвердилось**: breaker и cooldown персистятся; лишние 2 записи на успех | да, ~10 LOC |
| P8 | ~20 модулей на запрос, 3 системы отказа, 5 видов повторов | см. отчёт | хвосты в секунды–минуты при деградации (таргет combo 120 с → до 240 с) | присутствует | частично, конфиг + 50–150 LOC |
| P9 | Шифрование учёток | `encryption.ts` | — | фикс v3.7.9 есть, схема совместима | не требуется |
| P10 | У стримов `ttft_ms` = `latency_ms` — TTFT в статистике фактически нет | `usageHistory.ts:680-686` | нельзя измерять и улучшать TTFT по собственным данным | присутствует | да, 20–40 LOC |

**Факт фазы 0, шаг 0 (28.09.2026).** С хоста `codotok` `curl --http2` к `https://chatgpt.com/backend-api/codex/responses` согласовал **HTTP/2**. Следствия: (1) `pipelining: 0` в stable на h2 не действует (по документации undici — «no effect once HTTP/2 is negotiated»), поэтому реальная проблема P1 на прямом пути — не `connection: close`, а 32 отдельных агента с round-robin и простоем сессии 4 с: h2-сессия переиспользуется, только если тот же агент получает следующий запрос в пределах 4 с (≈ от 8 req/s); (2) регрессия #4580 (очередь SSE за одним сокетом) — эффект HTTP/1.1, на h2 стримы мультиплексируются; (3) выбран вариант патча P1 для h2: один `Pool`/`Client` на origin chatgpt.com с `allowH2: true`, `keepAliveTimeout` 60 с, без round-robin, лимит параллельных стримов по `maxConcurrentStreams` сервера. Оговорки: curl не доказывает, что так же договаривается undici внутри stable, и не покрывает путь через HTTP-прокси — это проверяется отдельно (см. `docs/benchmark.md`, шаг 0).

**Главный вывод.** Самые дорогие по TTFT потери — keep-alive, affinity, preflight, хвосты повторов — не связаны с языком и чинятся внутри форка примерно за 300–600 LOC. Выигрыш переписывания на Rust — миллисекунды CPU на запрос (P3/P4/P5) и более простая, предсказуемая система отказов (P8); P6 неустранима ни там, ни там.

## 3. Матрица заимствований для codexpool

Актуальна, если по итогам бенчмарка выбирается вынос Codex-пула (ADR-0001, варианты B/C).

| Что берём | Откуда | Как используем |
|---|---|---|
| **Стратегия `quota-deadline`** (weighted-LRU по скорости сгорания недельного остатка и reset credits до дедлайна; исключение аккаунтов с исчерпанным 5h-окном) | stable `codexQuotaDeadlineRouting.ts`, коммиты `d5e638b40`, `0f092f480` | стратегия `quota_deadline` в роутере, по умолчанию вместо `headroom`. **Отличие от stable (улучшение):** в stable стратегия читает только кэш без проверки возраста и может выбрать исчерпанную учётку; в codexpool вход — окна из заголовков последнего ответа плюс фоновый опрос `wham/usage`. В stable стратегия включается настройкой, в codexpool это дефолт — при бенчмарке выровнять |
| Affinity диалога к аккаунту включена по умолчанию | урок stable (дефолт 0 теряет prompt-cache) | `sticky = "session"` по умолчанию, TTL 1 ч |
| Same-account transport retry, 1 раз, 2–3 с, только до вывода | stable `sameAccountTransportRetry.ts` (`de2b0cdcc`) | классификатор upstream-ошибок |
| Немедленный `needs_login` на «invalidated oauth token» | stable `015b67465` | `auth::refresh` / классификатор 401 |
| Другой аккаунт до ухода на следующую ступень combo, с ограничением общего дедлайна | stable `0a5f7f423` | failover-цикл; общий бюджет попыток вместо удвоения таймаута |
| Ожидание конца cooldown, когда весь пул занят (с перепроверкой дедлайна) | stable `1c6a46a27` | опция `wait_for_cooldown_s` вместо немедленного 429 `pool_exhausted` |
| Terminal failures: failed/cancelled/incomplete → ошибка; incomplete по лимиту с текстом → успех | stable `b45c4bd24` | учёт статуса в Stats и failover |
| Корректные HTTP-статусы ошибок до коммита стрима; `response.failed` на `/v1/responses` | stable `f96a5c159`, `792285c91` | codexpool не коммитит поток до первого чанка, поэтому статус всегда настоящий |
| Identity `codex-cli/0.155.0`, fidelity запросов, `OpenAI-Beta: responses_websockets=2026-02-06` для WS | stable `43fd688a1`, `ed0819032`, `bd2f872d7` | `upstream::headers`, переопределяемо в конфиге |
| Тонкий WS-путь: одно upstream-соединение на клиентскую сессию | stable `scripts/dev/responses-ws-proxy.mjs` | `transport = "ws"` — эксперимент MVP-3, сравнить с h2 в бенчмарке |
| `storage.sqlite` в `DATA_DIR`, `provider_connections`, `enc:v1` AES-256-GCM, scrypt static salt; `provider_specific_data.workspaceId`, `workspacePlanType`, `codexQuotaState` | stable `core.ts:88, 190-234`, `encryption.ts` | импортёр `codexpool import omniroute --db /app/data/storage.sqlite` |
| Refresh без `scope`, per-account singleflight, persist-first нового refresh_token, не рефрешить на 403 | stable/upstream `tokenRefresh.ts`, codex-switcher | `auth::refresh` |
| Формат токен-файлов CPA, fsnotify-watcher, таблица cooldown (429 → Retry-After / backoff 10 с…30 мин; 401/403 → 30 мин; 5xx → 1 мин), без cooldown на client-abort | CLIProxyAPI | импорт/экспорт, `pool::cooldown` |
| `~/.codex/auth.json`, `registry.json`, JWT-клеймы, `wham/usage`, нормализация окон по `limit_window_seconds` | codex-auth, codex-switcher | импортёры, фоновый опрос окон |
| Warm-up окна после сброса | codex-switcher | опциональный планировщик |

## 4. Инвариант при любом варианте: один владелец refresh-токенов

Refresh-токены ChatGPT одноразовые, а мьютекс refresh в stable работает только внутри процесса. Если одну и ту же учётку одновременно обслуживают stable и codexpool (или два инстанса stable), первый refresh инвалидирует токен у второго (`refresh_token_reused`) и учётка выпадает до ручного перелогина. Поэтому при выносе Codex-пула учётки **переносятся**, а не копируются: после импорта в codexpool соединения в stable отключаются (или stable ходит к Codex только через codexpool как через upstream). Для бенчмарка «stable против codexpool» нужны **разные** учётки в двух пулах либо прогоны строго по очереди с повторным импортом.

«Один владелец» означает один **процесс**, а не одну программу: две реплики stable на общей `storage.sqlite` конфликтуют при refresh точно так же. Горизонтальное масштабирование stable для Codex без внешнего координатора refresh невозможно.

**Протокол переноса учёток (двухфазный, после внешнего ревью).** Сбой посреди переноса не должен оставлять токен у двух владельцев:
1. Остановить stable или пометить переносимые соединения `is_active = 0` (фаза «transferring»), чтобы stable не делал refresh.
2. `codexpool import omniroute --no-verify` — токены копируются без refresh.
3. Первый refresh выполняет только codexpool; новый refresh-токен сохраняется в его БД (persist-first, `synchronous=FULL`).
4. Подтверждение: codexpool отмечает учётку активной, в stable соединение остаётся выключенным.
5. Откат: экспорт текущего токена из codexpool в stable (`codexpool accounts export --cpa`), затем включение соединения в stable. Старый токен stable после шага 3 недействителен — откатываться только через экспорт.

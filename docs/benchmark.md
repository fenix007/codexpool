# Методика бенчмарка: TTFT, TPS, error-rate

Цель — измеримо ответить на два вопроса: (1) сколько overhead добавляет прокси относительно прямого вызова upstream; (2) насколько codexpool лучше OmniRoute (и CLIProxyAPI как эталона) на одном и том же пуле учёток и одних и тех же промптах. Результаты воспроизводимы: фиксированный корпус, фиксированные учётки, одинаковое время суток, одинаковая машина.

## 1. Что меряем

| Метрика | Определение | Где меряется |
|---|---|---|
| **TTFT** | от отправки последнего байта запроса до первого **содержательного события** (`response.output_text.delta`, reasoning summary, `function_call_arguments.delta`, `output_item.added`), не до заголовков | `sse_bench.py` (клиентская сторона), `request_events.ttft_ms` (прокси) |
| **TTFT текста** (`ttft_text_ms`) | до первого `response.output_text.delta` / `choices[0].delta.content` — то, что пользователь видит как ответ. Для reasoning-моделей может быть на секунды позже TTFT | `sse_bench.py` |
| Размер первого события (`first_event_bytes`) | байты первого SSE-события — проверка гипотезы, что у Codex CLI peek stable заканчивается сразу (событие > 8 KB) | `sse_bench.py` |
| Серверные метки | stable: `OMNIROUTE_TRACE=true` (`pre_executor`, `inside_rate_limit`, `post_executor`); codexpool: `route_ms`, `upstream_ttfb_ms`. Нужны для абсолютных цифр overhead без клиентского шума | логи / Stats API |
| headers_ms | до HTTP-заголовков ответа | `sse_bench.py` — показывает overhead до выбора учётки и до первого байта upstream |
| **Overhead прокси** | `TTFT(через прокси) − TTFT(direct)` на одних кейсах, p50/p95; внутри codexpool — `ttft_ms − upstream_ttfb_ms` | compare.py, `/stats/summary.proxy_overhead_ms` |
| **TPS** | `output_tokens / (total_ms − ttft_ms) × 1000`; `output_tokens` — из `usage` финального события, иначе число дельт (флаг `tokens_estimated`) | обе стороны |
| **Error-rate** | доля запросов не в классе `ok`, с разбивкой: `http_429`, `http_401`, `http_403`, `http_5xx`, `timeout`, `stream_abort`, `stream_error`, `bad_sse` | обе стороны |
| Failover cost | для запросов с `attempts > 1`: добавка к TTFT относительно `attempts = 1` | `/stats/failovers` |
| Client-observed | `first_output_ms`, `total_ms`, exit code реальных Codex CLI / Claude Code | `client_bench.py` |

## 2. Бэкенды

| target | Как поднимается |
|---|---|
| `direct` | прямой `POST https://chatgpt.com/backend-api/codex/responses` с токеном одной учётки и заголовками codex-cli (базовая линия; в harness — через мини-прокси `direct_shim` codexpool с одной учёткой и без пула, либо `--raw-upstream` режим `sse_bench.py`, добавить в MVP-1) |
| `omniroute-stable` | **прод пользователя**: fenix007/omniroute ветка `stable` (upstream v3.8.48 + собственный patch set, теги `3.8.48-fork.N`, образ `ghcr.io/fenix007/omniroute`), тот же combo из тех же учёток. В `results/README.md` фиксировать тег fork.N и sha. Это главная базовая линия для codexpool |
| `omniroute-upstream` | опционально: upstream последней версии — только чтобы показать, что stable-форк уже лучше/хуже upstream на Codex-пути |
| `cliproxyapi` | опционально, те же учётки через `import --cpa`, `http://localhost:8317/v1` |
| `codexpool` | `http://127.0.0.1:8787/v1` |

**Планы учёток.** Сравнивать можно только учётки одинаковых планов (лимиты окон разные). Если одинаковых нет — нормировать error-rate и число failover на долю использованного окна и явно отметить это в отчёте.

**Учётки.** Refresh-токены одноразовые, поэтому два прокси не могут одновременно держать одни и те же учётки: первый refresh одного инвалидирует токен у другого (`refresh_token_reused`). Либо у каждого бэкенда свой набор из ≥3 учёток одинаковых планов, либо прогоны идут строго по очереди с переносом учёток (импорт в codexpool → отключение в stable → прогон → обратный перенос). Для `direct` — отдельная учётка или короткий прогон при остановленном stable.

**Фаза 0 (до любых патчей)**: `direct` против `omniroute-stable` в HTTP-режиме и через тонкий WS-путь (`codexTransport` / `responses-ws-proxy`), параллельно `OMNIROUTE_RUNTIME_DIAGNOSTICS=1` и `OMNIROUTE_TRACE=true`, плюс подсчёт новых TCP-соединений к chatgpt.com (`ss -tni` / SYN) при 1/5/20 req/s. `ttft_ms` из `usage_history` stable для сравнения **не использовать**: у стримов он равен `latency_ms` (P10). Записать текущие значения `codexSessionAffinityTtlMs`, стратегии Codex и `retryCodexAccountOnTimeout` в проде.

Внутри каждого бэкенда — один набор учёток; перед серией — `wham/usage`, чтобы убедиться, что окна не на грани исчерпания (иначе 429 исказят error-rate).

## 3. Корпус (`bench/corpus.json`)

| id | Что проверяет |
|---|---|
| `short_echo` | чистый TTFT: минимум токенов, максимум запросов |
| `short_code` | типичный короткий ответ Codex |
| `medium_gen` | TPS на ~1000 выходных токенов |
| `long_context` | overhead на теле ~25k токенов (буферизация/парсинг тела прокси) — `prompt_file` подставляется скриптом `prepare_corpus.py` из исходников репозитория |
| `tool_call` | корректность проброса `function_call_arguments.delta` и `tools` |

## 4. Протокол прогона

0. **Проверка протокола (первый шаг, с прод-хоста):** `curl -sS -o /dev/null -w '%{http_version}\n' --http2 https://chatgpt.com/backend-api/codex/responses` и `echo | openssl s_client -connect chatgpt.com:443 -alpn h2,http/1.1 | grep ALPN`. Из sandbox бессмысленно: egress там перехватывается TLS-прокси. **Результат 28.09: `2` (HTTP/2) с хоста codotok.** Дополнительно проверить, что h2 согласует сам undici в контейнере stable (команда ниже) и что это прод-хост OmniRoute; если egress идёт через HTTP-прокси — повторить через него.

   ```bash
   # внутри контейнера stable (каталог приложения, где есть node_modules/undici)
   node -e "const dc=require('diagnostics_channel');dc.subscribe('undici:client:connected',m=>console.log('alpn=',m.socket.alpnProtocol));const {fetch,Agent}=require('undici');fetch('https://chatgpt.com/backend-api/codex/responses',{method:'POST',body:'{}',headers:{'content-type':'application/json'},dispatcher:new Agent({connections:1,pipelining:0})}).then(r=>console.log('status',r.status))"
   ```

   Затем выровнять конфиг бэкендов (стратегия Codex, affinity TTL, preflight, `retryCodexAccountOnTimeout`) и записать фактические значения.
1. **Прогрев**: 1 запрос `short_echo` на каждый бэкенд (TLS/h2 соединения, JIT Node в OmniRoute).
2. **Серия A — латентность** (`concurrency = 1`): все кейсы × `repeat ≥ 30` (не меньше 200 запросов на бэкенд в сумме по сериям), порядок бэкендов **перемешивается на каждом повторе** (seed записывается), чтобы нелинейный дрейф upstream распределился равномерно.
3. **Серия B — параллелизм** (`concurrency` = 1 / 4 / 8 / 16 / 32): `short_echo` и `medium_gen` × 10 на каждом уровне, `direct` на тех же уровнях — как базовая линия. Смотрим деградацию TTFT p95 и TPS относительно `direct` — здесь проявляются блокировки event loop, пересборка кэша соединений stable и очередь в диспетчере.
4. **Серия C — отказоустойчивость**: намеренно исчерпанная учётка первой в combo (или мок с `--fail-429 0.3`). Меряем error-rate и добавку к TTFT при failover. Для codexpool — сверка с `/stats/failovers`.
5. **Серия C2 — весь пул у лимита**: все учётки близки к исчерпанию 5h-окна (или мок `--fail-429 0.9`). Меряем долю мгновенных 429 против ожидания cooldown (`wait_for_cooldown` / cooldown-aware retry stable) и хвосты TTFT.
6. **Серия D — реальные клиенты**: `client_bench.py --client codex` и `--client claude` × 5 задач × 3 повтора на каждый бэкенд; сопоставляем `first_output_ms` с `ttft_ms` прокси по `x-bench-run-id`.
7. Всё в одну сессию (≤ 90 мин), один регион/сеть; записать `run_id`, версию каждого прокси, список учёток (алиасы) в `results/README.md`.

## 5. Критерии успеха MVP-1

- Overhead codexpool на TTFT ≤ 5 мс p50 и ≤ 15 мс p95 (серия A, относительно `direct`).
- TTFT codexpool ≤ TTFT omniroute-stable − 50 мс p50. Порог предварительный: конкретная ожидаемая разница пересчитывается после аудита горячего пути в stable (report-stable-hotpath.md) — если stable уже снял часть overhead, цель снижается, а решение «переписывать или нет» принимается по фактической разнице.
- Error-rate codexpool не выше omniroute-stable в серии C (stable уже содержит собственные механики failover: quota-deadline routing, same-account transport retry, retry другого аккаунта до фолбэка провайдера, ожидание cooldown deadline — это сильная базовая линия).
- TPS не ниже `direct` в пределах 2 %.
- Серия B при concurrency 16: рост TTFT p95 codexpool не больше роста у `direct` при той же конкурентности + 10 п.п.
- Серия C: error-rate клиента 0 % при одной исчерпанной учётке из трёх; добавка к TTFT при failover ≤ 1 RTT до upstream + 5 мс.

## 6. Инструменты

- `bench/sse_bench.py` — синтетический SSE-клиент (stdlib), JSONL + summary.
- `bench/client_bench.py` — прогон `codex exec --json` (изолированный `CODEX_HOME`) и `claude -p --output-format stream-json` (`ANTHROPIC_BASE_URL`).
- `bench/compare.py` — таблица сравнения бэкендов с Δ к базовой линии; отчёт-страница строится из `*.summary.json`.
- `bench/mock_upstream.py` — мок с управляемыми TTFT/TPS/доля 429/обрывы — для тестов роутера и CI без реальных учёток.

Самопроверка harness на моке (22.09.2026): задано TTFT 250 мс / 80 tok/s → измерено 251.5 мс / 78.2 tok/s, 429 корректно классифицированы; собственный overhead клиента ≈ 1.5 мс.


## Изменения после внешнего ревью (23.09.2026)

Добавлены: проверка протокола как шаг 0, `ttft_text_ms` и `first_event_bytes`, серия C2, конкурентность до 32, перемешивание порядка бэкендов на каждом повторе, пороги относительно `direct`, требование одинаковых планов учёток. Разбор ревью: `review/triage-docs.md`.

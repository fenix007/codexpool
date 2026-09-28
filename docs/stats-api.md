# codexpool Stats API

Отдельный HTTP-сервер (`stats_listen`, по умолчанию `127.0.0.1:8788`), read-only, аутентификация `Authorization: Bearer <admin_key>` (или без auth на loopback, если `stats_public_loopback = true`). Данные — из SQLite `request_events` + живое состояние пула.

## 1. Событие запроса (`request_events`)

```sql
CREATE TABLE request_events (
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,              -- unix ms начала запроса
  request_id TEXT NOT NULL,         -- uuid, = x-codexpool-request-id в ответе
  client TEXT,                      -- метка client_key
  client_app TEXT,                  -- codex-cli | claude-code | openai-sdk | other (по UA/originator)
  client_version TEXT,
  endpoint TEXT NOT NULL,           -- responses | chat | messages
  model TEXT NOT NULL,              -- запрошенная
  upstream_model TEXT,              -- после алиасов/rewrite
  scope TEXT,                       -- codex | spark
  combo TEXT, step INTEGER,
  account_id TEXT,                  -- финальная учётка (NULL, если пул исчерпан)
  attempts INTEGER NOT NULL,        -- сколько учёток перебрали
  attempts_json TEXT,               -- [{account, status, error_class, ms}] по каждой попытке
  status INTEGER,                   -- http-код клиенту
  error_class TEXT,                 -- ok | http_429 | http_401 | http_403 | http_5xx | upstream_timeout | stream_error | client_abort | pool_exhausted | bad_request
  error_message TEXT,
  ttft_ms INTEGER,                  -- до первого content-дельта клиенту
  upstream_ttfb_ms INTEGER,         -- до первого байта от upstream (разница = overhead прокси после выбора)
  route_ms INTEGER,                 -- от приёма запроса до отправки upstream (выбор учётки, refresh)
  total_ms INTEGER,
  stream INTEGER,                   -- 1/0
  input_tokens INTEGER, output_tokens INTEGER, cached_tokens INTEGER, reasoning_tokens INTEGER,
  tps REAL,                         -- output_tokens / (total_ms - ttft_ms) * 1000
  request_bytes INTEGER, response_bytes INTEGER,
  session_id TEXT, sticky_hit INTEGER,
  usage_5h_after REAL, usage_weekly_after REAL   -- из x-codex-* после ответа, если были
);
CREATE INDEX ix_events_ts ON request_events(ts);
CREATE INDEX ix_events_account_ts ON request_events(account_id, ts);
```

## 2. Эндпоинты

Все принимают `from`, `to` (ISO 8601 или относительные `-1h`, `-7d`; по умолчанию `-24h`), `account`, `model`, `client_app`, `combo` как фильтры.

| Метод и путь | Ответ |
|---|---|
| `GET /stats/summary` | `{requests, ok, error_rate, error_classes:{…}, ttft_ms:{p50,p95,p99}, upstream_ttfb_ms:{…}, proxy_overhead_ms:{p50,p95}, tps:{p50,p95}, tokens:{input,output,cached}, attempts_avg, failovers, pool_exhausted}` |
| `GET /stats/timeseries?bucket=5m` | `[{ts, requests, errors, ttft_p50, tps_p50, tokens_out}]` |
| `GET /stats/accounts` | по каждой учётке: `{id, alias, plan, requests, error_rate, ttft_p50, tps_p50, tokens_out, inflight, auth_status, scopes:{codex:{used_5h, reset_5h, used_weekly, reset_weekly, cooldown_until, backoff_level}, spark:{…}}, last_error, last_used_at}` — живое состояние + агрегаты за период |
| `GET /stats/accounts/{id}/timeseries` | окна квоты во времени из `usage_snapshots` + запросы |
| `GET /stats/models` | агрегаты по модели/scope |
| `GET /stats/errors?limit=100` | последние ошибки с `attempts_json` |
| `GET /stats/requests?limit=100&cursor=` | сырые события (пагинация по `id`) |
| `GET /stats/requests/{request_id}` | одно событие полностью |
| `GET /stats/failovers` | события с `attempts > 1`: причина, с какой учётки на какую |
| `GET /stats/bench/{run_id}` | агрегаты по `x-bench-run-id` — сопоставление с harness'ом (TTFT прокси vs TTFT клиента) |
| `GET /stats/export?format=jsonl|csv` | выгрузка событий за период |
| `GET /metrics` | Prometheus text format |
| `GET /healthz` | `{ok, accounts_ready, accounts_total, inflight}` |

Пример `GET /stats/summary?from=-1h`:

```json
{
  "from": "2026-09-22T14:00:00Z", "to": "2026-09-22T15:00:00Z",
  "requests": 412, "ok": 397, "error_rate": 0.036,
  "error_classes": {"http_429": 9, "upstream_timeout": 3, "client_abort": 3},
  "ttft_ms": {"p50": 812, "p95": 1940, "p99": 3300},
  "upstream_ttfb_ms": {"p50": 806, "p95": 1925},
  "proxy_overhead_ms": {"p50": 2.1, "p95": 6.8},
  "tps": {"p50": 71.4, "p95": 98.0},
  "tokens": {"input": 9_812_330, "output": 402_118, "cached": 7_110_002},
  "attempts_avg": 1.04, "failovers": 15, "pool_exhausted": 0
}
```

## 3. Live-поток

`GET /stats/stream` — SSE с событиями `request` (по завершении каждого запроса), `account` (изменение состояния окна/cooldown), `failover`. Для дашбордов и для отладки бенчмарка в реальном времени.

## 4. Метрики Prometheus

```
codexpool_requests_total{account,model,scope,status,error_class,client_app}
codexpool_ttft_ms_bucket{account,model}          # histogram
codexpool_upstream_ttfb_ms_bucket{account}
codexpool_tps{account,model}                      # summary/gauge последнего окна
codexpool_inflight{account}
codexpool_window_used_percent{account,scope,window="5h|weekly"}
codexpool_window_reset_seconds{account,scope,window}
codexpool_cooldown_active{account,scope}
codexpool_auth_status{account,status}
codexpool_refresh_total{account,result}
codexpool_failover_total{from_error_class}
codexpool_stats_dropped_total
```

## 5. Ретенция

`stats.retention_days = 30` — фоновая чистка `request_events` раз в сутки; `usage_snapshots` агрегируются до 1 точки/час после 7 дней. `attempts_json` и `error_message` можно отключить (`stats.store_errors = false`) для экономии.

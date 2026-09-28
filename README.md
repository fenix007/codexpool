# codexpool

Быстрый прокси для пула Codex-аккаунтов (ChatGPT OAuth): combo-роутинг, failover/failback, отдельный Stats API, импорт учёток из OmniRoute / CLIProxyAPI / `~/.codex`.

**Статус: скелет (MVP-1 в работе).** Прошёл внешнее ревью двумя моделями через Airouter (23.09.2026); принятые правки внесены, разбор — `docs/review/triage-code.md`. Главный незакрытый блокер — заглушка `stream_with_accounting` в `proxy/mod.rs`. Структура модулей и контракты зафиксированы, тела с `todo!()` — точки реализации. Сборка: `cargo build --release` (в sandbox проектирования toolchain отсутствовал — первая компиляция на машине разработчика; ожидаемы правки версий крейтов).

```
src/
├── main.rs          запуск: конфиг → store → pool → серверы proxy (:8787) и stats (:8788)
├── cli.rs           clap: serve | import | accounts | login | usage | bench
├── config.rs        ~/.codexpool/config.toml
├── auth/            jwt-клеймы, refresh (без scope, per-account mutex, persist-first)
├── pool/            AccountState, окна 5h/weekly, cooldown, выбор готовых учёток
├── router/          combo-ступени, стратегии, план failover
├── upstream/        reqwest-клиент с пулом h2, заголовки codex, классификация ошибок
├── proxy/           axum: /v1/responses passthrough, /v1/chat/completions, /v1/messages, /v1/models
├── stats/           RequestEvent → mpsc → batch writer; Stats API + /metrics
├── store/           SQLite (WAL): accounts, account_scopes, request_events, usage_snapshots
└── import/          storage.sqlite OmniRoute (AES-GCM enc:v1), CPA-файлы, ~/.codex/auth.json, codex-switcher
```

Документация: `docs/architecture.md`, `docs/stats-api.md`, `docs/benchmark.md`, `docs/ADR-0001-language.md`.

## Быстрый старт (после реализации MVP-1)

```bash
cargo build --release
./target/release/codexpool import omniroute --db /app/data/storage.sqlite --key "$STORAGE_ENCRYPTION_KEY"   # локально: ~/.omniroute/storage.sqlite
./target/release/codexpool import codex-file ~/.codex/auth.json --alias personal
./target/release/codexpool accounts list
./target/release/codexpool serve            # proxy 127.0.0.1:8787, stats 127.0.0.1:8788
```

Codex CLI (`~/.codex/config.toml`):

```toml
model_provider = "codexpool"
[model_providers.codexpool]
name = "codexpool"
base_url = "http://127.0.0.1:8787/v1"
env_key = "CODEXPOOL_KEY"
wire_api = "responses"
```

Claude Code: `ANTHROPIC_BASE_URL=http://127.0.0.1:8787 ANTHROPIC_AUTH_TOKEN=$CODEXPOOL_KEY claude` (MVP-2).


## Структура репозитория

```
Cargo.toml, config.example.toml, src/   Rust-скелет codexpool (не компилировался; вариант C из ADR-0001)
bench/                                  harness бенчмарка: sse_bench.py, client_bench.py, compare.py, mock_upstream.py, corpus.json
docs/                                   архитектура, ADR-0001, методика бенчмарка, Stats API, сводка аудита
docs/audit/                             отчёты аудита: журнал stable-форка, горячий путь stable, upstream OmniRoute, CLIProxyAPI, codex-auth/switcher
docs/review/                            внешнее ревью через Airouter и разбор находок
```

## С чего начинать

По ADR-0001 порядок такой: **фаза 0** — бенчмарк прод-`stable` (fenix007/omniroute) против прямого вызова chatgpt.com (`bench/`, методика в `docs/benchmark.md`; первый шаг — проверить с прод-хоста, отдаёт ли chatgpt.com HTTP/2); **фаза 1** — патчи в `stable`; этот сервис реализуется, только если после фазы 1 сработает порог.

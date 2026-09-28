# Разбор внешнего ревью Rust-скелета codexpool (Airouter, 23.09.2026)

Ревьюеры: `codex/gpt-5.5` и `kr/qwen3-coder-next` (не Claude). `codex/gpt-5.6-sol` в каталоге есть, но не уложился в лимит времени, а `cs/deepseek-v4-pro` и `cs/glm-5.3` возвращали только reasoning без текста. Rust-toolchain в sandbox нет, поэтому `cargo check` не запускался. Спорные утверждения о компиляции я проверял **по исходникам самих крейтов нужных версий** (GitHub, теги релизов), а не по памяти моделей. Исходные ответы: `codex-gpt-5-5-code.md`, `kr-qwen3-coder-next-code.md`. Номера строк у ревьюеров не всегда совпадают с файлами: код отдавался им частями.

Итог: 70 находок, из них **~20 ложных** (в основном «ошибки компиляции», которых нет), **~25 приняты и исправлены**, остальное — известные `todo!()` (MVP-1) или мелочи на потом.

## Отклонено — проверено по первоисточнику

| Находка | Кто | Проверка |
|---|---|---|
| `AesGcm<Aes256, U16>` и `Nonce::<U16>` не существуют в aes-gcm 0.10 — «весь импорт OmniRoute не скомпилируется» | обе | aes-gcm v0.10.3 `src/lib.rs:193`: `pub struct AesGcm<Aes, NonceSize, TagSize = U16>`; `:133`: `pub type Nonce<NonceSize> = GenericArray<u8, NonceSize>`; `:325-330`: для nonce ≠ 12 байт J0 = GHASH(IV…), то есть ровно то, что делает Node для 16-байтного IV. `pub use aes` — `:107` |
| `scrypt::Params::new` в 0.11 без аргумента длины | Qwen | scrypt v0.11.0 `src/params.rs:44`: `pub fn new(log_n: u8, r: u32, p: u32, len: usize)` |
| `chrono::Duration::from_std(..).unwrap_or_default()` не компилируется | Qwen | chrono v0.4.38 `time_delta.rs:52`: `TimeDelta` деривит `Default`, значит `Result::unwrap_or_default` доступен |
| Нет фич `serde_json/raw_value`, `tower-http/trace`, `clap/env` | GPT | Все три включены в `Cargo.toml` |
| `Scope` / `AuthStatus` не `Copy` → ошибки move | GPT | Оба деривят `Clone, Copy` |
| `std::time::Duration - Duration` не определён | обе | `impl Sub for Duration` есть в std; заменено на `saturating_sub` только ради защиты от паники |
| `serde_json::to_string(&enum).trim_matches('"')` портит значения | обе | Для unit-вариантов с `rename_all` результат — `"codex"`; `trim_matches` снимает только кавычки |
| `wreq` как optional dependency с фичей `["wreq"]` некорректен | обе | Для optional-зависимости Cargo создаёт неявную фичу с её именем; `tls-impersonate = ["wreq"]` валидно (существование нужной версии крейта проверим при первой сборке) |
| `axum::serve(listener, router)` требует `into_make_service()` | Qwen | В axum 0.7/0.8 `serve` принимает `Router` напрямую |
| `rusqlite::Connection: !Send` — `Mutex<Connection>` в `Arc` невалиден | Qwen | `Connection: Send` (но `!Sync`), поэтому `Mutex<Connection>: Sync` — паттерн корректен |
| Дублирование `?5` в UPDATE сдвигает `id` | Qwen | Нумерованные параметры SQLite можно переиспользовать; `id` остаётся `?6` |
| `clone_from_slice` может паниковать | Qwen | `sorted` строится из того же `ready` — длины равны всегда |
| `r.text()` после `r.json()` | Qwen | Это разные ветки (успех / неуспех) |
| Каталог данных не создаётся | GPT | `Store::open` делает `create_dir_all(parent)` |
| Ключ `STORAGE_ENCRYPTION_KEY` из env не читается | обе | `#[arg(long, env = "STORAGE_ENCRYPTION_KEY")]` у `--key` |
| `aes_gcm::Error` без `Display` | GPT | `aead::Error` реализует `Display` |
| В `store` 12-байтный nonce, «несовместимо с OmniRoute» | GPT | Это разные форматы по замыслу: свой формат хранения codexpool и формат импорта OmniRoute |
| `inflight_add(-1)` вызывается дважды | Qwen | В каждой ветке ровно один декремент; реальная утечка только в заглушке стрима (см. ниже) |

## Принято и исправлено

| Находка | Кто | Правка |
|---|---|---|
| `Ok(Some(Err(e))) \| Ok(None)` — привязка `e` не во всех альтернативах, ошибка компиляции | GPT | `Ok(Some(Err(_))) \| Ok(None)`, убран мусорный `e_str` |
| `Envelope<'a>` с `#[serde(flatten, borrow)]` и `HashMap<&str, &RawValue>` не соберётся | GPT | Поле `_rest` удалено (serde и так пропускает неизвестные поля); `&str` → `String`, потому что заимствование ломается на строках с escape |
| Рекурсия `Box::pin(responses(...))` после 401 сбрасывала флаг и могла зациклиться на refresh | обе | Очередь кандидатов: после успешного refresh учётка возвращается в начало, refresh — не больше одного раза на учётку за запрос |
| `401 «invalidated oauth token»` не обрабатывался отдельно | — (по архитектуре) | Сразу `NeedsLogin` без refresh, как в stable `015b67465` |
| `sticky_get`: деструктуризация `DashMap::Ref` как кортежа — ошибка компиляции | GPT | `.filter(\|e\| e.value().1 > now)` |
| `get` / `ready_among` клонируют через guard | GPT | Явно `.value().clone()` |
| DashMap write-guard держится на время синхронной записи в SQLite (`observe`, `cooldown`, `set_auth_status`) | обе | Снимок состояния, `drop(guard)`, потом запись; перенос записи в фоновый writer — TODO MVP-1 |
| `looks_like_error_frame` ищет подстроку в сыром TCP-чанке; `"type": "error"` с пробелом не ловится | обе | Разбор первого SSE-фрейма (`event:` / `data:` + JSON), в том числе `response.failed`; дочитывание неполного первого фрейма — TODO MVP-1 |
| Кандидаты не дедуплицируются между ступенями | GPT | Одна учётка — один раз в плане |
| `quota_deadline`: прошедший reset считался «дедлайном через 6 ч» | GPT | Прошедший reset → горизонт 7 дней, как в stable |
| `Headroom` сортирует кортеж лексикографически (99 % / 1 % выше 50 % / 50 %) | GPT | Ключ — минимум из двух окон |
| 3xx классифицировались как ошибка клиента | GPT | 3xx → `UpstreamError` (failover) |
| Ключи клиента и admin-ключ сравниваются через `==`; первые 8 символов ключа уходят в статистику | обе | `ct_eq` за константное время; в статистику пишется `key#N` |
| Stats API без `admin_key` открыт на любом адресе; проверка loopback через `starts_with("localhost")` пропускает `localhost.evil` | обе | `Config::load` разбирает `listen` и `stats_listen` как `SocketAddr` и требует `admin_key`, если адрес не loopback |
| `Debug` у `Tokens` / `Config` печатает токены и ключи | обе | Ручные `Debug` с `<redacted>` |
| Batch-writer блокирует поток tokio и теряет весь батч при ошибке | обе | `block_in_place`, при ошибке батч копится до 10 000 событий, сверх — отброс со счётчиком `codexpool_stats_dropped_total` |
| `default_scope` по умолчанию пустой | GPT | Подставляется `codex` |
| `sticky` не чистится | GPT | Ленивая чистка при размере > 10 000 |

## Известно и остаётся на MVP-1 (не баг скелета, а незаписанная часть)

- **`stream_with_accounting` — заглушка**, клиент получит 200 с пустым телом, `inflight` не освобождается (обе модели: блокер). Это первая задача MVP-1: отдать `first_chunk`, дальше стрим с idle-таймаутом, TTFT и usage из финального фрейма, освобождение `inflight` и отправка события на всех путях, включая обрыв клиента (через guard с `Drop`).
- `stream=false` от клиента: форсируем стрим к upstream, но собирать итоговый JSON ещё нечем (MVP-1.1).
- `Store::load_accounts` / `upsert_account`, методы Stats API, `/metrics`, `/stats/export`, `/stats/stream` — `todo!()` по плану.
- Мелочи на потом: `Retry-After` в формате HTTP-date, `fetch_usage` с захардкоженным URL (у `wham/usage` другой base, это осознанно), классификация refresh-ошибок по подстрокам тела, кардинальность метрик по аккаунту (аккаунтов единицы — приемлемо).

## Оценка до первой сборки

Реальных ошибок компиляции ревьюеры нашли четыре (привязка в `match`, `Envelope` с flatten/borrow, `sticky_get`, возможно клонирование через guard) — все исправлены. Остальные «блокеры компиляции» из их списков опровергнуты по исходникам крейтов. Точное число правок до зелёного `cargo check` покажет только первая сборка на машине с Rust — версии крейтов (`wreq`, `metrics-exporter-prometheus`, `notify`) могли уйти вперёд.

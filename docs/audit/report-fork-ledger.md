# Журнал stable-форка fenix007/OmniRoute

Дата среза: 23.09.2026. Ветка `stable` = upstream v3.8.48 hotfix (`7ee5bbc64`, 13.07.2026) + **104 коммита** (19.07–22.09.2026), 435 файлов, +27 445 / −7 292 строк. Источники: `FORK.md` в ветке stable (69 КБ), `git log 7ee5bbc64..origin/stable`, `changelog.d/fixes/fork-*.md`.

> Исправление предыдущего аудита: утверждение «в форке нет собственных правок» было неверным — я сравнивал ветку `main`, которая по процессу форка только отслеживает upstream и никогда не деплоится. Продакшен собирается из `stable`.

## 1. Процесс форка

| Ветка / тег | Назначение |
|---|---|
| `stable` | продакшен-ветка, образы собираются только с неё |
| `main` | отслеживает upstream для PR, не деплоится |
| `3.8.48-fork.N` | релизные теги; сознательно без `v*`, чтобы унаследованные upstream-workflow (docker-publish, electron-release) не срабатывали |

Мотив заморозки (FORK.md): *«Upstream releases after 3.8.48 are unstable for our deployment (ai-router)»*. Релизы 3.8.50+ принесли крупные рефакторинги (`dispatchPrelude.ts`, `credentialPatterns.ts`, `runNonStreamingProviderLeg`), портировать их целиком нельзя — нужные фиксы адаптируются по одному через ревью upstream (регулярные «upstream review» 08.09, 09.09, 11.09, 21.09).

Образы: `.github/workflows/fork-image-fenix007.yml`, amd64 и arm64 в отдельных job'ах, публикация `ghcr.io/fenix007/omniroute:3.8.48-fork.N` (+ `stable`, `sha-*`); коммиты без тега помечаются `[skip ci]`. Деплой в ai-router: `OMNIROUTE_IMAGE` / `OMNIROUTE_VERSION` в `.env`, `make omniroute-update`.

Quality gates на каждое изменение: `check:file-size`, `check:complexity-ratchets` (baseline в `config/quality/complexity-baseline.json`), `typecheck:core`, ESLint с бюджетом `explicit-any`, `test:unit`, `test:coverage` ≥ 60 % (последний прогон: 80.98 % строк, 78.36 % веток, 23 310 тестов).

## 2. Релизы fork.N

| Тег | Дата | Содержимое |
|---|---|---|
| fork.1 | 20.08 | первичные cherry-pick: #7171, #7399, #8306, #10016 (адаптирован), порт #8307; CI; FORK.md |
| fork.2 | 20.08 | tier-1 бэкпорты Codex/SSE из 3.8.50: #6710, #7526, #7570, #7957, #8043, #9231, #10525 |
| fork.3 | 20.08 | tier-2 устойчивость combo + tier-3 квоты/rate-limit: #9207, #9328, #9342, #9164, #10034, #10137, #10217, #10116, #10534, #9708/#10792 |
| fork.4 | 20.08 | свои: keepalive commit window 2000→4000 мс, `annotateErrorFrameStatus`, Claude-shaped error frames на `/v1/messages`, `OMNIROUTE_STREAM_DEFAULT_MODE` |
| fork.6 | 21.08 | Responses-shaped error frames на `/v1/responses` (`response.failed`); фикс `normalizeStreamDefaultMode` |
| fork.7 | 21.08 | terminal marker в крупных чанках: до фикса 171 из 176 non-2xx call-log'ов были ложными 499 с нулём токенов — ломало учёт квоты в ai-router |
| fork.8–10 | 22.08 | Kiro: `response_format` в промпт, снятие JSON-фенса, детект JSON-контракта |
| fork.11 | 30.08 | связанный бэкпорт 7 PR из 3.8.50: #7277 (Responses stickiness), #7908/#8011 (client-abort lifecycle), #7825 (Codex-аккаунты с одним email), #11179 (реальное окно контекста), #8732 (guard пустого стрима), #8807, #8931 |
| fork.12–15 | 30.08–04.09 | Trae error status; немедленный failover инвалидированных OAuth; запрет коалесинга no-cache; incomplete→length; классификация невалидных промптов; abort таймаутов combo; безопасность OAuth/кэша/валидации |
| fork.16–17 | 03–04.09 | OpenCode Go: маршрутизация 34 моделей по протоколу; официальный API квоты |
| fork.18 | 04.09 | GPT-6 Astra; сроки Codex reset credits; версия форка в админке |
| fork.19–21 | 08–11.09 | upstream review: разбиение крупных модулей, #12863, #13059, #13110, #13274; SaluteSpeech |
| fork.22 | 14–15.09 | CLIProxy request fidelity; Codex WebSocket (Responses beta, strip token limits); приватное профилирование |
| fork.23 | 16–18.09 | стратегия `quota-deadline`; исправления usage-токенов; восстановление quality gates |
| fork.24 | 21.09 | #13738, #14052 (identity Codex 0.155.0); ожидание cooldown deadline |
| fork.25 | 22.09 | сохранение terminal failures, ограниченные ретраи модели; ретрай другого Codex-аккаунта до фолбэка на провайдера |

## 3. Классификация 104 коммитов

| Класс | Кол-во |
|---|---|
| A. Бэкпорты / адаптации из upstream (≈15 авторов upstream) | 30 |
| B. Собственная работа форка | 56 |
| C. CI / docs / quality / тесты | 18 |

Бэкпорты (A), по upstream PR: #7171 `a4026e05b`, #7399 `29259ed2b`, #8306 `e22fb0f41`, #10016 `83799b03f`, #8307 `07b60bb7d` (порт, PR open), #6710 `a9a3b0106`, #7526 `a0b4b8745`, #7570 `70cfe1c91`, #7957 `63d682b31`, #8043 `35287b5a6`, #9231 `651d86fd4`, #10525 `e3def43cf`, #9207 `0a7e404c1`, #9328 `16470cce3`, #9342 `e1ea352e3`, #9164 `e0f2ce472` (частично), #10034 `b3d4ac272`, #10137 `feb12f9c3`, #10217 `b04b85440`, #10116 `527596828`, #10534 `cc589387d`, #9708/#10792 `de2b0cdcc`, связка 3.8.50 `06cda56c3`, #12124 `7f414e5e0`, #12863 `e5ed4b10b`, #13059 `fc71f2da9`, #13274+#13110 `d076d7b8b`, #13738 `6d21ae025`, #14052 `43fd688a1`.

## 4. Темы собственной работы и бэкпортов

**Роутинг Codex-аккаунтов и failover** — `d5e638b40`, `397811109`, `0f092f480` (стратегия `quota-deadline`); `1c6a46a27` (ожидание cooldown deadline: таймер просыпался на 1 мс раньше, съедал единственный ретрай и отдавал 429); `de2b0cdcc` (ретрай transport-ошибок до вывода на том же аккаунте); `015b67465` (немедленный failover инвалидированных OAuth); `0a5f7f423` (другой аккаунт до фолбэка combo на провайдера, опция `config.retryCodexAccountOnTimeout`); `cc589387d`, `527596828`; `a39b546fa` (пересчёт плана при refresh); `21c7de476`, `0105b6f67` (reset credits); `43fd688a1` (identity 0.155.0); `ed0819032` (порт fidelity запросов из CLIProxyAPI); `bd2f872d7`, `8fc1b924b` (Codex WebSocket); `e7e9d0ee4`, `e56bf9ab7`; `07b60bb7d`, `651d86fd4` (картинки: ретрай по аккаунтам, refresh на 401).

**Корректность SSE / Responses** — `f96a5c159` (keepalive commit window), `098261b47`, `792285c91` (actionable и Responses-shaped error frames), `5b83791f9`, `25dd63bda` (дефолт стрима на деплой), `77184aca1` (terminal marker), `c500242f2`, `457b344c9`, `34f4d1ea5`, `28231896d` (usage-токены), `b45c4bd24` / `98dd69474` (terminal failures, ограниченные ретраи), `e95446809`, плюс бэкпорты Codex SSE #6710, #7526, #7570, #7957, #8043.

**Combo / circuit breaker / rate limit** — бэкпорты #9207, #9328, #9342, #9164, #10034, #10137, #10217, #10016; свои `35359a0ef`, `f2e04498c`, `4194ed923`, `7d05c4f0e`, `5e0317011`.

**Безопасность** — `b7d6a110a`, `214e68739`, `d076d7b8b`, `d4d86b148`, `75727e21e`, `0c13729e4`, `49e56057a`.

**Вне Codex** — xAI `29259ed2b`; Kiro `7a0f711d0`, `3c7b6123d`, `fe26399d1`; Trae `16a56a8d7`; Gemini `e5ed4b10b`, `fc71f2da9`, `6d21ae025`; OpenCode `67a9c3ea9`, `7f414e5e0`; аудио/TTS `397aedfa4`, `8c0c32482`, `b48742901`; картинки `8ddd8eea3`; GPT-6 Astra `f8bc41439`, `290a8e722`, `b359d86ff`.

**Производительность и диагностика** — `5c226c1e4` (ограничение фоновой экстракции фактов), `c67c00480` (приватное профилирование рантайма), `5a1b218e2`, `5e7c935d4` (кэш каталога моделей).

## 5. Что ушло в upstream

| Направление | Кол-во |
|---|---|
| upstream → форк (бэкпорты) | 30 |
| форк → upstream | 0 из 56 по колонке «Upstream PR» в FORK.md |

FORK.md декларирует «keep sending them upstream as PRs», но на срез ни у одного собственного патча PR не указан. Отложенных к портированию upstream PR — больше 25.

## 6. Известные проблемы и заметки из FORK.md

- Образ ~1.8 ГБ; однажды обновление забило диск на VPS до 100 % и SQLite упал с `disk I/O error`. Процедура: `df -h /` перед обновлением, удалять старые теги.
- `quota-deadline` — стратегия только для выбора аккаунта (не combo-стратегия). С `retryCodexAccountOnTimeout: true` combo разрешает один дополнительный dispatch на другой аккаунт до фолбэка на провайдера; при target-deadline 120 с суммарное ожидание Codex может дойти до 240 с.
- К fork.19 было 51 падающий тест, все закрыты к fork.24 (фиксированные sleep, CRLF в фикстурах, сетевой тест каталога).
- ai-router синхронизирует call-log в квоту и `/limits` через `lib/omniroute-call-log-sync.ts` — значит корректность usage/статусов в call-log для пользователя критична (это и был смысл фикса fork.7).
- Числа аккаунтов Codex и TTFT-замеров в FORK.md нет.

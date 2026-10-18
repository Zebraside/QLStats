# QLStats

QLStats собирает статистику игроков с сервера Quake Live через ZMQ-поток статистики и показывает её на веб-странице: блок «Текущий матч» (live), сводки за день и за всё время, таблицу участников.

- Требования и правила учёта: [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md)
- Архитектура и контракт API: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Формат событий: [docs/research/ql-zmq-events.md](docs/research/ql-zmq-events.md)

Один бинарник `qlstats` и один файл SQLite. Фронтенд (`static/`) вшивается в бинарник при сборке.

## Сборка

Нужен стабильный Rust (проверено на 1.99) и C-компилятор: крейт `zmq` собирает libzmq из исходников (vendored), `rusqlite` — SQLite (`bundled`). Системные libzmq/sqlite не нужны.

```sh
cargo build --release          # бинарник: target/release/qlstats
cargo test                     # unit + интеграционные (replay, HTTP)
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Эталоны интеграционных тестов лежат в `tests/golden/`. После осознанного изменения правил их можно перезаписать: `UPDATE_GOLDEN=1 cargo test --test replay`.

## Конфигурация

Переменные окружения или файл `.env` в рабочем каталоге (путь можно задать флагом `--env-file`). Уже заданные переменные окружения имеют приоритет над `.env`. Шаблон — `.env.example`.

| Переменная | По умолчанию | Назначение |
|------------|--------------|------------|
| `QL_ZMQ_HOST` | — | Адрес QL-сервера. Нужен для `run` и `record`. |
| `QL_ZMQ_PORT` | — | Порт `zmq_stats_port`. |
| `QL_ZMQ_USER` | `stats` | Логин PLAIN. |
| `QL_ZMQ_PASSWORD` | — | Пароль `zmq_stats_password`. В логи не пишется. |
| `QLSTATS_DB_PATH` | `./data/qlstats.db` | Файл SQLite (каталог создаётся). Флаг `--db` перекрывает. |
| `QLSTATS_HTTP_ADDR` | `0.0.0.0:8080` | Адрес HTTP-сервера. |
| `QLSTATS_TZ` | `Europe/Moscow` | Часовой пояс «дня» и полуночи. |
| `QLSTATS_LIVE_OFFLINE_SECS` | `15` | Через сколько секунд без связи live показывает «нет связи». |
| `QLSTATS_STATIC_DIR` | — | Dev-режим: отдавать статику с диска из этого каталога, а не вшитую. |
| `RUST_LOG` | `info` | Уровень логов (`tracing`), например `info,qlstats=debug`. |

## Команды

```sh
qlstats run                                   # collector + writer + web
qlstats recompute                             # пересчитать всё из raw_events (с бэкапом)
qlstats record --out file.jsonl [--duration-secs 60]
qlstats replay file.jsonl [--db path] [--dump]
qlstats replay file.jsonl --publish tcp://127.0.0.1:28556 [--speed 2] [--loop]
```

Общие флаги: `--env-file PATH`, `--db PATH`.

- **run** — подключается к ZMQ (PLAIN, переподключение, socket monitor), пишет каждое сообщение в `raw_events`, по `MATCH_REPORT` считает матч, держит live-состояние в памяти, отдаёт HTTP. При старте применяет миграции и восстанавливает live-состояние из `raw_events`. Останавливается по SIGINT/SIGTERM.
- **recompute** — делает бэкап `VACUUM INTO <db>.bak-YYYYMMDD-HHMMSS`, затем одной транзакцией удаляет `matches`, `match_players`, `match_player_weapons`, `deaths`, `players` и строит их заново из `raw_events`. Лучше запускать при остановленном сервисе. Работающий сервис увидит новые цифры после следующего матча, полуночи или перезапуска.
- **record** — пишет поток в JSONL того же формата, что `docs/research/sample-*.jsonl`. БД не трогает.
- **replay** — прогоняет JSONL через тот же writer-пайплайн и live-автомат (по умолчанию во временную БД). `--dump` печатает JSON с ключами `summary`, `players`, `live` (ответы API на момент последнего события) и `matches` (все строки `matches`, включая незасчитанные с `skip_reason`). Логи идут в stderr.
- **replay --publish** — фейковый QL-сервер: ZMQ PUB с PLAIN на указанном адресе, события отправляются с исходными интервалами (`--speed` ускоряет, `--loop` повторяет файл). Если задан `QL_ZMQ_PASSWORD`, встроенный ZAP-обработчик проверяет пароль. Удобно для локальной проверки: в одном терминале `qlstats replay docs/research/sample-long.jsonl --publish tcp://127.0.0.1:28556 --loop`, в другом `QL_ZMQ_HOST=127.0.0.1 QL_ZMQ_PORT=28556 qlstats --db /tmp/t.db run`.

## HTTP

| Путь | Что отдаёт |
|------|------------|
| `GET /api/live` | «Текущий матч» из памяти. |
| `GET /api/summary` | Сводка за день (по `QLSTATS_TZ`) и за всё время. |
| `GET /api/players` | Таблица участников. |
| `GET /healthz` | `200`, пока доступна БД, иначе `503`; состояние ZMQ в теле. |
| `GET /`, `/app.js`, `/style.css`, … | Любой файл из `static/`. |

Форматы — ARCHITECTURE.md, раздел 9. Ответы `/api/*` заранее сериализованы и сжаты (gzip) в снапшоте (`ArcSwap`), имеют `ETag` и `Cache-Control: no-cache`; при совпадении `If-None-Match` сервер отвечает `304`. Снапшот статистики пересобирается после каждого завершённого матча и в полночь; live — при изменении состояния (дебаунс 200 мс) и при смене флага offline.

## Деплой (systemd)

```sh
sudo useradd --system --home /var/lib/qlstats --create-home qlstats
sudo install -m 755 target/release/qlstats /usr/local/bin/qlstats
sudo install -m 600 -o qlstats .env /etc/qlstats.env     # QLSTATS_DB_PATH=/var/lib/qlstats/data/qlstats.db
```

`/etc/systemd/system/qlstats.service`:

```ini
[Unit]
Description=QLStats
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=qlstats
WorkingDirectory=/var/lib/qlstats
EnvironmentFile=/etc/qlstats.env
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/qlstats run
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload && sudo systemctl enable --now qlstats
journalctl -u qlstats -f
```

Бэкап: раз в сутки `sqlite3 qlstats.db "VACUUM INTO '/backup/qlstats-$(date +%F).db'"` (cron или systemd-timer). Копировать файл БД через `cp` при работающем процессе нельзя (WAL). Главная ценность — `raw_events`, остальное восстанавливает `qlstats recompute`. После обновления версии с изменёнными правилами учёта запустите `qlstats recompute`. Например, колонка «Любимое оружие» (миграция БД v2, поле `fav_weapon` в `/api/players`) для матчей, записанных до обновления, заполняется только после `recompute`; до этого у таких игроков `fav_weapon` равно `null`.

## Структура кода

```
src/
  main.rs       точка входа (cli::main)
  lib.rs        модули
  config.rs     env/.env, маскирование пароля
  model.rs      serde-типы событий, терпимые десериализаторы
  stats.rs      чистые правила учёта: классификация смертей, стрики, победители, точность, counted, day_key
  store.rs      SQLite: миграции, writer, finalize_match, recompute, запросы агрегатов
  live.rs       LiveState (автомат «Текущего матча»), LiveHub (RwLock + watch), восстановление
  collector.rs  ZMQ SUB, PLAIN, monitor, переподключение, record
  replay.rs     прогон JSONL, --dump, --publish (PUB + ZAP)
  web.rs        axum-роутер, снапшоты (ArcSwap, ETag, gzip), статика (rust-embed), полночь
  cli.rs        clap: run / recompute / record / replay
tests/
  fixtures/     sample-long.jsonl, sample-60s.jsonl (живой поток), synthetic.jsonl (синтетика)
  golden/       эталонные --dump
  replay.rs     golden, recompute == инкрементальный учёт, повторный прогон, _INVALID, incomplete
  http.rs       304 по ETag, gzip, формат JSON, смена суток, healthz, статика
```

Синтетическая фикстура `tests/fixtures/synthetic.jsonl` покрывает засчитанный FFA (3 человека + бот, ничья лидеров, вышедший игрок, все категории смертей, повторный REPORT, битый JSON), CA (`team_mode`), `aborted`, `no_start` и DUEL с победой по `WIN` сразу после полуночи по Москве.

## Отклонения от архитектуры

Пробелы и противоречия в документах закрыты так:

1. **Признак `incomplete`.** В документах не сказано, как узнать о разрыве соединения при пересчёте. Collector пишет в `raw_events` синтетические строки `_CONNECTED`/`_DISCONNECTED` (без `MATCH_GUID`), а `finalize_match` ставит `incomplete = 1`, если между первым событием матча и его `MATCH_REPORT` есть любая из них (`_CONNECTED` без `_DISCONNECTED` означает рестарт процесса после падения). Так `recompute` даёт тот же результат.
2. **`skip_reason = unsupported_mode`** — для режимов, которых нет ни в списке командных, ни среди FFA/DUEL/RACE. В схеме такого значения нет.
3. **Статика** вшивается через `rust-embed` (в документе `include_str!`): отдаётся любой файл из `static/`, скрытые файлы (`.gitkeep` и т. п.) не отдаются. `QLSTATS_STATIC_DIR` включает чтение с диска. ETag статики — по хэшу содержимого. gzip для статики и `/healthz` делает `CompressionLayer`, для `/api/*` gzip заранее посчитан в снапшоте.
4. **ETag** слабые (`W/"…"`) и содержат идентификатор запуска процесса, чтобы после рестарта старые ETag клиентов не давали ложный `304`. ETag `/api/live` — версия `LiveState`, флаг offline и, сверх документа, начало текущего обрыва (иначе два разных обрыва подряд дали бы один ETag со старым `offline_since`).
5. **`MATCH_STARTED` содержит `MATCH_GUID`** в живом потоке (в ql-zmq-events.md сказано обратное) — используется.
6. **Live: связь** считается установленной по `HANDSHAKE_SUCCEEDED` (а не по `CONNECTED`: TCP-соединение без успешного PLAIN ещё не связь) и по любому полученному фрейму.
7. **Live: фаза вне матча** выводится из состава: есть человек — «Разминка», нет — «Сервер пуст». Поэтому после рестарта посреди матча (без `MATCH_STARTED` в хвосте `raw_events`) показывается «Разминка» до следующего `MATCH_STARTED`.
8. **Live: зрители-люди** делают сервер «непустым»: если на сервере только зритель, состояние `warmup`, `players: []`, `spectators: 1`.
9. **Live: `PLAYER_STATS` с `QUIT = 1`** не добавляет игрока в список (иначе вышедшие возвращаются в список в конце матча).
10. **Live: `kills`** заполняется во всех режимах (как в примере FFA из раздела 9), `score` в командных режимах равен числу убийств соперников. Игроки отсортированы по счёту, затем по нику. `team` в некомандных режимах всегда `free`.
11. **Live: лимиты** — только ненулевые: `frag`, `time_min` всегда; `capture` для CTF/ONEFLAG, `round` для CA/FT/AD, `score` для DOM.
12. **Live: `PLAYER_DEATH` без warmup** с GUID, отличным от текущего матча, только добавляет неизвестных игроков, счёт не меняет. `MATCH_REPORT` с чужим GUID не закрывает идущий матч.
13. **Сводка:** если лучшее значение равно 0 (например, за день есть матчи, но побед нет ни у кого), `players: []`.
14. **Стрик:** смерть категории `ignored` по правилу 7 (оружейный `MOD` без `KILLER`) тоже обнуляет стрик жертвы — это собственная смерть, а не `SWITCHTEAM`.
15. **Участие:** смерть `SWITCHTEAM` без warmup тоже делает человека участником (буквально по 3.3).
16. **DUEL без `WIN`:** победитель — максимальный `SCORE` среди всех игроков матча, включая бота; если выиграл бот, победы нет ни у кого.
17. **Точность** в `/api/players` округлена до 4 знаков.
18. **Writer:** вставка в `raw_events` и upsert `players` — одна транзакция; при занятой БД (`SQLITE_BUSY`, например во время `recompute`) она повторяется каждые 0,5 с до 5 минут, чтобы не терять сырые события; `finalize_match` — отдельная `BEGIN IMMEDIATE`, чтобы ошибка расчёта матча не теряла сырое событие. Сигнал на пересборку снапшота шлётся после любого записанного матча (засчитанного или нет).
19. **Таблица `deaths`** содержит смерти без warmup только завершённых матчей (включая `ignored`).
20. **`record`** для неразобранного фрейма пишет `"type": "_INVALID"` вместе с `"raw"`.
21. **`replay`:** `--dump` дополнительно выводит `matches`; у `--publish` есть флаг `--loop` и ZAP-обработчик с проверкой пароля.
22. **Переподключение:** сокет и контекст пересоздаются, если 60 с нет успешного handshake или 30 мин тишины при живом соединении (WARN после 10 мин тишины); пауза между попытками растёт от 1 до 30 с.
23. **Переменные `QL_ZMQ_*`** обязательны только для `run` и `record`. Добавлена `QLSTATS_STATIC_DIR`.
24. **Смена суток** при запросе (`snapshot.day_key != today`) пересобирает снапшот синхронно (под мьютексом, один раз).

## Известные ограничения и недоделки

- Стресс-тест (ARCHITECTURE §12: k6, oha, замеры CPU/RSS) не проводился.
- Восстановление live после рестарта видит только события после последнего `MATCH_REPORT`: люди, которые остались на сервере и молчат, появятся в списке после своего первого события.
- Live-счёт приближённый (правила REQUIREMENTS 3.10); `TELEFRAG`/`KAMIKAZE` пока не встречались в живом потоке.
- `recompute` при работающем сервисе не уведомляет веб: новые цифры появятся после следующего матча, полуночи или рестарта.

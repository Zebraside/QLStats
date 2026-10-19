# QLStats

QLStats собирает статистику игроков с сервера Quake Live через ZMQ-поток статистики и показывает её на веб-странице: блок «Текущий матч» (live), сводки за день и за всё время, таблицу участников.

- Требования и правила учёта: [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md)
- Архитектура и контракт API: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Формат событий: [docs/research/ql-zmq-events.md](docs/research/ql-zmq-events.md)

Один бинарник `qlstats` и один файл SQLite. Фронтенд (`static/`) вшивается в бинарник при сборке.

Самый простой способ запустить на сервере — Docker-образ `totekm/qlstats`, см. раздел «Запуск в Docker».

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

Три источника, все необязательны:

- **TOML-файл** — только если задан флагом `--config PATH` или переменной `QLSTATS_CONFIG` (сама `QLSTATS_CONFIG` читается до загрузки `.env`, поэтому в `.env` её задавать бесполезно). Шаблон — `config.example.toml`. Неизвестные ключи — ошибка. В ошибке разбора указываются путь, строка и столбец, но не текст строки (там может быть пароль).
- **Переменные окружения** и файл `.env` в рабочем каталоге (путь можно задать флагом `--env-file`). Уже заданные переменные окружения имеют приоритет над `.env`. Пустое значение считается незаданным. Шаблон — `.env.example`.
- **Флаги CLI** `--db`, `--http-addr`.

Приоритет (строго): **флаги CLI > переменные окружения (включая `.env`) > TOML-файл > значения по умолчанию**. Источники смешиваются по полям: например, хост и порт можно задать в TOML, а пароль — в `QL_ZMQ_PASSWORD`.

| TOML-ключ | Переменная | Флаг | По умолчанию | Назначение |
|-----------|------------|------|--------------|------------|
| `zmq.host` | `QL_ZMQ_HOST` | — | — | Адрес QL-сервера. Нужен для `run` и `record`. |
| `zmq.port` | `QL_ZMQ_PORT` | — | — | Порт `zmq_stats_port`. Обязателен, если задан хост. |
| `zmq.user` | `QL_ZMQ_USER` | — | `stats` | Логин PLAIN. |
| `zmq.password` | `QL_ZMQ_PASSWORD` | — | — | Пароль `zmq_stats_password`. Обязателен, если задан хост. В логи и тексты ошибок не попадает. |
| `database.path` | `QLSTATS_DB_PATH` | `--db` | `./data/qlstats.db` | Файл SQLite (каталог создаётся). |
| `http.listen` | `QLSTATS_HTTP_ADDR` | `--http-addr` | `0.0.0.0:8080` | Адрес HTTP-сервера. |
| `general.tz` | `QLSTATS_TZ` | — | `Europe/Moscow` | Часовой пояс «дня» и полуночи. |
| `general.live_offline_secs` | `QLSTATS_LIVE_OFFLINE_SECS` | — | `15` | Через сколько секунд без связи live показывает «нет связи». |
| `general.static_dir` | `QLSTATS_STATIC_DIR` | — | — | Dev-режим: отдавать статику с диска из этого каталога, а не вшитую. |
| `http.rate_limit_per_sec` | `QLSTATS_RATE_LIMIT_PER_SEC` | — | `10` | Per-IP лимит: сколько запросов в секунду восстанавливается в квоте клиента. `0` — per-IP лимит выключен. |
| `http.rate_limit_burst` | `QLSTATS_RATE_LIMIT_BURST` | — | `60` | Ёмкость per-IP квоты: сколько запросов подряд можно сделать с одного IP. Не меньше `1`. |
| `http.max_concurrency` | `QLSTATS_MAX_CONCURRENCY` | — | `256` | Предел одновременно обрабатываемых запросов на весь сервер, сверх него — `503`. `0` — без предела, максимум `1000000`. |
| `http.trust_proxy` | `QLSTATS_TRUST_PROXY` | — | `false` | Брать IP клиента из `X-Forwarded-For` / `X-Real-IP` / `Forwarded`. Только за обратным прокси, см. «Ограничение нагрузки». Принимает `true/false`, `1/0`, `yes/no`, `on/off`. |
| `http.request_timeout_secs` | `QLSTATS_REQUEST_TIMEOUT_SECS` | — | `10` | Таймаут обработки запроса, сверх него — `503`. `0` — без таймаута. |
| — | `RUST_LOG` | — | `info` | Уровень логов (`tracing`), например `info,qlstats=debug`. |
| — | `QLSTATS_CONFIG` | `--config` | — | Путь к TOML-файлу. Пустое значение — файл не читается (так в Docker отключают конфиг, заданный в образе). |

## Команды

```sh
qlstats run                                   # collector + writer + web
qlstats recompute                             # пересчитать всё из raw_events (с бэкапом)
qlstats record --out file.jsonl [--duration-secs 60]
qlstats replay file.jsonl [--db path] [--dump]
qlstats replay file.jsonl --publish tcp://127.0.0.1:28556 [--speed 2] [--loop]
```

Общие флаги (ставятся до или после подкоманды):

- `--config PATH` — TOML-файл конфигурации (или `QLSTATS_CONFIG`);
- `--env-file PATH` — файл с переменными окружения (по умолчанию `./.env`, если есть);
- `--db PATH` — файл SQLite, перекрывает `QLSTATS_DB_PATH` и `database.path`;
- `--http-addr ADDR` — адрес HTTP-сервера, перекрывает `QLSTATS_HTTP_ADDR` и `http.listen`.

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

### Ограничение нагрузки

Все пути, включая `/healthz`, проходят через одни и те же слои (снаружи внутрь): журнал запросов → таймаут → предел одновременных запросов → per-IP лимит → gzip → обработчик. Параметры — в таблице раздела «Конфигурация» (секция `[http]`).

- **Per-IP лимит** (`tower_governor`, алгоритм GCRA): с одного IP можно сделать `rate_limit_burst` запросов подряд, дальше квота восстанавливается со скоростью `rate_limit_per_sec` в секунду. Сверх квоты — `429 Too Many Requests` с заголовком `Retry-After` (секунды, не меньше 1). Записи о давно не приходивших IP удаляются раз в минуту.
- **Перегрузка**: одновременно обрабатывается не больше `max_concurrency` запросов на весь сервер; лишний запрос сразу получает `503 Service Unavailable` с `Retry-After: 1`, очередь не копится.
- **Таймаут**: запрос, который обрабатывается дольше `request_timeout_secs`, прерывается с ответом `503`.

Умолчания (10 запросов/с, 60 подряд, 256 одновременных) рассчитаны с большим запасом: зритель опрашивает API примерно раз в несколько секунд, а в нагрузочном тесте 300 зрителей давали около 85 RPS на весь сервер (`docs/perf/2026-10-18-stress.md`).

**Один IP — много зрителей.** Лимит считается по IP, поэтому зрители за одним NAT (локальная сеть, общежитие, мобильный оператор с CGNAT) делят одну квоту. Если такое ожидается, поднимите `rate_limit_burst` и `rate_limit_per_sec` или выключите per-IP лимит (`rate_limit_per_sec = 0`). То же самое, только хуже, происходит за обратным прокси: все запросы приходят с `127.0.0.1`, и без `trust_proxy` весь сайт получает одну квоту на всех — за nginx включайте `trust_proxy = true`.

**`trust_proxy` только за прокси.** При `trust_proxy = true` IP клиента берётся из `X-Forwarded-For` (первый адрес в списке), затем `X-Real-IP`, затем `Forwarded`, и только если их нет — из адреса соединения. Если сервис при этом доступен напрямую, без прокси, любой клиент подставит в заголовок произвольный IP и обойдёт лимит. Поэтому слушайте только `127.0.0.1` (`http.listen = "127.0.0.1:8080"`) или закройте порт фаерволом, а в nginx **перезаписывайте** заголовок, а не дописывайте к нему: с `$proxy_add_x_forwarded_for` первым в списке останется адрес, присланный самим клиентом.

**От DDoS приложение не защищает.** Эти лимиты отсекают одного-нескольких назойливых клиентов и не дают сервису упасть под всплеском запросов. От объёмных атак — насыщение канала, SYN-flood, тысячи IP ботнета — они не помогают: пакеты доходят до машины, занимают канал и сокеты раньше, чем запрос попадёт в приложение. Это задача уровня перед сервисом: nginx (`limit_req`, `limit_conn`), фаервол (`nftables`/`iptables`, SYN cookies) или CDN вроде Cloudflare. Пример для nginx:

```nginx
# http { ... }
limit_req_zone  $binary_remote_addr zone=qlstats_req:10m rate=10r/s;
limit_conn_zone $binary_remote_addr zone=qlstats_conn:10m;

server {
    listen 80;
    server_name stats.example.org;

    location / {
        limit_req  zone=qlstats_req burst=60 nodelay;
        limit_conn qlstats_conn 20;
        limit_req_status  429;
        limit_conn_status 429;

        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host            $host;
        proxy_set_header X-Real-IP       $remote_addr;
        proxy_set_header X-Forwarded-For $remote_addr;   # перезаписать, не дописывать
    }
}
```

При такой схеме сервис запускают с `http.listen = "127.0.0.1:8080"` и `trust_proxy = true`.

## Запуск в Docker

Готовый образ лежит на Docker Hub: `totekm/qlstats` (платформы `linux/amd64` и `linux/arm64`). Внутри только скомпилированный бинарник `qlstats` поверх `distroless/cc` (нет shell и пакетного менеджера), процесс работает от непривилегированного пользователя `nonroot` (uid/gid 65532). Фронтенд вшит в бинарник.

Контейнеру нужна одна папка, примонтированная в `/data`. В ней лежит конфиг `config.toml`, туда же приложение складывает базу `qlstats.db` (и её WAL-файлы `*.db-wal`, `*.db-shm`, бэкапы `recompute`), поэтому база переживает пересоздание контейнера и обновление образа.

### Шаг 1. Папка и конфиг

```bash
sudo mkdir -p /srv/qlstats
```

`/srv/qlstats/config.toml` (все параметры необязательны, полный список — в таблице раздела «Конфигурация»):

```toml
[database]
path = "/data/qlstats.db"      # внутри контейнера; /data — примонтированная папка

[zmq]                          # QL-сервер: zmq_stats_enable 1, zmq_stats_port, zmq_stats_password
host = "1.2.3.4"
port = 27960
password = "changeme"
# user = "stats"               # логин PLAIN, по умолчанию stats

[http]
listen = "0.0.0.0:8080"        # не менять: порт наружу задаётся через -p при запуске
# trust_proxy = true           # только если перед контейнером nginx (см. «Ограничение нагрузки»)

[general]
tz = "Europe/Moscow"           # часовой пояс «дня» для сводок
```

### Шаг 2. Запуск

```bash
docker run -d --name qlstats --restart unless-stopped \
  --user "$(id -u):$(id -g)" \
  -p 8080:8080 \
  -v /srv/qlstats:/data \
  totekm/qlstats:latest
```

- `--user` запускает процесс от вашего пользователя, чтобы он мог читать `config.toml` и писать базу в `/srv/qlstats` (папка должна принадлежать вам: `sudo chown -R "$(id -u):$(id -g)" /srv/qlstats`).
- `-p 8080:8080` — слева порт, который открыт на сервере (фаервол, панель хостинга). Замените `8080` слева на него, например `-p 27961:8080`; справа всегда `8080`, это порт внутри контейнера.

Проверка (порт — тот, что указали слева в `-p`):

```bash
curl -s http://localhost:8080/healthz
```

```bash
docker logs -f qlstats
```

`/healthz` отвечает `200`, пока доступна БД; в теле — состояние связи с ZMQ. Страница статистики — `http://<сервер>:8080/`. Если перед сервисом стоит nginx, публикуйте порт только на loopback: `-p 127.0.0.1:8080:8080` и включите `trust_proxy = true` в конфиге.

То же через Docker Compose: в репозитории лежит готовый [compose.yaml](compose.yaml). Скопируйте его на сервер, поправьте `user`, порт и путь к папке, затем:

```bash
docker compose up -d
```

### Обновление

```bash
docker pull totekm/qlstats:latest && docker rm -f qlstats
```

и снова `docker run …` из шага 2 (или `docker compose pull && docker compose up -d`). База и конфиг остаются в `/srv/qlstats`. Если в новой версии изменились правила учёта, после обновления выполните `recompute` (ниже).

### Другие команды

Точка входа образа — сам `qlstats`, поэтому аргументы после имени образа передаются ему. Конфиг `/data/config.toml` подхватывается автоматически (в образе задано `QLSTATS_CONFIG=/data/config.toml`).

```bash
docker run --rm -v /srv/qlstats:/data totekm/qlstats:latest --help
```

Пересчёт статистики из сырых событий (при остановленном сервисе; делает бэкап `qlstats.db.bak-<дата>` рядом с базой):

```bash
docker stop qlstats && docker run --rm -v /srv/qlstats:/data totekm/qlstats:latest recompute && docker start qlstats
```

Бэкап БД работающего сервиса — через `sqlite3` на хосте (копировать файл `cp` нельзя, см. раздел «Деплой»). Это безопасно на Linux-сервере, где контейнер и `sqlite3` работают на одном ядре. На macOS с Docker Desktop или colima базу работающего контейнера с хоста открывать нельзя: том проходит через virtiofs, блокировки SQLite не работают между VM и хостом, файл повреждается.

```bash
sqlite3 /srv/qlstats/qlstats.db "VACUUM INTO '/backup/qlstats-$(date +%F).db'"
```

### Параметры контейнера

| Что | Значение | Зачем |
|-----|----------|-------|
| `-v <папка>:/data` | обязательный том | `config.toml`, база `qlstats.db`, бэкапы. Рабочий каталог процесса — `/data`, относительные пути в конфиге считаются от него. |
| `-p <порт>:8080` | публикация HTTP | Внутри контейнер слушает `0.0.0.0:8080` (`http.listen`). Снаружи порт любой; `127.0.0.1:8080:8080` — только для nginx на том же хосте. |
| `-e RUST_LOG=…` | уровень логов | По умолчанию `info`; `info,qlstats=debug` — подробнее. Логи идут в stdout, смотреть `docker logs`. |
| `-e QL_ZMQ_HOST=…` и другие переменные | переопределение конфига | Любой параметр из таблицы «Конфигурация» можно задать переменной окружения; она перекрывает значение из `config.toml`. |
| `-e QLSTATS_CONFIG=` (пустое) | без TOML | Отключает конфиг, зашитый в образ; тогда всё берётся из переменных окружения или файла `/data/.env` (он читается автоматически). |
| `--user uid:gid` | пользователь процесса | Без флага — `65532:65532` (`nonroot`). Папка `/data` должна быть доступна этому пользователю на запись, поэтому в шаге 2 передаётся ваш uid:gid. |
| аргументы после образа | команда `qlstats` | По умолчанию `run`; также `recompute`, `replay`, `record`, `--help`. |

Исходящие соединения: контейнеру нужен доступ к QL-серверу по TCP на `zmq.port`. Входящие — только HTTP-порт.

### Сборка образа самостоятельно

```bash
docker buildx build --platform linux/amd64 -t totekm/qlstats:latest .
```

Сборка кросс-компилирует под целевую платформу без эмуляции, поэтому с arm-мака образ под x86-сервер собирается за несколько минут. Публикация: `docker login`, затем та же команда с `--push`. Multi-arch вариант `--platform linux/amd64,linux/arm64` работает только вместе с `--push` (локальное хранилище Docker не принимает список манифестов без containerd image store).

## Деплой (systemd)

```sh
sudo useradd --system --home /var/lib/qlstats --create-home qlstats
sudo install -m 755 target/release/qlstats /usr/local/bin/qlstats
sudo install -m 600 -o qlstats .env /etc/qlstats.env     # QLSTATS_DB_PATH=/var/lib/qlstats/data/qlstats.db
```

Вместо `EnvironmentFile` можно использовать TOML (`database.path = "/var/lib/qlstats/data/qlstats.db"` и т. д.):

```sh
sudo install -d -m 755 /etc/qlstats
sudo install -m 600 -o qlstats config.toml /etc/qlstats/config.toml   # из config.example.toml
```

и в юните `ExecStart=/usr/local/bin/qlstats --config /etc/qlstats/config.toml run` без строки `EnvironmentFile`. Переменные окружения, если заданы, всё равно перекрывают значения из файла.

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
# Запас по файловым дескрипторам: каждое HTTP-соединение — один fd.
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload && sudo systemctl enable --now qlstats
journalctl -u qlstats -f
```

Время в БД. `raw_events.received_at` — Unix-время в миллисекундах UTC; его выставляет получатель (collector, `replay`) в момент приёма фрейма, это не время сервера QL. Колонка проиндексирована (`raw_events_received_at`, миграция v3), поэтому выборки по периоду дешёвые. Матчи за период фильтруются по `matches.finished_at` (мс UTC, индекс `matches_finished_at`; `started_at` пуст у матча без `MATCH_STARTED`, такой матч не засчитывается). «День» для сводок считается по `QLSTATS_TZ` (`matches.day_key`).

Бэкап: раз в сутки `sqlite3 qlstats.db "VACUUM INTO '/backup/qlstats-$(date +%F).db'"` (cron или systemd-timer). Копировать файл БД через `cp` при работающем процессе нельзя (WAL). Главная ценность — `raw_events`, остальное восстанавливает `qlstats recompute`. События хранятся как компактный JSON; базу, записанную версией до 2026-10-18 (отступы сохранялись как есть), можно ужать один раз при остановленном сервисе: `sqlite3 qlstats.db "UPDATE raw_events SET payload = json(payload) WHERE json_valid(payload); VACUUM;"`. После обновления версии с изменёнными правилами учёта запустите `qlstats recompute`. Например, колонка «Любимое оружие» (миграция БД v2, поле `fav_weapon` в `/api/players`) для матчей, записанных до обновления, заполняется только после `recompute`; до этого у таких игроков `fav_weapon` равно `null`.

## Структура кода

```
src/
  main.rs       точка входа (cli::main)
  lib.rs        модули
  config.rs     слои конфигурации TOML → env/.env → CLI (PartialConfig, resolve), маскирование пароля
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
23. **Переменные `QL_ZMQ_*`** обязательны только для `run` и `record`. Добавлена `QLSTATS_STATIC_DIR`. Сверх документа — TOML-файл (`--config`) и флаг `--http-addr`.
24. **Смена суток** при запросе (`snapshot.day_key != today`) пересобирает снапшот синхронно (под мьютексом, один раз).

## Известные ограничения и недоделки

- Стресс-тест (ARCHITECTURE §12: k6, oha, замеры CPU/RSS) не проводился.
- Восстановление live после рестарта видит только события после последнего `MATCH_REPORT`: люди, которые остались на сервере и молчат, появятся в списке после своего первого события.
- Live-счёт приближённый (правила REQUIREMENTS 3.10); `TELEFRAG`/`KAMIKAZE` пока не встречались в живом потоке.
- `recompute` при работающем сервисе не уведомляет веб: новые цифры появятся после следующего матча, полуночи или рестарта.

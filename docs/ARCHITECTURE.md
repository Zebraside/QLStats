# QLStats — архитектура

Требования и правила учёта описаны в [REQUIREMENTS.md](REQUIREMENTS.md), формат событий — в [research/ql-zmq-events.md](research/ql-zmq-events.md).

## 1. Общая схема

```
 QL server (PUB, PLAIN "stats")
        │  tcp, JSON-фреймы
        ▼
 ┌──────────────────────── qlstats (один процесс) ────────────────────────┐
 │                                                                        │
 │  collector (OS-поток, blocking zmq, poll 1s, reconnect, monitor)       │
 │        │ ├─ model::parse → live::LiveState.apply(event)  (в памяти)    │
 │        │ │     monitor: connected/disconnected → LiveState.link        │
 │        │ │     └─ watch → web: /api/live (JSON + ETag в ArcSwap)       │
 │        │ RawMessage{received_at, bytes}                                │
 │        ▼  mpsc                                                         │
 │  store::writer (OS-поток, единственное write-соединение SQLite)        │
 │     1. INSERT raw_events  (всегда, до парсинга)                        │
 │     2. model::parse → upsert players(last_name)                        │
 │     3. TYPE=MATCH_REPORT → stats::finalize_match(guid)  ─┐ одна        │
 │          читает raw_events по GUID, применяет правила,   │ транзакция  │
 │          пишет matches / match_players / deaths ...     ─┘             │
 │     4. сигнал "data changed"                                           │
 │        │                                                               │
 │        ▼  watch                                                        │
 │  web::snapshot (пересборка агрегатов SQL-запросами → готовый JSON      │
 │                 + ETag, хранится в ArcSwap; также в 00:00 МСК)         │
 │        │                                                               │
 │        ▼                                                               │
 │  web (tokio + axum): /api/live /api/summary /api/players /healthz /    │
 └────────────────────────────────────────────────────────────────────────┘
        │ HTTP: /api/live, /api/summary, /api/players один раз при загрузке страницы
        ▼
 браузеры участников (vanilla JS)
```

Сырые события пишутся в базу раньше любой обработки. Нормализованные таблицы — производные: `qlstats recompute` удаляет их и строит заново, прогоняя `raw_events` через ту же функцию `finalize_match`. Live-состояние («Текущий матч») живёт только в памяти и обновляется прямо в collector'е, минуя БД (раздел 8).

## 2. Структура кода

Используется **один crate** (`lib.rs` + `main.rs`) с модулями, без workspace. Бинарник один, кода немного, публиковать части отдельно не нужно. Workspace добавил бы манифестов и усложнил сборку, а границы между частями обеспечивают модули и `pub(crate)`. Логика живёт в `lib.rs`, поэтому интеграционные тесты в `tests/` вызывают её напрямую.

| Модуль | Ответственность |
|--------|-----------------|
| `config` | Чтение env/.env (`dotenvy`), валидация, маскирование пароля в `Debug`. |
| `model` | serde-типы событий. Терпимые десериализаторы: `bool` из `true/false/0/1`, `SteamId` из строки или числа (хранится строкой), `#[serde(default)]`, неизвестные поля игнорируются, неизвестный `TYPE` даёт `Event::Other`. |
| `collector` | ZMQ SUB: подключение, PLAIN, переподключение, socket monitor, режим записи в JSONL. |
| `store` | SQLite: миграции (`PRAGMA user_version`), writer-поток, запросы на чтение для агрегатов. |
| `live` | `LiveState`: автомат состояний «Текущего матча», live-счёт, список участников, восстановление из `raw_events` при старте. Чистая логика плюс обёртка `RwLock` + `watch`. |
| `stats` | Чистые функции правил учёта: классификация смертей, стрики, победители, точность, критерии `counted`, day-key. Ввода-вывода нет, поэтому покрываются unit-тестами. |
| `web` | axum-роутер, снапшот (кэш JSON + ETag), статика, таймер полуночи. |
| `cli` | `clap`: подкоманды `run`, `recompute`, `record`, `replay`. |

## 3. Библиотеки

| Крейт | Зачем | Обоснование |
|-------|-------|-------------|
| `zmq` (rust-zmq) | ZMQ SUB | Обёртка над libzmq (установлен `brew zeromq 4.3.5`, на Linux — `libzmq5`/`libzmq3-dev`). Поддерживает PLAIN, `ZAP_DOMAIN`, `RECONNECT_IVL` и socket monitor. API блокирующий, поэтому крейт работает в отдельном OS-потоке. Для статической сборки есть feature `vendored`: libzmq собирается из исходников. |
| ~~`zeromq`~~ (чистый Rust) | альтернатива | Не нужна системная libzmq, есть async API. Не выбран: механизм PLAIN в нём не реализован или не отлажен, а без PLAIN к серверу не подключиться. Совместимость с libzmq-сервером тоже не проверена. |
| `tokio` | runtime | Нужен для axum, таймеров и каналов. |
| `axum` + `tower-http` | HTTP | Минималистичный и быстрый. `tower-http` даёт gzip (`CompressionLayer`) и `TraceLayer`. |
| `rusqlite` (`bundled`) | SQLite | Выбран вместо `sqlx`. SQLite вкомпилирована в бинарник, поэтому версия предсказуема и системная зависимость не нужна. API синхронный и простой, проверка запросов при компиляции и async-драйвер не нужны: запись идёт из одного потока, чтение — редкая пересборка снапшота. |
| `serde`, `serde_json` | JSON | Разбор событий и ответы API. |
| `chrono` + `chrono-tz` | время | Перевод UTC в `Europe/Moscow` и вычисление следующей полуночи. |
| `arc-swap` | снапшот | Неблокирующее чтение готового ответа из сотен параллельных запросов. |
| `tracing` + `tracing-subscriber` | логи | Структурированные логи, фильтр через `RUST_LOG`. |
| `clap` (derive) | CLI | Подкоманды. |
| `dotenvy` | `.env` | Загрузка конфигурации. |
| `anyhow` / `thiserror` | ошибки | `anyhow` на верхнем уровне, `thiserror` в модулях. |

## 4. Фронтенд

Одна статическая страница: `index.html`, `app.js`, `style.css`. Это vanilla HTML/CSS/JS без фреймворка и без сборки. Файлы вшиваются в бинарник через `include_str!` (если файлов станет больше — `rust-embed`) и отдаются axum.

`app.js`:
- запрашивает `/api/summary`, `/api/players` и `/api/live` один раз при загрузке страницы; автообновления, таймеров и `If-None-Match` на клиенте нет (решение владельца, REQUIREMENTS §4). Новые данные — только после перезагрузки страницы, о чём говорит подпись «данные на ЧЧ:ММ:СС» в шапке;
- время с начала матча вычисляется один раз от `started_at` с поправкой на часы сервера (заголовок `Date`) и дальше не тикает;
- рендерит два блока сводки и таблицу;
- сортирует таблицу на клиенте по клику на заголовок (повторный клик меняет направление; `null`-точность всегда внизу);
- выбранную сортировку сохраняет в `localStorage`.

Визуальный макет делается отдельно в Claude Design и переносится в эти три файла. Контракт с макетом — JSON API (раздел 9).

## 5. Схема БД

Режим `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`. Все времена хранятся в UTC, в миллисекундах.

```sql
raw_events(
  id          INTEGER PRIMARY KEY,      -- порядок получения
  received_at INTEGER NOT NULL,         -- UTC ms
  type        TEXT NOT NULL,            -- TYPE или '_INVALID'
  match_guid  TEXT,                     -- NULL, если нет/не разобран
  payload     TEXT NOT NULL             -- исходный JSON без отступов (lossy UTF-8)
);  -- INDEX (match_guid, id), INDEX (type)

matches(
  guid          TEXT PRIMARY KEY,       -- дедупликация повторного REPORT
  game_type     TEXT, map TEXT,
  started_at    INTEGER,                -- received_at MATCH_STARTED, NULL если не было
  finished_at   INTEGER NOT NULL,       -- received_at MATCH_REPORT
  day_key       TEXT NOT NULL,          -- 'YYYY-MM-DD' по Europe/Moscow от finished_at
  aborted       INTEGER NOT NULL,
  exit_msg      TEXT, game_length INTEGER,
  human_players INTEGER NOT NULL,
  counted       INTEGER NOT NULL,
  skip_reason   TEXT,                   -- aborted|no_start|team_mode|race|lt2_humans; NULL если counted
  incomplete    INTEGER NOT NULL DEFAULT 0, -- 1, если между событиями матча был разрыв ZMQ (решение 6.1)
);  -- INDEX (counted, day_key)

match_players(                          -- только люди; строки пишутся и для неучтённых матчей
  match_guid TEXT, steam_id TEXT, PRIMARY KEY (match_guid, steam_id),
  name TEXT,
  has_stats INTEGER,                    -- был ли PLAYER_STATS (non-warmup)
  score INTEGER, rank INTEGER, win_raw INTEGER, quit INTEGER,
  max_streak_reported INTEGER,          -- MAX_STREAK, справочно
  weapons TEXT,                         -- WEAPONS как есть (JSON)
  -- вычислено stats:: по deaths этого матча
  kills INTEGER, deaths INTEGER, suicides INTEGER, env_deaths INTEGER,
  best_streak INTEGER,
  is_winner INTEGER,                    -- по правилам 3.6
  weapon_kills TEXT                     -- засчитанные kills по оружию из MOD: {"RAILGUN":12,...};
                                        -- NULL, если нет; миграция v2 (ALTER TABLE ... ADD COLUMN)
);

match_player_weapons(                   -- для SQL-агрегации точности
  match_guid TEXT, steam_id TEXT, weapon TEXT, shots INTEGER, hits INTEGER,
  PRIMARY KEY (match_guid, steam_id, weapon)
);

deaths(
  id INTEGER PRIMARY KEY,
  raw_event_id INTEGER REFERENCES raw_events(id),
  match_guid TEXT, time INTEGER,        -- TIME из события
  victim_steam_id TEXT, killer_steam_id TEXT,  -- killer NULL, если KILLER нет
  mod TEXT,
  category TEXT CHECK (category IN ('kill','suicide','environment','ignored'))
);

players(steam_id TEXT PRIMARY KEY, last_name TEXT, last_seen INTEGER);
```

**Агрегаты не материализуются.** Таблица участников и сводки строятся SQL-запросами по `match_players JOIN matches WHERE counted = 1` (суммы, `MAX(best_streak)`, `SUM(is_winner)`) и по `match_player_weapons` (точность: `SUM(hits)/SUM(shots)` по оружию при `SUM(shots) > 0`, затем `AVG`). Любимое оружие: `weapon_kills` засчитанных матчей суммируются по игроку в Rust, выбор с тай-брейком делает `stats::favorite_weapon`. Запросы выполняются только при пересборке снапшота: после засчитанного матча и в полночь. На объёмах одного сервера (тысячи матчей) это миллисекунды. Пересчётная логика остаётся в одном месте, а `recompute` не синхронизирует отдельные агрегатные таблицы. Если объём вырастет, можно добавить таблицу `player_totals`, которая обновляется в транзакции матча.

## 6. Поток обработки матча

1. **Приём.** Collector получает фрейм и передаёт `(received_at, bytes)` в writer.
2. **Запись сырого события.** Writer делает `INSERT INTO raw_events`, извлекая `TYPE` и `DATA.MATCH_GUID` минимальным разбором. Сервер присылает JSON с трёхпробельными отступами; в базу пишется компактная форма (`serde_json::to_string`, порядок ключей сохраняется) — это в 1,8 раза меньше без потери данных. Если разбор не удался, тип пишется как `_INVALID`, а payload — как есть.
3. **Ник.** Для событий с парой `(STEAM_ID != 0, NAME)` выполняется `UPSERT players` с `last_seen = received_at`.
4. **Буферизация по `MATCH_GUID`.** В памяти события не буферизуются: буфер — это `raw_events` с индексом `(match_guid, id)`. Поэтому перезапуск процесса посреди матча ничего не теряет, а инкрементальный путь и `recompute` совпадают.
5. **Завершение матча** по `MATCH_REPORT`. Внутри одной транзакции `BEGIN IMMEDIATE … COMMIT` выполняется `finalize_match(guid, report_raw_id)`:
   - если `matches.guid` уже есть, транзакция завершается: повторный REPORT игнорируется;
   - читаются `raw_events WHERE match_guid = ? AND id <= report_raw_id ORDER BY id`;
   - собираются `MATCH_STARTED` (`id < report_raw_id`), `PLAYER_STATS` и `PLAYER_DEATH` без warmup;
   - `stats::` классифицирует смерти, определяет людей-участников, считает per-match `kills`/`deaths`/`suicides`/`env_deaths`/`best_streak`, засчитанные `kills` по оружию из `MOD` (`weapon_kills`), победителей и оружие;
   - `counted` вычисляется по правилам 3.1: `!aborted`, есть `MATCH_STARTED`, режим FFA/DUEL, людей ≥ 2. При невыполнении записывается `skip_reason`;
   - пишутся `matches`, `match_players`, `match_player_weapons`, `deaths`;
   - `COMMIT`. При ошибке — `ROLLBACK` и запись в лог. Сырые события остаются, матч можно пересчитать позже.
6. **Уведомление.** После `COMMIT` засчитанного матча writer шлёт сигнал в `tokio::sync::watch`, и web пересобирает снапшот.

Неполный поток обрабатывается так. Если до `MATCH_REPORT` нет `MATCH_STARTED`, матчу ставится `skip_reason = no_start`. Если `MATCH_STARTED` есть, а `MATCH_REPORT` не пришёл, матч не завершается и в статистике не появляется. Его события лежат в `raw_events` и не мешают.

## 7. День и полночь по Москве

- `day_key` вычисляется один раз при записи матча: `finished_at → Europe/Moscow → YYYY-MM-DD`.
- В «Статистику дня» попадают засчитанные матчи с `day_key = today_msk()`, где `today_msk()` берётся из инжектируемых часов (trait `Clock`, в тестах — фиксированное время).
- Снапшот хранит свой `day_key`. Фоновая задача спит до следующей полуночи по Москве (вычисляется через `chrono-tz`) и пересобирает снапшот. Дополнительно при каждом запросе проверяется, что `snapshot.day_key == today_msk()`; при расхождении снапшот пересобирается, что страхует от сдвига часов.

## 8. Live-состояние («Текущий матч»)

Правила описаны в REQUIREMENTS, раздел 3.10. Схема БД для live не меняется: состояние не хранится, а для восстановления после рестарта используется `raw_events`.

**Структура:**

```rust
struct LiveState {
    link: Link,                    // Connected | Disconnected { since }
    phase: Phase,                  // Empty | Warmup | InProgress
    guid: Option<String>,          // текущий MATCH_GUID
    server_title: Option<String>,
    match_info: Option<MatchInfo>, // map, game_type, factory_title, limits, started_at
    players: BTreeMap<SteamKey, LivePlayer>, // name, bot, team, score: Option<i32>, kills
    spectators: u32,               // производное от players с team = spectator
    version: u64,                  // растёт при каждом изменении → ETag
}
```

`SteamKey` — это `STEAM_ID`, а для ботов (`"0"`) — `bot:<NAME>`, потому что у всех ботов один и тот же `STEAM_ID`.

**Кто обновляет.**
- Collector после приёма фрейма разбирает событие и вызывает `live.apply(&event, received_at)` под `RwLock::write` (операция занимает микросекунды). Затем он передаёт сырой фрейм writer'у. Так live не зависит от задержек SQLite.
- Монитор ZMQ-сокета обновляет `link`.
- Если `version` изменилась, отдельная tokio-задача, разбуженная через `watch`, сериализует снапшот в JSON (с дебаунсом 200 мс), вычисляет ETag и кладёт результат в `ArcSwap`. Обработчик `/api/live` только читает `ArcSwap`.

**Переходы состояний:**

| Событие | Действие |
|---------|----------|
| `MATCH_STARTED` | `phase = InProgress`, `guid`, `match_info` (`MAP`, `GAME_TYPE`, `FACTORY_TITLE`, лимиты, `started_at = received_at`). Список игроков заменяется на `PLAYERS` (команда нормализуется из числа), счёт = 0. |
| `MATCH_REPORT` | Матч закрывается: `match_info = None`, `guid = None`. Боты удаляются. `phase = Warmup`, если остались люди, иначе `Empty`. |
| `PLAYER_CONNECT` | Игрок добавляется (`team` пока неизвестна и показывается как `spectator`, пока не придёт `SWITCHTEAM`). Если `phase = Empty` и событие с `WARMUP = true`, `phase = Warmup`. |
| `PLAYER_DISCONNECT` | Игрок удаляется. Если людей не осталось, удаляются боты, `phase = Empty`, `match_info = None`. |
| `PLAYER_SWITCHTEAM` | Обновляется `team` из `KILLER.TEAM` (строка: `FREE`/`RED`/`BLUE`/`SPECTATOR`). |
| `PLAYER_DEATH`, `WARMUP = false`, `guid` совпадает | Изменение счёта по REQUIREMENTS 3.10; неизвестный участник добавляется. `PLAYER_KILL` игнорируется (дубль). |
| Любое событие с `WARMUP = true` | Если `phase = InProgress` с другим GUID, это значит, что `MATCH_REPORT` потерян: матч закрывается. `phase = Warmup`, участники сохраняются, `score = None`. Неизвестные игроки из `KILLER`/`VICTIM`/`NAME` добавляются. |
| `PLAYER_MEDAL`, `PLAYER_STATS` | Только добавляют неизвестного участника и обновляют ник. Счёт не меняют. |
| Тишина | Ничего не меняет. На пустом сервере событий нет, поэтому `Empty` определяется по списку участников. |
| Монитор: `DISCONNECTED`, `CONNECT_RETRIED`, ошибка reconnect | `link = Disconnected { since }`, если ещё не был. |
| Монитор: `CONNECTED`/`HANDSHAKE_SUCCEEDED` | `link = Connected`. Фаза и участники сохраняются как были: пропущенные события не восстановить, ошибки исправит следующий `MATCH_STARTED`/`MATCH_REPORT` или правило устаревания. |
| Таймер, раз в 1 с | Если `link = Disconnected` дольше `QLSTATS_LIVE_OFFLINE_SECS`, наружу отдаётся `state = "offline"`. Если `InProgress` длится дольше `TIME_LIMIT + 5 мин`, матч закрывается. |

Внешнее `state` вычисляется так: `offline`, если связь потеряна дольше порога; иначе `empty`, если людей нет; иначе `warmup` или `in_progress` по `phase`.

**Восстановление при старте.**
1. До запуска collector'а выполняется `SELECT … FROM raw_events WHERE id > (SELECT MAX(id) FROM raw_events WHERE type = 'MATCH_REPORT') ORDER BY id`.
2. Найденные события прогоняются через `apply`.
3. Применяется правило устаревания относительно текущего времени.
4. `link` начинается с `Disconnected { since: now }`, но порог «offline» отсчитывается от старта, чтобы не мигать «нет связи» при обычном запуске.

**Ограничения.**
- Командный счёт не показывается.
- Live-счёт приближённый.
- Бот без CONNECT/DISCONNECT может задержаться в списке в разминке (REQUIREMENTS, раздел 6).

## 9. HTTP API

Ответы `/api/*` — готовые байты из снапшота с заголовками `ETag` (номер версии снапшота), `Cache-Control: no-cache` и gzip. Если `If-None-Match` совпадает, сервер отвечает `304` без тела.

**`GET /api/live`** отдаётся из памяти (`ArcSwap`), с тем же `ETag`/`304`. ETag — это `LiveState.version` плюс флаг `offline`.

Поля ответа:
- `state` — одно из `in_progress`, `warmup`, `empty`, `offline`;
- `score` — live-счёт (REQUIREMENTS 3.10); `null` в разминке;
- `kills` — для командных режимов;
- `team` — `free`, `red`, `blue`;
- `score_approximate: true` — всегда, как подсказка интерфейсу;
- `elapsed_sec` вычисляет клиент: `generated_at − started_at`, дальше тикает локально.

Идёт матч (FFA):

```json
{
  "generated_at": "2026-10-18T20:49:12+03:00",
  "state": "in_progress",
  "server_title": "Twitch server W",
  "match": {
    "guid": "3f8e4859-cb22-4d9b-a875-e7b4e8e646f8",
    "map": "campgrounds", "game_type": "FFA", "factory_title": "Free For All",
    "team_mode": false,
    "started_at": "2026-10-18T20:41:30+03:00",
    "limits": { "frag": 50, "time_min": 15 }
  },
  "players": [
    { "steam_id": "76561198070782136", "name": "vancho.dovlatov.ru", "bot": false, "team": "free", "score": 7, "kills": 8 },
    { "steam_id": "76561198045366387", "name": "Drowsy",             "bot": false, "team": "free", "score": 5, "kills": 5 },
    { "steam_id": "0",                 "name": "Ptica",              "bot": true,  "team": "free", "score": 3, "kills": 3 }
  ],
  "spectators": 1,
  "score_approximate": true
}
```

Разминка (карта и режим до `MATCH_STARTED` неизвестны):

```json
{ "generated_at": "2026-10-18T20:39:50+03:00", "state": "warmup", "server_title": "Twitch server W",
  "match": null,
  "players": [{ "steam_id": "76561198045366387", "name": "Drowsy", "bot": false, "team": "free", "score": null, "kills": null }],
  "spectators": 0, "score_approximate": true }
```

Сервер пуст:

```json
{ "generated_at": "2026-10-18T20:50:00+03:00", "state": "empty", "server_title": "Twitch server W",
  "match": null, "players": [], "spectators": 0, "score_approximate": true }
```

Нет связи. Последнее известное состояние не отдаётся, чтобы не вводить в заблуждение:

```json
{ "generated_at": "2026-10-18T20:52:31+03:00", "state": "offline", "server_title": "Twitch server W",
  "offline_since": "2026-10-18T20:52:03+03:00",
  "match": null, "players": [], "spectators": 0, "score_approximate": true }
```

**`GET /api/summary`**

```json
{
  "generated_at": "2026-10-18T21:14:03+03:00",
  "day": {
    "date": "2026-10-18",
    "matches": 7,
    "top_wins":  { "value": 3,  "players": [{ "steam_id": "76561198000000001", "name": "Anarki" }] },
    "top_kills": { "value": 58, "players": [{ "steam_id": "76561198000000001", "name": "Anarki" },
                                            { "steam_id": "76561198000000002", "name": "Doom" }] }
  },
  "all_time": {
    "matches": 412,
    "top_wins":  { "value": 97,   "players": [{ "steam_id": "76561198000000002", "name": "Doom" }] },
    "top_kills": { "value": 5210, "players": [{ "steam_id": "76561198000000002", "name": "Doom" }] }
  }
}
```

Если матчей нет, возвращается `"matches": 0`, `"value": 0`, `"players": []`.

**`GET /api/players`**

```json
{
  "generated_at": "2026-10-18T21:14:03+03:00",
  "players": [
    { "steam_id": "76561198000000002", "name": "Doom", "matches": 230, "kills": 5210,
      "deaths": 4100, "best_streak": 17, "wins": 97, "accuracy": 0.3184,
      "suicides": 41, "env_deaths": 12, "fav_weapon": { "weapon": "RAILGUN", "kills": 1234 } }
  ]
}
```

`accuracy` — доля от 0 до 1 или `null`. `fav_weapon` — любимое оружие (REQUIREMENTS 3.4): `weapon` — канонический ключ (`GAUNTLET`, `MACHINEGUN`, `SHOTGUN`, `GRENADE`, `ROCKET`, `LIGHTNING`, `RAILGUN`, `PLASMA`, `BFG`, `HMG`, `CHAINGUN`, `NAILGUN`, `PROXMINE`), `kills` — засчитанные убийства этим оружием; `null`, если ни одно убийство не приписано оружию. Форматирование в проценты выполняет фронтенд. Порядок строк не гарантирован, сортирует клиент.

**`GET /healthz`** отвечает `200`, пока доступна БД, иначе `503`. Состояние ZMQ на код ответа не влияет: сервер может просто молчать.

```json
{ "status": "ok", "version": "0.1.0", "db": "ok",
  "zmq": { "connected": true, "last_event_at": "2026-10-18T21:13:58+03:00", "reconnects": 2 } }
```

**`GET /`**, `/app.js`, `/style.css` — статика.

## 10. CLI

| Команда | Действие |
|---------|----------|
| `qlstats run` | Основной режим: collector, writer и web. |
| `qlstats recompute` | Одна транзакция: удаляет `deaths`, `match_player_weapons`, `match_players`, `matches`, `players`, затем проходит `raw_events ORDER BY id` (`players` upsert, `finalize_match` на каждый `MATCH_REPORT`). Запускается при остановленном сервисе. Можно и при работающем: WAL плюс `BEGIN IMMEDIATE`, writer подождёт. Перед запуском выводит предупреждение и делает бэкап (`VACUUM INTO`). |
| `qlstats record --out file.jsonl` | Подключается к ZMQ и пишет строки в том же формате, что уже записанные `docs/research/sample-*.jsonl`: `{"received_at": "<ISO-8601 UTC>", "type": "...", "data": {...}}`. Если фрейм не разобрался, вместо `data` пишется `"raw": "<исходный текст>"`. БД не трогает. Нужен для отладки и создания фикстур. |
| `qlstats replay file.jsonl [--db path] [--dump]` | Прогоняет записанные события через writer-пайплайн и `LiveState` с их `received_at`. По умолчанию использует временную БД. С `--dump` печатает JSON `/api/summary`, `/api/players` и `/api/live` (часы — `received_at` последнего события). Основа интеграционных тестов. |
| `qlstats replay file.jsonl --publish tcp://127.0.0.1:28556 [--speed 1.0]` | Работает как фейковый QL-сервер: ZMQ PUB с PLAIN, события отправляются с исходными интервалами. Нужен для локальной проверки collector'а и live-блока, а также для стресс-теста. |

Общие флаги: `--env-file`, `--db`.

## 11. Тестирование

- **Unit-тесты (`stats`, `model`)** на JSON-фикстурах в `tests/fixtures/`:
  - десериализация: `0/1` и `true/false`, `STEAM_ID` строкой и числом, лишние и отсутствующие поля, битый JSON;
  - классификация смертей: каждая строка таблицы из REQUIREMENTS 3.4, включая `SWITCHTEAM`, бот-killer, бот-victim, самоподрыв `ROCKET_SPLASH`, `/kill`, `LAVA`, `UNKNOWN` без `KILLER`;
  - стрик: обнуление смертью от окружения, игнор убийств ботов, максимум по матчам;
  - победители: DUEL по `WIN`, FFA с ничьей лидеров, исключение вышедших;
  - точность: исключение `S = 0`, суммирование по матчам;
  - любимое оружие: соответствие `MOD` → оружие, `TELEFRAG`/`KAMIKAZE` без оружия, тай-брейк по порядку оружия, учёт только засчитанных убийств;
  - критерии `counted`: aborted, нет `MATCH_STARTED`, командный режим, один человек плюс бот, повторный REPORT;
  - `day_key` на границе: `20:59:59.999Z` и `21:00:00.000Z` относятся к разным московским суткам.
- **Unit-тесты `live`** на фикстурах из `docs/research/sample-long.jsonl` (копируются в `tests/fixtures/`):
  - конец реального матча: `PLAYER_STATS` ×2, `MATCH_REPORT`, `PLAYER_DISCONNECT`. Ожидается `in_progress → warmup` (человек ещё на сервере, боты сброшены) → `empty` после ухода последнего человека;
  - разминка: `PLAYER_CONNECT` и `SWITCHTEAM` (`SPECTATOR → FREE`) с `WARMUP = true` дают `warmup` с одним игроком, `team = free`, `score = null`;
  - `MATCH_STARTED` с `PLAYERS` (числовой `TEAM`, бот `STEAM_ID "0"`) даёт `in_progress`, участники и бот помечены;
  - live-счёт: убийство бота человеком +1, `ROCKET_SPLASH` по себе −1, `LAVA` −1, `SWITCHTEAM` 0; сравнение с итоговым `SCORE` из `PLAYER_STATS` той же записи (допуск ±2);
  - потерянный `MATCH_REPORT`: событие warmup с новым GUID закрывает матч; устаревание по `TIME_LIMIT + 5 мин`;
  - `link`: `Disconnected` дольше порога даёт `offline`, после `Connected` возвращается прежняя фаза;
  - восстановление: `restore_from(raw_events)` совпадает с инкрементальным `apply` по тем же событиям.
- **Интеграционный тест replay:**
  - `replay` фикстурного JSONL (синтетика плюс фрагмент живой записи) сравнивается с эталонным JSON (golden-файл);
  - `replay` с последующим `recompute` даёт тот же результат;
  - двойной прогон одного файла не меняет результат (дедупликация).
- **HTTP:** тесты роутера через `tower::ServiceExt::oneshot` проверяют `304` по ETag и формат JSON.

## 12. Стресс-тест

План (методика и цели — в REQUIREMENTS, раздел 5):

1. Сборка `--release` на целевой машине. БД наполняется через `replay` реалистичной записи или синтетикой: около 200 игроков и около 5000 матчей.
2. **Простой**, 10 мин: `pidstat -u -r -p <pid> 1` (на macOS — цикл `ps -o %cpu,rss -p <pid>`).
3. **Сценарий «зритель», k6**: 300 VU. Каждый загружает `/`, `/app.js`, `/style.css`, затем:
   - `/api/live` раз в 5 с (60 RPS суммарно);
   - `/api/summary` и `/api/players` раз в 30 с (20 RPS);
   - все запросы с `If-None-Match`.

   Этот сценарий описывает первоначальную схему с опросом. После отказа от автообновления (2026-10-18) реальная нагрузка ниже: один набор запросов на загрузку страницы, поэтому сценарий остаётся консервативной верхней оценкой.

   Длительность 10 мин. Сервис подключён к `qlstats replay sample-long.jsonl --publish …` (в цикле), чтобы live-состояние и снапшот статистики менялись под нагрузкой.
4. **Насыщение, oha**: `oha -c 300 -z 60s` отдельно по `/api/live`, `/api/players` (без ETag и с ETag), `/api/summary` и `/`.
5. Фиксируются p50/p95/p99, RPS, ошибки, CPU% и RSS (средние и пиковые), fd. Результаты записываются в отдельную заметку `docs/perf/` вместе с конфигурацией железа и коммитом.

Ожидаемая картина: все `/api/*`, включая `/api/live`, отдают готовые байты из `ArcSwap` без обращения к БД. Узким местом могут оказаться только gzip и сеть, поэтому сжатые ответы тоже кэшируются в снапшоте.

## 13. Эксплуатация

- **Запуск:** systemd-unit с `Type=simple`, `Restart=always`, `EnvironmentFile=/etc/qlstats.env` (права 600), `WorkingDirectory` с каталогом `data/`, `User=qlstats`. Логи идут в journald.
- **Логирование:**
  - INFO — подключение и переподключение ZMQ, засчитанные и отклонённые матчи с `skip_reason`, пересборка снапшота;
  - WARN — `_INVALID`-события, смерти категории `ignored` по правилу 7, тишина в ZMQ дольше N минут;
  - ERROR — ошибки транзакций.
  - Пароль не логируется.
- **Переподключение ZMQ:**
  - `RECONNECT_IVL = 1 s`, `RECONNECT_IVL_MAX = 30 s`, `HEARTBEAT_IVL`/`HEARTBEAT_TIMEOUT` (libzmq ≥ 4.2) для обнаружения мёртвого TCP;
  - socket monitor логирует `CONNECTED`, `DISCONNECTED` и `HANDSHAKE_FAILED_AUTH`;
  - при ошибке `recv` сокет и контекст пересоздаются с экспоненциальной паузой (как в minqlx).
- **healthz** используется внешним мониторингом. По `zmq.last_event_at` видно, что сервер давно молчит.
- **Бэкап SQLite:** раз в сутки `sqlite3 qlstats.db ".backup ..."` или `VACUUM INTO` (systemd-timer или cron), хранятся последние N копий. `raw_events` — главная ценность: всё остальное восстанавливается через `recompute`. Копировать файл БД простым `cp` при работающем процессе нельзя (WAL).
- **Обновление версии:** при старте применяются миграции по `user_version`. Если изменились правила учёта, после обновления запускается `qlstats recompute`.

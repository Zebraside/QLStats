# Формат ZMQ-статистики Quake Live (результат исследования)

Источники: [ql-stats-model](https://github.com/quakelive-server-standards/ql-stats-model) (TS-модель, README с реальными JSON), [minqlx `_zmq.py`](https://github.com/MinoMino/minqlx/blob/master/python/minqlx/_zmq.py), [quakestats](https://pypi.org/project/quakestats/), [ql-console](https://github.com/quakelive-server-standards/ql-console). Публичной спецификации нет; модель датирована 2021 годом — реальные ключи на сервере надо сверить с записью живого потока.

## 1. Транспорт
- Сервер: ZMQ PUB. Cvar'ы `zmq_stats_enable 1`, `zmq_stats_ip`, `zmq_stats_port`, `zmq_stats_password`.
- Клиент: ZMQ SUB, пустой префикс подписки (получать всё).
- Аутентификация: ZMQ PLAIN, `ZMQ_PLAIN_USERNAME = "stats"`, пароль = cvar, `ZMQ_ZAP_DOMAIN = "stats"` (так в minqlx).
- Сообщение: один фрейм, UTF-8 JSON `{"TYPE": "...", "DATA": {...}}`, без топик-префикса. Декодировать lossy (minqlx игнорирует битые байты).
- Версия libzmq сервера неизвестна; для Rust — крейт `zmq` (rust-zmq) над libzmq 4.x.
- PUB не буферизует: события при обрыве теряются навсегда. minqlx при ошибке пересоздаёт контекст и сокет. Использовать `ZMQ_RECONNECT_IVL`, опрос с таймаутом, рассчитывать на неполные матчи.

## 2. События
Во всех событиях, кроме MATCH_REPORT/MATCH_STARTED, есть `MATCH_GUID`. `TIME` — секунды от начала матча (по живой записи; в модели ошибочно указаны мс). У большинства игровых событий верхнеуровневый `WARMUP`.

- **MATCH_STARTED:** `MATCH_GUID`, `GAME_TYPE`, `MAP`, `FACTORY`, `FACTORY_TITLE`, `SERVER_TITLE`, лимиты (`FRAG_LIMIT`, `SCORE_LIMIT`, `TIME_LIMIT`, `ROUND_LIMIT`, `CAPTURE_LIMIT`, `MERCY_LIMIT`), флаги `INSTAGIB`, `INFECTED`, `QUADHOG`, `TRAINING` (0/1), `PLAYERS[]` (`NAME`, `STEAM_ID`, `TEAM`).
- **MATCH_REPORT:** `ABORTED` (bool), `MATCH_GUID`, `GAME_TYPE`, `MAP`, `FACTORY`, `GAME_LENGTH` (сек), `EXIT_MSG` ("Timelimit hit.", "Players have forfeited.", "Shutdown"), `TSCORE0` (red), `TSCORE1` (blue), `RESTARTED`, `FIRST_SCORER`, `LAST_SCORER`, `LAST_TEAMSCORER` ("none" = пусто), `LAST_LEAD_CHANGE_TIME`, лимиты и флаги как в MATCH_STARTED.
- **PLAYER_STATS** (по одному на игрока, перед MATCH_REPORT):
  - `MATCH_GUID`, `NAME`, `STEAM_ID`, `MODEL`, `ABORTED`, `WARMUP`, `QUIT` (0/1), `RANK` (-1 у вышедшего), `TIED_RANK`, `TEAM_RANK`, `SCORE`, `KILLS`, `DEATHS`, `WIN` (0/1), `LOSE` (0/1), `TEAM` (0 free, 1 red, 2 blue).
  - `MAX_STREAK` (верхний уровень DATA), `PLAY_TIME`, `TEAM_JOIN_TIME`, `HOLY_SHITS`.
  - `DAMAGE{DEALT,TAKEN}`, `MEDALS{...}`, `PICKUPS{...}`, `ITEM_TIMING{...}`, флаги CTF (могут отсутствовать).
  - `WEAPONS{<ИМЯ>: {S, H, K, D, DG, DR, P, T}}`: S shots, H hits, K kills, D deaths, DG damage given, DR damage received; P, T не документированы. Точность = H/S. Имена: BFG, CHAINGUN, GAUNTLET, GRENADE, HMG, LIGHTNING, MACHINEGUN, NAILGUN, PLASMA, PROXMINE, RAILGUN, ROCKET, SHOTGUN, OTHER_WEAPON.
- **PLAYER_KILL / PLAYER_DEATH:** приходят парой с одинаковыми данными. `MATCH_GUID`, `TIME`, `MOD`, `SUICIDE`, `TEAMKILL`, `WARMUP`, `ROUND`, `TEAM_ALIVE`, `TEAM_DEAD`, `OTHER_TEAM_ALIVE`, `OTHER_TEAM_DEAD`, блоки `KILLER` и `VICTIM`.
  - Блок: `NAME`, `STEAM_ID`, `TEAM`, `BOT`, `BOT_SKILL`, `HEALTH`, `ARMOR`, `AMMO`, `WEAPON`, `HOLDABLE`, `POWERUPS`, `POSITION{X,Y,Z}`, `VIEW{X,Y,Z}`, `SPEED`, `AIRBORNE`, `SUBMERGED`. У `VICTIM` ещё `STREAK`.
  - У PLAYER_DEATH `KILLER` отсутствует, если убийца не игрок.
  - ROUND/TEAM_ALIVE и т.п. null вне раундовых режимов.
- **PLAYER_CONNECT / PLAYER_DISCONNECT:** `MATCH_GUID`, `NAME`, `STEAM_ID`, `TIME`, `WARMUP`.
- **PLAYER_SWITCHTEAM:** игрок в ключе `KILLER` (`NAME`, `STEAM_ID`, `TEAM`, `OLD_TEAM` — строки), плюс `TIME`, `WARMUP`.
- **PLAYER_MEDAL:** `MEDAL`, `NAME`, `STEAM_ID`, `TIME`, `TOTAL`, `WARMUP`.
- **ROUND_OVER:** `MATCH_GUID`, `ROUND`, `TEAM_WON`, `TIME`, `WARMUP`.

### MOD (причина смерти)
BFG, BFG_SPLASH, CHAINGUN, CRUSH, FALLING, GAUNTLET, GRENADE, GRENADE_SPLASH, HMG, HURT, KAMIKAZE, LAVA, LIGHTNING, MACHINEGUN, NAILGUN, PLASMA, PLASMA_SPLASH, PROXMINE, PROXMINE_SPLASH, RAILGUN, ROCKET, ROCKET_SPLASH, SHOTGUN, SLIME, SWITCHTEAM, TRIGGER_HURT, WATER, UNKNOWN, плюс SUICIDE (команда `/kill`). Средой модель считает CRUSH, FALLING, HURT, LAVA, SLIME, TRIGGER_HURT, WATER, UNKNOWN. TELEFRAG в модели не найден.

`SUICIDE: true` ставится и для `/kill`, и для самоподрыва своим оружием, и для SWITCHTEAM. «Смерть не от игрока» определять по отсутствию `KILLER` и/или `MOD`, не по одному флагу.

### Бот или человек
Поле `BOT` (bool) и `BOT_SKILL` есть в блоках KILLER/VICTIM. В PLAYER_STATS/PLAYER_CONNECT его нет — там minqlx считает ботом `STEAM_ID == 0`. Формат значения у бота ("0" строкой или число) не проверен.

## 3. GAME_TYPE
AD, CA, CTF, DUEL, DOM, FFA, FT, HAR, ONEFLAG, RACE, RR, TDM.
Командные: CA, TDM, CTF, ONEFLAG, FT, AD, DOM, RR, HAR. Некомандные: FFA, DUEL, RACE. Написания ONEFLAG/HAR проверить на живом сервере.

## 4. Подводные камни
- Порядок в конце матча: N × PLAYER_STATS, затем MATCH_REPORT. Завершать матч по MATCH_REPORT, PLAYER_STATS буферизовать по MATCH_GUID.
- Warmup: много событий с `WARMUP: true`, включая PLAYER_STATS — фильтровать.
- `ABORTED` бывает false при форфейте ("Players have forfeited."). Решение по ABORTED + EXIT_MSG + WARMUP.
- minqlx принимает MATCH_REPORT только после MATCH_STARTED (защита от смены карты/рестартов).
- KILL и DEATH — дубли одной смерти; считать одно (DEATH, т.к. у него KILLER опционален).
- MATCH_GUID может меняться в warmup при смене карты — привязывать к GUID, не к карте.
- Булевы флаги приходят то `true/false`, то `0/1` — десериализовать терпимо.
- Готового Rust-парсера нет.

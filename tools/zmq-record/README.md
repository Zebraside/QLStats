# zmq-record

Одноразовый инструмент: записывает живой ZMQ-поток статистики Quake Live в JSONL, чтобы сверить реальную схему событий с `docs/research/ql-zmq-events.md`.

## Запуск

```sh
. ~/.cargo/env
cargo build --release
./target/release/zmq-record --out ../../docs/research/sample-60s.jsonl --duration-secs 60
```

Параметры: `--out <file.jsonl>` (обязателен), `--duration-secs N` (без него работает до Ctrl+C), `--env <путь/.env>` (по умолчанию `../../.env`).

Нужны переменные `QL_ZMQ_HOST`, `QL_ZMQ_PORT`, `QL_ZMQ_USER`, `QL_ZMQ_PASSWORD` (в `.env` или окружении). Пароль не логируется.

## Формат

Строка JSONL: `{"received_at":"<RFC3339 UTC>","type":"<TYPE>","data":<DATA>}`. Если сообщение не JSON: `{"received_at":..., "raw":"<lossy string>"}`.

В stderr: события монитора сокета (CONNECTED, HANDSHAKE_SUCCEEDED, HANDSHAKE_FAILED_AUTH и т.д.), каждое событие (TYPE, MATCH_GUID, WARMUP) и статистика по типам в конце.

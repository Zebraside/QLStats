# Стресс-тест QLStats

Методика: `docs/ARCHITECTURE.md`, раздел 12, и `docs/REQUIREMENTS.md`, раздел 5. Результаты — в `docs/perf/`.

- `gen.py` — генератор синтетического JSONL (по умолчанию 200 игроков, 5000 FFA-матчей).
- `viewer.js` — k6-сценарий «зритель».

```sh
python3 -I tools/stress/gen.py --out /tmp/stress.jsonl
./target/release/qlstats replay tests/fixtures/synthetic.jsonl --db data/stress.db
./target/release/qlstats replay /tmp/stress.jsonl --db data/stress.db

# фейковый QL-сервер и отдельный экземпляр сервиса
./target/release/qlstats replay docs/research/sample-long.jsonl --publish tcp://127.0.0.1:28600 --loop --speed 5 &
QL_ZMQ_HOST=127.0.0.1 QL_ZMQ_PORT=28600 QLSTATS_DB_PATH=./data/stress.db QLSTATS_HTTP_ADDR=127.0.0.1:8082 ./target/release/qlstats run &

# нагрузка
k6 run -e BASE=http://127.0.0.1:8082 -e VUS=300 -e DURATION=5m tools/stress/viewer.js
oha -z 60s -c 300 -j --no-tui http://127.0.0.1:8082/api/live
```

CPU и RSS снимаются циклом `ps -o %cpu,rss -p <pid>` раз в секунду. Генератор нагрузки и сервис на одной машине делят CPU.

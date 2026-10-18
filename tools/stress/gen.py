#!/usr/bin/env python3
"""Генератор синтетического JSONL для стресс-теста QLStats.

Формат строк совпадает с tests/fixtures/synthetic.jsonl: MATCH_STARTED ->
PLAYER_DEATH... -> PLAYER_STATS -> MATCH_REPORT, received_at растёт.

Запуск: python3 -I tools/stress/gen.py --out stress.jsonl [--players 200] [--matches 5000]
"""
import argparse
import json
import random
import uuid
from datetime import datetime, timedelta, timezone

MODS = ["RAILGUN", "ROCKET", "ROCKET_SPLASH", "SHOTGUN", "LIGHTNING", "PLASMA", "MACHINEGUN", "GRENADE"]
MAPS = ["campgrounds", "bloodrun", "aerowalk", "cure", "toxicity", "lostworld"]
WEAPONS = ["GAUNTLET", "MACHINEGUN", "SHOTGUN", "GRENADE", "ROCKET", "PLASMA", "LIGHTNING", "RAILGUN"]


def ts(t):
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--players", type=int, default=200)
    ap.add_argument("--matches", type=int, default=5000)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--end", default="2026-10-18T17:00:00", help="UTC, конец потока")
    a = ap.parse_args()
    rnd = random.Random(a.seed)
    players = [{"NAME": f"Player{i:03d}", "STEAM_ID": str(76561198000000000 + i)} for i in range(a.players)]
    step = timedelta(minutes=20)
    base = datetime.fromisoformat(a.end).replace(tzinfo=timezone.utc) - step * a.matches
    t = base
    with open(a.out, "w") as f:
        def emit(typ, data):
            nonlocal t
            f.write(json.dumps({"received_at": ts(t), "type": typ, "data": data}, ensure_ascii=False) + "\n")
            t += timedelta(milliseconds=rnd.randint(200, 3000))

        for m in range(a.matches):
            t = base + step * m
            guid = str(uuid.UUID(int=rnd.getrandbits(128), version=4))
            n = rnd.randint(4, 10)
            ps = rnd.sample(players, n)
            mp = rnd.choice(MAPS)
            emit("MATCH_STARTED", {
                "MATCH_GUID": guid, "GAME_TYPE": "FFA", "MAP": mp, "FACTORY": "ffa",
                "FACTORY_TITLE": "Free For All", "SERVER_TITLE": "Stress server", "FRAG_LIMIT": 30,
                "TIME_LIMIT": 10, "CAPTURE_LIMIT": 8, "ROUND_LIMIT": 10, "SCORE_LIMIT": 150,
                "PLAYERS": [{"NAME": p["NAME"], "STEAM_ID": p["STEAM_ID"], "TEAM": 0} for p in ps]})
            score = {p["STEAM_ID"]: 0 for p in ps}
            for k in range(rnd.randint(15, 40)):
                v = rnd.choice(ps)
                kl = rnd.choice(ps)
                vic = {"NAME": v["NAME"], "STEAM_ID": v["STEAM_ID"], "BOT": False, "BOT_SKILL": None, "TEAM": 0}
                d = {"MATCH_GUID": guid, "MOD": rnd.choice(MODS), "TIME": 10 + k * 5, "WARMUP": False,
                     "SUICIDE": kl is v, "TEAMKILL": False, "VICTIM": vic}
                if kl is not v:
                    d["KILLER"] = {"NAME": kl["NAME"], "STEAM_ID": kl["STEAM_ID"], "BOT": False, "BOT_SKILL": None, "TEAM": 0}
                    score[kl["STEAM_ID"]] += 1
                emit("PLAYER_DEATH", d)
            order = sorted(ps, key=lambda p: -score[p["STEAM_ID"]])
            for rank, p in enumerate(order, 1):
                w = {}
                for wn in WEAPONS:
                    s = rnd.randint(0, 80) if rnd.random() < 0.5 else 0
                    w[wn] = {"S": s, "H": rnd.randint(0, s) if s else 0, "K": 0, "D": 0, "DG": 0, "DR": 0, "P": 0, "T": 0}
                emit("PLAYER_STATS", {
                    "MATCH_GUID": guid, "NAME": p["NAME"], "STEAM_ID": p["STEAM_ID"], "WARMUP": 0,
                    "ABORTED": False, "QUIT": 0, "RANK": rank, "SCORE": score[p["STEAM_ID"]],
                    "WIN": 1 if rank == 1 else 0, "LOSE": 0 if rank == 1 else 1, "TEAM": 0,
                    "MAX_STREAK": rnd.randint(1, 8), "KILLS": 99, "DEATHS": 99, "WEAPONS": w})
            emit("MATCH_REPORT", {
                "MATCH_GUID": guid, "GAME_TYPE": "FFA", "MAP": mp, "ABORTED": False,
                "EXIT_MSG": "Fraglimit hit.", "GAME_LENGTH": 600, "SERVER_TITLE": "Stress server",
                "FRAG_LIMIT": 30, "TIME_LIMIT": 10, "TSCORE0": 0, "TSCORE1": 0})
    print(f"written {a.out}")


if __name__ == "__main__":
    main()

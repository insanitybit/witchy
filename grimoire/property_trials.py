"""Deterministic public U64 trials over the ordinary Grimoire test session."""

import hashlib
import json
import re
from pathlib import Path

DRIVER = "witchy-prop/1"


def word(seed, case, trial):
    h = hashlib.sha256()
    for value in (DRIVER, str(seed), case, str(trial)):
        b = value.encode()
        h.update(len(b).to_bytes(4, "big"))
        h.update(b)
    return int.from_bytes(h.digest()[:8], "little")


def read_token(path):
    with path.open("rb") as stream:
        raw = stream.read(8193)
    if len(raw) > 8192:
        raise ValueError("oversized replay")
    token = json.loads(raw)
    fields = {
        "driver",
        "artifact",
        "case",
        "seed",
        "trial",
        "original",
        "best",
        "site",
        "reason",
    }
    if (
        not isinstance(token, dict)
        or token.keys() != fields
        or any(not isinstance(token[k], str) for k in fields - {"trial"})
    ):
        raise ValueError("invalid replay record")
    if type(token["trial"]) is not int or not 0 <= token["trial"] < 1024:
        raise ValueError("invalid replay trial")
    for k in ("seed", "best", "original"):
        if not re.fullmatch("0|[1-9][0-9]{0,19}", token[k]) or int(token[k]) >= 1 << 64:
            raise ValueError("invalid replay U64")
    return token


def run(args, artifact, case, execute):
    if [s["type"] for s in case["slots"]] != ["U64"]:
        raise ValueError("property driver needs exactly one public U64")
    if (
        args.read_root
        or args.write_root
        or args.read_dir
        or args.write_dir
        or args.word
        or args.bytes
        or args.fixture
        or args.coverage
    ):
        raise ValueError("property replay uses the empty external grant profile")
    name = case["id"]

    def observe(n):
        return execute({"id": "trial", "case": name, "args": [str(n)]})

    if args.replay:
        token = args.replay_token
        if (
            token["driver"] != DRIVER
            or token["artifact"] != artifact
            or token["case"] != name
        ):
            raise ValueError("replay identity changed")
        seed = int(token["seed"])
        if str(word(seed, name, token["trial"])) != token["original"]:
            raise ValueError("replay generator disagrees with the captured input")
        if (
            not 0 <= seed < 1 << 64
            or not isinstance(token["trial"], int)
            or not 0 <= token["trial"] < 1024
        ):
            raise ValueError("invalid replay seed or trial")
        best = int(token["best"])
        if not 0 <= best <= int(token["original"]) or str(best) != token["best"]:
            raise ValueError("invalid minimized value")
        original = observe(int(token["original"]))
        row = observe(best)
        for got in (original, row):
            if (
                got["status"] != "assertion"
                or got["reason"] != token["reason"]
                or got["site"]["key"] != token["site"]
            ):
                return {"status": "replay_mismatch", "observed": got}, 2
        return {"original": original, "replayed": row}, 1
    if not 0 <= args.seed < 1 << 64 or not 1 <= args.trials <= 1024:
        raise ValueError("invalid campaign seed or trial limit")
    for trial in range(args.trials):
        original = word(args.seed, name, trial)
        first = observe(original)
        if first["status"] == "pass":
            continue
        if first["status"] != "assertion":
            return {"trial": trial, "outcome": first}, 2
        best = original
        # Erase high bits first. Every candidate executes, and a smaller value wins
        # only when the original lexical assertion and reason still fail.
        attempts = 0
        for bit in reversed(range(64)):
            candidate = best & ~(1 << bit)
            if candidate == best:
                continue
            row = observe(candidate)
            attempts += 1
            if row["status"] not in ("assertion", "pass"):
                return {"trial": trial, "outcome": row, "best": str(best)}, 2
            if (
                row["status"] == "assertion"
                and row["site"]["key"] == first["site"]["key"]
                and row["reason"] == first["reason"]
            ):
                best = candidate
        token = {
            "driver": DRIVER,
            "artifact": artifact,
            "case": name,
            "seed": str(args.seed),
            "trial": trial,
            "original": str(original),
            "best": str(best),
            "site": first["site"]["key"],
            "reason": first["reason"],
        }
        if args.save_replay:
            Path(args.save_replay).write_text(json.dumps(token, indent=2) + "\n")
        return {"counterexample": token, "shrink_attempts": attempts}, 1
    return {"passed_trials": args.trials, "seed": str(args.seed)}, 0

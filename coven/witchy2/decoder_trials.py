#!/usr/bin/env python3
"""Independent byte-oriented runner for the registry's wire decoder."""

import argparse
import json
from pathlib import Path
import random
import subprocess
import time

CASE = "test.wy::length"
CODEC = "coven-length/be32-v1"


def expected(data):
    return int.from_bytes(data[:4], "big") if len(data) >= 4 else 4294967295


class Session:
    def __init__(self, command):
        self.proc = subprocess.Popen(
            command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
        )
        self.header = self.receive()
        self.send({"op": "list"})
        catalog = self.receive()["cases"]
        self.case = next(c for c in catalog if c["id"] == CASE)
        if self.case["slots"] != [
            {"name": "input", "type": "&Console<Read>"},
            {"name": "expected", "type": "U64"},
        ]:
            raise ValueError("decoder schema changed")

    def send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def receive(self):
        line = self.proc.stdout.readline(2_000_001)
        if not line or len(line) > 2_000_000:
            raise ValueError("invalid test session frame")
        return json.loads(line)

    def run(self, i, data):
        request = {
            "id": str(i),
            "case": CASE,
            "args": [str(expected(data))],
            "input_hex": data.hex(),
        }
        self.send({"op": "run", "entries": [request]})
        row = self.receive()
        if row.get("case") != CASE or row.get("id") != str(i):
            raise ValueError(f"unmatched outcome: {row}")
        summary = self.receive()
        if row["status"] != "pass" or summary.get("done") != {"pass": 1}:
            raise ValueError(f"decoder failed on {data.hex()}: {row}")
        return row

    def close(self):
        self.proc.stdin.close()
        if self.proc.wait(timeout=2):
            raise ValueError("test session failed")


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--grimoire", default="grimoire")
    cli.add_argument("--project", type=Path, default=Path(__file__).resolve().parent)
    cli.add_argument("--artifact")
    cli.add_argument("--guided", action="store_true")
    cli.add_argument(
        "--iterations", type=int, default=24, choices=range(1, 257), metavar="1..256"
    )
    cli.add_argument("--seed", type=int, default=42)
    cli.add_argument("--save", type=Path)
    cli.add_argument("--replay", type=Path)
    args = cli.parse_args()
    command = [args.grimoire, "test", args.project, "--session", "--case", CASE]
    if args.guided:
        command.append("--coverage")
    if args.artifact:
        command.extend(("--artifact", args.artifact))
    session = Session(command)
    corpus, seen, discoveries = [b""], set(), []
    try:
        if args.guided and "guest-edges/1" not in session.header["features"]:
            raise ValueError("coverage negotiation failed")
        if args.replay:
            with args.replay.open("rb") as stream:
                raw = stream.read(131073)
            if len(raw) > 131072:
                raise ValueError("oversized saved corpus")
            saved = json.loads(raw)
            if (
                saved["codec"] != CODEC
                or saved["case"] != CASE
                or not 1 <= len(saved["inputs"]) <= 256
            ):
                raise ValueError("incompatible regression corpus")
            for i, item in enumerate(saved["inputs"]):
                data = bytes.fromhex(item)
                if len(data) > 128:
                    raise ValueError("oversized regression")
                session.run(i, data)
            print(
                json.dumps(
                    {
                        "artifact": session.header["artifact"],
                        "replayed": len(saved["inputs"]),
                    }
                )
            )
            return
        rng = random.Random(args.seed)
        start = time.monotonic()
        for i in range(args.iterations):
            if time.monotonic() - start > 30:
                raise ValueError("decoder campaign deadline")
            parent = rng.choice(corpus)
            data = (parent + rng.randbytes(rng.randint(1, 8)))[:128] if i else b""
            if i and rng.randrange(3) == 0:
                data = data[: rng.randrange(len(data) + 1)]
            row = session.run(i, data)
            hits = set(row.get("features", []))
            if i == 0 or (hits - seen if args.guided else data not in corpus):
                if data not in corpus:
                    corpus.append(data)
                discoveries.append(
                    {
                        "bytes": data.hex(),
                        "features": sorted(hits),
                        "new": sorted(hits - seen),
                    }
                )
                seen.update(hits)
        if args.guided:
            for i, item in enumerate(discoveries):
                again = session.run(f"replay-{i}", bytes.fromhex(item["bytes"]))
                if again["features"] != item["features"]:
                    raise ValueError("guest edge set changed on replay")
        if args.save:
            args.save.write_text(
                json.dumps(
                    {"case": CASE, "codec": CODEC, "inputs": [v.hex() for v in corpus]},
                    indent=2,
                )
                + "\n"
            )
        print(
            json.dumps(
                {
                    "artifact": session.header["artifact"],
                    "trials": args.iterations,
                    "discoveries": discoveries,
                    "map": session.header.get("edge_maps", []),
                }
            )
        )
    finally:
        session.close()


if __name__ == "__main__":
    main()

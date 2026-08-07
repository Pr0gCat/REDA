#!/usr/bin/env python
"""Run every probe in probes.py against a live Minecraft server over RCON.

Usage:
    python run.py --properties ../minecraft-server/server/server.properties --out results/1.20.1.json --label 1.20.1
    python run.py --properties ../minecraft-server-26.2/server/server.properties --out results/26.2.json --label 26.2

Reads host/port/password from the target server's server.properties (the
same file the server itself reads, per docs/minecraft-server.md), forceloads
one region for the whole run, executes every probe in probes.py in its own
slot, writes a JSON report, and prints a summary table.

The server must already be running (see minecraft-server/run-server.sh or
minecraft-server-26.2/run-server.sh) with RCON enabled before this is run.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

from harness import Slot, clear_region, forceload_region, forceload_release, settle
from probes import PROBES
from rcon import RconClient


def read_properties(path: Path) -> dict:
    props = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        props[key.strip()] = value.strip()
    return props


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--properties", required=True, help="path to the target server's server.properties")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--out", required=True, help="path to write the JSON report")
    ap.add_argument("--label", required=True, help="a short label for this run, e.g. '1.20.1' or '26.2'")
    ap.add_argument("--only", default=None, help="comma-separated probe names to run (default: all)")
    args = ap.parse_args()

    props = read_properties(Path(args.properties))
    port = int(props.get("rcon.port", "25575"))
    password = props.get("rcon.password")
    if not password:
        print("error: rcon.password not found in server.properties", file=sys.stderr)
        return 1

    probes = PROBES
    if args.only:
        wanted = set(args.only.split(","))
        probes = [p for p in probes if p.name in wanted]

    client = RconClient(args.host, port, password)
    print(f"Connecting to {args.host}:{port} ...")
    client.connect()
    print("Connected. Forceloading the probe region...")
    forceload_region(client, len(probes))
    clear_region(client, len(probes))
    # A freshly-generated chunk region needs a few real seconds before it
    # reliably ticks at all -- confirmed directly: a basic, always-reliable
    # 1-hop test (redstone block touching dust) failed immediately after
    # forceloading a brand new area and passed a few seconds later with
    # nothing else changed. See harness.py's module docstring.
    settle(4.0)

    report = {"label": args.label, "probes": []}

    try:
        for i, p in enumerate(probes):
            slot = Slot(index=i, client=client)
            print(f"[{i+1}/{len(probes)}] {p.name} ...", end=" ", flush=True)
            slot.clear()
            settle(0.5)
            try:
                result = p.run(slot)
                if result.all_measurements_null:
                    status = "NEVER TRANSITIONED (all measurements timed out)"
                elif result.all_match:
                    status = "OK"
                else:
                    status = "DISAGREES"
            except Exception as exc:  # noqa: BLE001
                print(f"ERROR: {exc}")
                report["probes"].append(
                    {
                        "name": p.name,
                        "category": p.category,
                        "assumption": p.assumption,
                        "source": p.source,
                        "error": str(exc),
                    }
                )
                continue
            print(status)
            report["probes"].append(
                {
                    "name": p.name,
                    "category": p.category,
                    "assumption": p.assumption,
                    "source": p.source,
                    "checks": [
                        {
                            "question": c.question,
                            "expected": c.expected,
                            "actual": c.actual,
                            "matches": c.matches,
                            "note": c.note,
                        }
                        for c in result.checks
                    ],
                    "measurements": [
                        {"question": m.question, "note": m.note, "value_ticks": m.value_ticks}
                        for m in result.measurements
                    ],
                    "never_transitioned": result.all_measurements_null,
                }
            )
    finally:
        print("Releasing forceload and clearing the region...")
        clear_region(client, len(probes))
        forceload_release(client)
        client.close()

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(f"\nWrote {out_path}")

    total = len(report["probes"])
    disagreements = sum(
        1 for p in report["probes"] if "checks" in p and not all(c["matches"] for c in p["checks"])
    )
    errored = sum(1 for p in report["probes"] if "error" in p)
    print(f"{total} probes run, {disagreements} disagreed with our code's assumption, {errored} errored.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

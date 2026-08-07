#!/usr/bin/env python
"""Diff two run.py JSON reports (e.g. 1.20.1 vs 26.2) into a rule-by-rule table.

Usage:
    python compare.py results/1.20.1.json results/26.2.json
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def load(path: str) -> dict:
    return json.loads(Path(path).read_text(encoding="utf-8"))


def probe_verdict(p: dict) -> str:
    if "error" in p:
        return f"ERROR: {p['error']}"
    parts = []
    for c in p.get("checks", []):
        mark = "PASS" if c["matches"] else "DISAGREE"
        parts.append(f"[{mark}] {c['question']} -> {c['actual']}")
    for m in p.get("measurements", []):
        parts.append(f"[MEASURED] {m['question']} -> {m['value_ticks']} ticks ({m['note']})")
    return "; ".join(parts)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("a", help="first report (e.g. 1.20.1)")
    ap.add_argument("b", help="second report (e.g. 26.2)")
    args = ap.parse_args()

    ra, rb = load(args.a), load(args.b)
    by_name_a = {p["name"]: p for p in ra["probes"]}
    by_name_b = {p["name"]: p for p in rb["probes"]}

    names = list(by_name_a.keys())
    label_a, label_b = ra["label"], rb["label"]

    print(f"{'PROBE':45} {'CATEGORY':16} {label_a:>10} {label_b:>10}  SAME?")
    print("-" * 100)

    disagreements = []
    for name in names:
        pa, pb = by_name_a.get(name), by_name_b.get(name)
        if pa is None or pb is None:
            print(f"{name:45} {'':16} {'missing':>10} {'missing':>10}  ?")
            continue
        va = probe_verdict(pa)
        vb = probe_verdict(pb)
        same = "yes" if va == vb else "DIFFERENT"
        category = pa.get("category", "")
        print(f"{name:45} {category:16} {'see below':>10} {'see below':>10}  {same}")
        print(f"    {label_a}: {va}")
        print(f"    {label_b}: {vb}")
        code_disagrees_a = "checks" in pa and not all(c["matches"] for c in pa["checks"])
        code_disagrees_b = "checks" in pb and not all(c["matches"] for c in pb["checks"])
        if va != vb or code_disagrees_a or code_disagrees_b:
            disagreements.append((name, pa, pb, va != vb, code_disagrees_a, code_disagrees_b))

    print("\n" + "=" * 100)
    print("SUMMARY OF NOTABLE PROBES")
    print("=" * 100)
    if not disagreements:
        print("Every probe agrees between versions and matches our code's assumption.")
    for name, pa, pb, version_diff, ca, cb in disagreements:
        print(f"\n- {name}")
        if version_diff:
            print(f"  VERSION DIFFERENCE between {label_a} and {label_b}.")
        if ca:
            print(f"  {label_a}: game DISAGREES with our code's assumption ({pa['assumption']})")
        if cb:
            print(f"  {label_b}: game DISAGREES with our code's assumption ({pb['assumption']})")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

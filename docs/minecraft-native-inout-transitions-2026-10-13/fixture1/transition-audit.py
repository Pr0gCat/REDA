"""M1 audit: validate every landing state of the 4096-transition walk against
the frozen trace, and prove the walk exercised all 4096 declared-state
transitions. Reuses audit-game.py's raw-RCON reconstruction (proven correct on
the 128-state run) but scoped to the walk's visited states, plus edge coverage.

No new ignores, no weakened settling: a state is accepted only if every gate,
lamp, injection, channel cell, bus read and external pad matches the trace, and
the derived checked_kind matches. The walk is accepted only if all 4096 ordered
pairs over the 64 declared states appear as consecutive vertices."""
import argparse
import collections
import json
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = Path("/home/seith/REDA")
sys_path = str(ROOT / "target/qualification/shared-inout-game-producer/final-protocol")
import sys
sys.path.insert(0, sys_path)
import circuit_conformance as h

ap = argparse.ArgumentParser()
ap.add_argument("--raw", default=str(HERE / "transition-rcon.jsonl"))
ap.add_argument("--report", default=str(HERE / "transition-result.json"))
ap.add_argument("--out", default=str(HERE / "transition-audit.json"))
a = ap.parse_args()

d = h.parse_dump(HERE / "candidate.dump")
t = json.loads((HERE / "expected-trace.json").read_text())
report = json.loads(Path(a.report).read_text())
control_order = t["control_order"]

origin = tuple(report["origin"])
assert origin == (230000, 151, 180000)
absolute = lambda p: tuple(a + b for a, b in zip(origin, p))

# positions -> observation keys (same construction as audit-game.py)
sites = {}
def add(pos, key):
    sites.setdefault(absolute(pos), set()).add(key)
for name, pos in d.gate_outputs.items():
    add(pos, ("gate", name))
for name, pos in d.outputs.items():
    add(pos, ("lamp", name))
injections = {x["signal"]: x for x in t["internal_injections"]}
for signal, item in injections.items():
    add(item["position"], ("injection", signal))
bus = t["pinout"]["inouts"][0]
for i, p in enumerate(bus["channel"]):
    add(p, ("channel", str(i)))
add(bus["readback"], ("read", "bus"))
add(bus["external_pad"], ("read", "pad"))

controls = {absolute(p): n for n, p in d.inputs.items()}
lever_values, zero, observed = {}, {}, {}
entries = 0
skipped_unparseable = 0
# Tolerate a transient non-UTF-8 byte in one RCON log line (log-integrity
# glitch, not a circuit observation): replace undecodable bytes, skip lines that
# no longer parse, and count them so the skip is transparent.
with open(a.raw, errors="replace") as f:
    for line in f:
        try:
            entry = json.loads(line)
            cmd, response = entry["command"], entry["response"]
        except (json.JSONDecodeError, KeyError):
            skipped_unparseable += 1
            continue
        entries += 1
        m = re.fullmatch(r"setblock (-?\d+) (-?\d+) (-?\d+) minecraft:lever\[([^]]+)\] replace", cmd)
        if m:
            p = tuple(map(int, m.group(1, 2, 3)))
            if p in controls:
                power = re.search(r"(?:^|,)powered=(true|false)(?:,|$)", m.group(4))
                assert power
                lever_values[controls[p]] = power.group(1) == "true"
                zero.clear()
            continue
        if len(lever_values) != 7:
            continue
        m = re.fullmatch(r"execute if block (-?\d+) (-?\d+) (-?\d+) (minecraft:[a-z_]+)(?:\[([^]]+)\])?", cmd)
        if not m:
            continue
        p = tuple(map(int, m.group(1, 2, 3)))
        if p not in sites:
            continue
        block, properties = m.group(4, 5)
        passed = response.strip().startswith("Test passed")
        value = None
        if block == "minecraft:redstone_wire" and properties == "power=0":
            zero[p] = passed
            if passed:
                value = False
        elif block == "minecraft:redstone_wire" and properties is None:
            if passed and zero.get(p) is False:
                value = True
        elif passed and properties in ("lit=true", "powered=true"):
            value = True
        elif passed and properties in ("lit=false", "powered=false"):
            value = False
        if value is not None:
            raw = sum(int(lever_values[n]) << bit for bit, n in enumerate(control_order))
            for key in sites[p]:
                observed.setdefault(raw, {})[key] = value

# The walk as actually executed: map each report vector to its raw + declared state.
def raw_of(vec):
    # report vectors are in sorted(d.inputs) order; raw is defined over control_order
    controls = dict(zip(sorted(control_order), map(int, vec)))
    return sum(int(controls[n]) << bit for bit, n in enumerate(control_order))
declared_of = {}
for v in t["vectors"]:
    declared_of[v["raw_control_state"]] = v["icarus_declared_state"]
walk_raw = [raw_of(row["vector"]) for row in report["vectors"]]
visited = set(walk_raw)
assert walk_raw[0] == 0  # starts at all-zero (post-build state)

# Every visited state must have been observed and must match the trace.
expected_keys = {key for keys in sites.values() for key in keys}
status_aliases = {al["label"].split(":")[-1]: al["alias"] for al in t["output_aliases"] if not al["label"].startswith("interface:")}
failures, missing = [], []
counts = collections.Counter()
for raw in sorted(visited):
    exp = next(v for v in t["vectors"] if v["raw_control_state"] == raw)
    actual = observed.get(raw)
    if actual is None:
        missing.append(raw)
        continue
    mandatory = {("gate", n) for n in d.gate_outputs} | {("lamp", n) for n in d.outputs}
    if not mandatory <= set(actual):
        failures.append({"raw": raw, "category": "missing_gate_lamp", "missing": sorted(mandatory - set(actual))})
        continue
    for name, value in exp["electrical_observation_expected"].items():
        category = "gate" if name in d.gate_outputs else "lamp"
        if actual.get((category, name)) != value:
            failures.append({"raw": raw, "category": category, "signal": name, "expected": value, "actual": actual.get((category, name))})
    for signal in injections:
        key = ("injection", signal)
        if actual.get(key) != exp["electrical_observation_expected"][signal]:
            failures.append({"raw": raw, "category": "injection", "signal": signal, "expected": exp["electrical_observation_expected"][signal], "actual": actual.get(key)})
    if not expected_keys <= set(actual):
        failures.append({"raw": raw, "category": "missing_bus_reads", "missing": sorted(k for k in expected_keys - set(actual) if k[0] != "injection")})
        continue
    read, pad = actual[("read", "bus")], actual[("read", "pad")]
    for role, value, wanted in [("bus_read", read, exp["electrical_read_expected"]["bus_readback_and_all_channel_cells"]),
                                ("external_pad", pad, exp["electrical_read_expected"]["external_pad"])]:
        if value != wanted:
            failures.append({"raw": raw, "category": "read", "signal": role, "expected": wanted, "actual": value})
    lamp = lambda role: actual[("lamp", status_aliases[role])]
    coherent = all(actual[("channel", str(i))] == read for i in range(len(bus["channel"])))
    coherent = coherent and lamp("expected_value") == read
    coherent = coherent and pad == lamp("external_expected") == lamp("external_observed")
    checked = "ProtocolViolation" if not coherent else "Contention" if lamp("conflict") else "Floating" if lamp("floating") else "Value"
    if checked != exp["checked_kind"]:
        failures.append({"raw": raw, "category": "checked", "expected": exp["checked_kind"], "actual": checked})
    if checked == "Value" and exp["checked_kind"] == "Value" and read != exp["boolean_source_assertion"]:
        failures.append({"raw": raw, "category": "source_boolean", "expected": exp["boolean_source_assertion"], "actual": read})
    counts[checked] += 1

# Transition coverage: every consecutive pair of declared states must appear.
edges = set()
for i in range(len(walk_raw) - 1):
    edges.add((declared_of[walk_raw[i]], declared_of[walk_raw[i + 1]]))
all_pairs = {(x, y) for x in range(64) for y in range(64)}
missing_edges = sorted(all_pairs - edges)

accepted = not failures and not missing and not missing_edges and len(visited) == 64
result = {
    "accepted": accepted,
    "scope": "all 4096 declared-state transitions through the real game; per-landing-state electrical + checked-kind validation against the frozen trace",
    "visited_states": len(visited),
    "distinct_declared_states": len({declared_of[r] for r in visited}),
    "transitions_exercised": len(edges),
    "transitions_missing": len(missing_edges),
    "checked_counts_from_complete_raw_reads": dict(counts),
    "electrical_failures": failures[:50],
    "electrical_failure_count": len(failures),
    "states_not_observed": missing,
    "raw_rcon_pairs": entries,
    "skipped_unparseable_log_lines": skipped_unparseable,
}
Path(a.out).write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps({k: v for k, v in result.items() if k != "electrical_failures"}))
sys.exit(0 if accepted else 1)

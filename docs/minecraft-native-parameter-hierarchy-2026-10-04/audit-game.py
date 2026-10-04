"""Independently check source truth, real RCON readings and region cleanup."""
from pathlib import Path
import hashlib
import json
import re
import sys

root = Path(__file__).resolve().parent
sys.path.insert(0, str(root / "protocol"))
from circuit_conformance import parse_dump, evaluate_expected

origin = (210000, 151, 180000)
dump = parse_dump(root / "candidate.dump")
gates = {gate.name for gate in dump.gates}
public = {label: name for name, label in dump.output_labels.items()}
assert set(dump.inputs) == {f"a[{bit}]" for bit in range(4)}
assert set(public) == {f"y[{bit}]" for bit in range(8)}
assert len(gates) == 285 and gates == set(dump.gate_outputs)

def validate_report(report):
    assert report["origin"] == list(origin)
    assert report["size"] == list(dump.size)
    assert len(report["vectors"]) == 16
    assert len({row["vector"] for row in report["vectors"]}) == 16
    for row in report["vectors"]:
        assert len(row["vector"]) == 4 and set(row["vector"]) <= {"0", "1"}
        mask = sum(int(value) << bit for bit, value in enumerate(row["vector"]))
        expected = evaluate_expected(dump.gates, {f"a[{bit}]": (mask >> bit) & 1 for bit in range(4)}, dump.output_bindings)
        assert row["match"] and row["first_disagreeing_gate"] is None
        assert set(row["actual_gates"]) == gates
        for name, value in row["actual_gates"].items():
            assert value == expected[name], (row["vector"], name)
        assert set(row["actual_outputs"]) == set(public)
        for bit in range(8):
            assert row["actual_outputs"][f"y[{bit}]"] == ((mask ^ 16) >> bit) & 1
    return report

report = validate_report(json.loads((root / "game-report.json").read_text()))
trace = [json.loads(line) for line in (root / "game-rcon.jsonl").read_text().splitlines()]
absolute = lambda pos: tuple(origin[axis] + pos[axis] for axis in range(3))
lever_names = {absolute(pos): name for name, pos in dump.inputs.items()}
sites = {}
for name, pos in dump.gate_outputs.items():
    sites.setdefault(absolute(pos), []).append(("gate", name))
for name, pos in dump.outputs.items():
    sites.setdefault(absolute(pos), []).append(("public", name))
lever_values, dust_zero, observed = {}, {}, {}
for entry in trace:
    command, response = entry["command"], entry["response"]
    match = re.fullmatch(r"setblock (-?\d+) (-?\d+) (-?\d+) minecraft:lever\[([^]]+)\] replace", command)
    if match:
        pos = tuple(map(int, match.group(1, 2, 3)))
        if pos in lever_names:
            powered = re.search(r"(?:^|,)powered=(true|false)(?:,|$)", match.group(4))
            assert powered is not None
            lever_values[lever_names[pos]] = int(powered.group(1) == "true")
            dust_zero.clear()
        continue
    if len(lever_values) != 4:
        continue
    mask = sum(lever_values[f"a[{bit}]"] << bit for bit in range(4))
    match = re.fullmatch(r"execute if block (-?\d+) (-?\d+) (-?\d+) (minecraft:[a-z_]+)(?:\[([^]]+)\])?", command)
    if not match:
        continue
    pos = tuple(map(int, match.group(1, 2, 3)))
    if pos not in sites:
        continue
    block, properties = match.group(4, 5)
    passed = response.strip().startswith("Test passed")
    value = None
    if block == "minecraft:redstone_wire" and properties == "power=0":
        dust_zero[pos] = passed
        if passed:
            value = 0
    elif block == "minecraft:redstone_wire" and properties is None:
        if passed and dust_zero.get(pos) is False:
            value = 1
    elif passed and properties in ("lit=true", "powered=true"):
        value = 1
    elif passed and properties in ("lit=false", "powered=false"):
        value = 0
    if value is not None:
        for key in sites[pos]:
            observed.setdefault(mask, {})[key] = value
for row in report["vectors"]:
    mask = sum(int(value) << bit for bit, value in enumerate(row["vector"]))
    raw = observed[mask]
    for name, value in row["actual_gates"].items():
        assert raw[("gate", name)] == value, (mask, name, "raw gate")
    for label, name in public.items():
        assert raw[("public", name)] == row["actual_outputs"][label], (mask, label, "raw public")

frozen = 0
for line in (root / "FROZEN-SHA256SUMS").read_text().splitlines():
    digest, name = line.split("  ", 1)
    assert hashlib.sha256((root / name).read_bytes()).hexdigest() == digest, name
    frozen += 1
cleanup = json.loads((root / "cleanup-audit.json").read_text())
assert cleanup["accepted"] and cleanup["all_original_occupied_sites_air"]
assert cleanup["all_test_region_chunks_unforced_after_audit"]
positions = {absolute((block.x, block.y, block.z)) for block in dump.blocks}
air = {tuple(map(int, entry["command"].split()[3:6])) for entry in cleanup["responses"]
       if entry["command"].startswith("execute if block ") and entry["response"] == "Test passed"}
assert positions <= air and len(air) == cleanup["air_sites_checked"] == 3327
queries = [entry for entry in cleanup["responses"] if entry["command"].startswith("forceload query ")]
assert len(queries) == 60 and all("not marked" in entry["response"] for entry in queries)
releases = [entry for entry in trace if entry["command"].startswith("forceload remove ")]
assert releases and all(entry["response"].startswith("Unmarked ") for entry in releases)
result = {"accepted": True, "vectors": 16, "public_truth_checks": 128,
          "implemented_gate_truth_checks": 4560, "actual_gates": 285,
          "raw_real_rcon_command_reply_pairs": len(trace),
          "raw_report_gate_and_public_values_reconstructed": True,
          "frozen_hash_checks": frozen, "cleanup_original_positions_verified": len(positions),
          "cleanup_air_sites_checked": len(air), "cleanup_chunk_queries": len(queries),
          "release_acknowledgements": releases,
          "scope": "Real official26.2 stable-state conformance; no intra-tick waveform claim."}
(root / "root-raw-independent-audit.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result))

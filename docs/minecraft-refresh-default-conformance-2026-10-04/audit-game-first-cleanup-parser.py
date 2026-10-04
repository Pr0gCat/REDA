"""Independent source truth and actual gate audit of the real-game report."""
from pathlib import Path
import itertools
import json

root = Path(__file__).resolve().parent
report = json.loads((root / "game-report.json").read_text())
document = json.loads((root / "candidate-actual-implementation.json").read_text())
graph = document["actual_implementation"]
rows = report["vectors"]
assert len(rows) == 64
assert [row["vector"] for row in rows] == ["".join(map(str, bits)) for bits in itertools.product([0, 1], repeat=6)]
public_checks = private_checks = 0
for row in rows:
    values = dict(zip(sorted(graph["inputs"]), map(int, row["vector"])))
    a, b, c, d, e, f = [bool(values[name]) for name in "abcdef"]
    truth = {"u": a or b, "z": False, "o": True, "v": c and d,
             "w": c and d, "x": c, "y": e != f, "q": not (e or f), "r": f}
    assert set(row["actual_outputs"]) == set(truth)
    for name, target in truth.items():
        assert row["actual_outputs"][name] == int(target), (row["vector"], name)
        public_checks += 1
    assert set(row["actual_gates"]) == {gate["output"] for gate in graph["gates"]}
    for gate in graph["gates"]:
        operands = [bool(values[name]) for name in gate["inputs"]]
        high = any(operands)
        assert gate["kind"].startswith(("Nor(", "Or("))
        value = not high if gate["kind"].startswith("Nor(") else high
        assert row["actual_gates"][gate["output"]] == int(value), (row["vector"], gate)
        values[gate["output"]] = int(value)
        private_checks += 1
    assert row["first_disagreeing_gate"] is None and row["match"]
assert public_checks == 576 and private_checks == 960
trace = [json.loads(line) for line in (root / "game-rcon.jsonl").read_text().splitlines()]
cleanup = [row for row in trace if row["command"].startswith("forceload remove ")]
assert cleanup and all("Removed" in row["response"] for row in cleanup)
result = {
    "accepted": True,
    "source_addresses": 64,
    "public_occurrences": 9,
    "public_truth_checks": public_checks,
    "actual_implementation_gates": 15,
    "private_gate_truth_checks": private_checks,
    "server": "official Minecraft Java 26.2; existing server; no restart",
    "world_fingerprint": document["world_fingerprint"],
    "provenance": "exact frozen v2 dump; not fresh current compiler generation",
    "scope": "stable-state real-game conformance; no intra-tick waveform or generic circuit claim",
    "cleanup_release_acknowledgements": cleanup,
}
(root / "independent-game-result.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result))

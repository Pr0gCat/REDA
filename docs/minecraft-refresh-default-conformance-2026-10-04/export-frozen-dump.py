"""Export the retained physical world, not a fresh compiler invocation."""
from pathlib import Path
import gzip
import hashlib
import json
import sys

root = Path(__file__).resolve().parent
sys.path.insert(0, str(root / "protocol"))
from circuit_conformance import parse_dump

world = json.loads(gzip.decompress((root / "candidate-world.json.gz").read_bytes()))
document = json.loads((root / "candidate-actual-implementation.json").read_text())
source = document["source"]
actual = document["actual_implementation"]
owner = json.loads((root / "original-input-owner-manifest.json").read_text())
identity = json.loads((root / "original-owner-identity-result.json").read_text())
sites = json.loads((root / "candidate-sites.json").read_text())
assert source == actual == json.loads((root / "original-owner-source-netlist.json").read_text())
assert identity["accepted"] and identity["interfaces_and_implemented_observations_exact"]
assert identity["original_world_fingerprint"] == "4818f3a3af7c2c2f80c973a59e730e5d82697afb6c519b38483e5a9b01cee37f"
assert world["fingerprint"] == document["world_fingerprint"] == "a4d7b9d9e82cd4f759b0d3f9d21199b183e8ee7bb5bf98c8c4d4b397e04649ba"
cells = {tuple(at): state for at, state in world["cells"]}
anchor = lambda at: (at["x"], at["y"], at["z"])
logical = {row["id"]: row["logical_gate"] for row in owner["instances"]["instances"]}
for row in owner["instances"]["assignments"]:
    if "PrimaryInput" not in row["driver"]:
        continue
    ordinal = row["driver"]["PrimaryInput"]
    sink = row["sink"]["InstanceInput"]
    gate = source["gates"][logical[sink["instance"]]]
    assert gate["inputs"][sink["input_index"]] == source["inputs"][ordinal]
inputs = {}
for boundary in owner["boundaries"]:
    if "PrimaryInput" not in boundary["endpoint"]:
        continue
    ordinal = boundary["endpoint"]["PrimaryInput"]
    for block in boundary["blocks"]:
        assert cells[anchor(block["at"])] == block["state"]
    levers = [block for block in boundary["blocks"] if block["state"]["kind"] == "Lever"]
    assert len(levers) == 1
    inputs[source["inputs"][ordinal]] = anchor(levers[0]["at"])
assert set(inputs) == set(source["inputs"])
outputs = {}
ports = []
for site in sites:
    if "DeclaredOutput" not in site["id"]:
        continue
    ordinal = site["id"]["DeclaredOutput"]
    signal = source["outputs"][ordinal]
    at = anchor(site["at"])
    assert cells[at]["kind"] == "Lamp"
    if signal in outputs:
        assert outputs[signal] == at
    outputs[signal] = at
    label = site["display_label"].removeprefix("lamp:")
    ports.append((ordinal, signal, at, label))
assert sorted(row[0] for row in ports) == list(range(9))
gate_sites = {site["id"]["InstanceOutput"]: site for site in sites if "InstanceOutput" in site["id"]}
assert sorted(gate_sites) == list(range(15))
lines = ["SIZE " + " ".join(map(str, world["size"]))]
for at, state in sorted(cells.items(), key=lambda pair: (pair[0][1], pair[0][2], pair[0][0])):
    assert state["power"] == 0 and state["half"] is None
    facing = state["facing"].lower() if state["facing"] else "-"
    face = state["face"].lower() if state["face"] else "-"
    position = " ".join(map(str, at))
    lines.append(f"BLOCK {position} {state['kind']} {facing} {face} {str(state['lit']).lower()} {state['delay']} {state['name']}")
    for key, value in sorted(state["extra_properties"].items()):
        lines.append(f"BLOCKPROP {position} {key} {value}")
for name, at in sorted(inputs.items()):
    lines.append(f"INPUT {name} {' '.join(map(str, at))}")
for name, at in sorted(outputs.items()):
    lines.append(f"OUTPUT {name} {' '.join(map(str, at))}")
for ordinal, signal, at, label in sorted(ports):
    lines.append(f"OUTPUTPORT {ordinal} {signal} {signal} {' '.join(map(str, at))} {label}")
known = set(inputs)
for ordinal, gate in enumerate(actual["gates"]):
    assert set(gate["inputs"]) <= known
    known.add(gate["output"])
    site = gate_sites[ordinal]
    assert site["display_label"] == "gate:" + gate["output"]
    at = anchor(site["at"])
    kind = "merge" if gate["kind"].startswith("Or(") else "nor"
    assert gate["kind"].startswith(("Or(", "Nor("))
    lines.append(f"GATEOUT {gate['output']} {' '.join(map(str, at))} {cells[at]['kind']}")
    lines.append(f"GATE {gate['output']} {','.join(gate['inputs']) or '-'} {kind}")
dump_file = root / "candidate.dump"
dump_file.write_text("\n".join(lines) + "\n")
dump = parse_dump(dump_file)
assert len(dump.inputs) == 6 and len(dump.outputs) == 9 and len(dump.gates) == 15
assert len(dump.blocks) == len(cells) == 1094
manifest = {
    "artifact": "exact retained frozen v2 world; no new compilation",
    "optimization_commit": "8c4c03a",
    "fresh_current_compiler_provenance": False,
    "world_fingerprint": world["fingerprint"],
    "dump_sha256": hashlib.sha256(dump_file.read_bytes()).hexdigest(),
    "input_bindings": inputs,
    "input_provenance": "typed original primary-input boundaries; source graph identical; every boundary block preserved; every primary-input assignment checked against source gate operands",
    "ports": ports,
    "actual_implementation_gates": 15,
    "occupied_cells": 1094,
    "construction_policy": "existing game harness inert construction order; initial lit placeholders handled by that unchanged harness",
}
(root / "dump-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps({"dump": str(dump_file), "inputs": 6, "public_occurrences": 9, "actual_gates": 15, "world_fingerprint": world["fingerprint"]}))

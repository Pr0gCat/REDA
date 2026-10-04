"""Independent offline graph/source audit. Does not qualify physical game behavior."""
import csv
import hashlib
import importlib.util
import json
from collections import Counter
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE
HARNESS = HERE / 'protocol/circuit_conformance.py'
ORACLE = HERE / 'standard-oracle.csv'
sys.path.insert(0, str(HARNESS.parent))
spec = importlib.util.spec_from_file_location('unchanged_conformance', HARNESS)
h = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = h
spec.loader.exec_module(h)
dump_path, trace_path = HERE / 'candidate.dump', HERE / 'expected-trace.json'
d = h.parse_dump(dump_path)
t = json.loads(trace_path.read_text())
oracle = {}
endpoint_count = 0
with ORACLE.open() as f:
    for row in csv.reader(f):
        if len(row) != 4:
            assert row == ['oracle_tb.sv:33: $finish called at 8192 (1s)'], row
            continue
        before, after, phase = map(int, row[:3])
        assert phase in (0, 1)
        state, value = (before if phase == 0 else after), row[3].lower()
        assert value in ('0', '1', 'x', 'z')
        assert oracle.setdefault(state, value) == value
        endpoint_count += 1
assert endpoint_count == 8192 and set(oracle) == set(range(64))
assert len(d.inputs) == 7 and set(d.inputs) == set(t['control_order'])
actual = set(t['actual_gate_names'])
virtual = {g['output'] for g in t['virtual_reference_definitions']}
assert len(actual) == 37 and set(d.gate_outputs) == actual
assert actual.isdisjoint(virtual)
assert {g.name for g in d.gates} == actual | virtual
assert len(virtual) == 2 + len(t['output_aliases'])
assert not t['virtual_reference_observations'] and not d.output_bindings
blocks = {(b.x, b.y, b.z): b for b in d.blocks}
assert len(blocks) == len(d.blocks)
observations = []
for g in d.gates:
    if g.name in actual:
        pos = d.gate_outputs[g.name]
        assert blocks[pos].kind == g.observation_kind
        observations.append({'type': 'gate', 'signal': g.name, 'position': pos, 'kind': g.observation_kind})
aliases = {a['actual_signal']: a for a in t['output_aliases']}
injections = {a['signal']: a for a in t['internal_injections']}
inventory = {a['signal']: a for a in t['physical_output_inventory']}
assert len(inventory) == len(t['physical_output_inventory'])
assert set(inventory) == set(aliases) | set(injections)
assert set(aliases).isdisjoint(injections) and len(injections) == 2
for signal, item in inventory.items():
    pos = tuple(item['position'])
    assert blocks[pos].kind == item['kind']
    if signal in aliases:
        a = aliases[signal]
        assert d.outputs[a['alias']] == pos and item['kind'] == 'Lamp'
        assert d.output_labels[a['alias']] == a['label']
        observations.append({'type': 'lamp', **a})
    else:
        assert tuple(injections[signal]['position']) == pos and item['kind'] == 'RedstoneWire'
assert set(d.outputs) == {a['alias'] for a in aliases.values()}
bus = t['pinout']['inouts'][0]
for role, positions in [('channel', bus['channel']), ('readback', [bus['readback']]), ('external_pad', [bus['external_pad']])]:
    for pos in positions:
        assert blocks[tuple(pos)].kind == 'RedstoneWire'
        observations.append({'type': role, 'position': pos, 'kind': 'RedstoneWire'})
assert d.inputs[t['caller_fixture']['input']] == tuple(bus['external_entry'])
assert blocks[tuple(bus['external_entry'])].kind == 'Lever'
assert blocks[tuple(t['caller_fixture']['floor'])].kind == 'Solid'
counts = Counter()
assert {r['raw_control_state'] for r in t['vectors']} == set(range(128))
for row in t['vectors']:
    raw = row['raw_control_state']
    bits = [bool(raw & (1 << bit)) for bit in range(7)]
    controls = dict(zip(t['control_order'], bits))
    assert row['controls'] == controls
    values = h.evaluate_expected(d.gates, {k: int(v) for k, v in controls.items()}, d.output_bindings)
    expected = row['electrical_observation_expected']
    assert set(expected) == actual | set(d.outputs)
    assert all(values[k] == int(v) for k, v in expected.items())
    e0, d0, e1, d1, present, declared, caller = bits
    declared_state = raw & 63
    assert row['icarus_declared_state'] == declared_state
    ieee = oracle[declared_state]
    assert row['source_ieee_expected'] == ieee
    matched = caller == (present and declared)
    kind = 'ProtocolViolation' if not matched else {'x': 'Contention', 'z': 'Floating'}.get(ieee, 'Value')
    assert row['checked_kind'] == kind and row['protocol_matched'] == matched
    assert row['boolean_source_assertion'] == (ieee == '1' if kind == 'Value' else None)
    electrical = (e0 and d0) or (e1 and d1) or caller
    assert bool(values[t['typed_reads']['bus_read']]) == electrical
    assert bool(values[t['typed_reads']['external_pad_read']]) == caller
    assert row['electrical_read_expected'] == {'bus_readback_and_all_channel_cells': electrical, 'external_pad': caller}
    for signal, injection in injections.items():
        writer = injection['source_writer_ordinal']
        assert bool(values[signal]) == [(e0 and d0), (e1 and d1)][writer]
    counts[kind] += 1
assert dict(counts) == t['checked_expected_counts'] == {'Value': 38, 'Contention': 18, 'Floating': 8, 'ProtocolViolation': 64}
result = {'status': 'pass', 'scope': 'offline expected graph and frozen HDL oracle only; actual-game qualification pending',
          'raw_states': 128, 'icarus_states': 64, 'icarus_endpoint_rows': endpoint_count,
          'checked_counts': dict(counts), 'actual_gates': 37, 'output_lamps': len(d.outputs),
          'physical_observation_inventory': observations,
          'source_sha256': {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for p in [HARNESS, ORACLE, dump_path, trace_path, Path(__file__)]}}
(HERE / 'offline-audit.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({k: v for k, v in result.items() if k not in ('physical_observation_inventory', 'source_sha256')}))

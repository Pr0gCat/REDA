"""Audit only the fixed ROM64 diagnostic sample; never qualify production."""
import hashlib
import json
import re
import sys
from pathlib import Path
from audit_balanced_matrix_wave import evaluate
from compare_cover_probe import quality, replay_events

root = Path(sys.argv[1])
fixture = root / 'tests/fixtures/rom64_matrix.sv'
entries = re.findall(r"6'd(\d+)\s*:\s*q\s*=\s*16'h([0-9a-fA-F]+)", fixture.read_text())
assert len(entries) == 64
words = {int(address): int(word, 16) for address, word in entries}
assert set(words) == set(range(64))
sampled = [0, 21, 42, 63]
protocol = [(a, b, phase, [a, b][phase]) for a in sampled for b in sampled for phase in range(2)]
rows = []
reports = []
for directory in ['wave-control-bound-output', 'wave-matrix-output']:
    path = root / directory / 'result.jsonl'
    row = json.loads(path.read_text())
    record = row['result']['Ok']
    assert record['diagnostic_only'] is True
    assert record['sampled_addresses'] == sampled
    assert len(row['source_inputs']) == 6 and len(row['source_outputs']) == 16
    assert [(p['from'], p['to'], p['phase'], p['address']) for p in record['phases']] == protocol
    graphs = [({'inputs': row['source_inputs'], 'gates': record['source_gates']}, row['source_outputs']),
              ({'inputs': record['implemented_inputs'], 'gates': record['implemented_gates']}, record['implemented_outputs'])]
    evaluations = []
    for graph, outputs in graphs:
        values = [evaluate(graph, address) for address in range(64)]
        assert len(outputs) == 16
        assert [sum(int(v[name]) << bit for bit, name in enumerate(outputs)) for v in values] == [words[a] for a in range(64)]
        evaluations.append(values)
    gate_names = {g['output'] for g in record['implemented_gates']}
    previous_internal = {name: evaluations[1][0][name] for name in gate_names}
    previous_public = {str(label): bool(words[0] & (1 << (label % 16))) for label in range(32)}
    arrivals = []
    extra_internal = 0
    for p in record['phases']:
        assert isinstance(p['ticks'], int) and 0 <= p['ticks'] <= 2000
        expected = [bool(words[p['address']] & (1 << bit)) for bit in range(16)]
        assert p['expected'] == p['driver'] == p['lamp'] == expected
        wanted_internal = {name: evaluations[1][p['address']][name] for name in gate_names}
        assert p['internal'] == wanted_internal
        observed_internal = replay_events(previous_internal, p['internal_events'], 'signal', p['ticks'])
        assert observed_internal == wanted_internal
        ideal_changes = sum(previous_internal[name] != wanted_internal[name] for name in gate_names)
        assert len(p['internal_events']) >= ideal_changes
        extra_internal += len(p['internal_events']) - ideal_changes
        previous_internal = observed_internal
        observed_public = replay_events(previous_public, p['events'], 'label', p['ticks'])
        assert observed_public == {str(label): expected[label % 16] for label in range(32)}
        previous_public = observed_public
        arrivals.append([max((e['tick'] for e in p['events'] if e['label'] == str(label)), default=0) for label in range(32)])
    reports.append({'record_sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                    'phases': len(protocol), 'implemented_gates': len(gate_names),
                    'all_64_literal_words_match_source_and_implementation': True,
                    'all_public_and_implemented_gate_events_replayed': True,
                    'extra_internal_transitions': extra_internal,
                    'setup': quality(record, 0), 'target': quality(record, 1),
                    'arrivals': arrivals})
    rows.append(row)
assert rows[0]['source_inputs'] == rows[1]['source_inputs']
assert rows[0]['source_outputs'] == rows[1]['source_outputs']
assert rows[0]['result']['Ok']['source_gates'] == rows[1]['result']['Ok']['source_gates']
a, b = reports
regressions = {}
for kind in ['setup', 'target']:
    regressions[kind] = [key for key in a[kind].keys() | b[kind].keys() if b[kind].get(key, 0) > a[kind].get(key, 0)]
settlement_regressions = [i for i, (p, q) in enumerate(zip(rows[0]['result']['Ok']['phases'], rows[1]['result']['Ok']['phases'])) if q['ticks'] > p['ticks']]
arrival_regressions = [(i, label) for i, (p, q) in enumerate(zip(a['arrivals'], b['arrivals'])) for label in range(32) if q[label] > p[label]]
result = {'diagnostic_only': True, 'production_adoption_qualified': False,
          'fixture_sha256': hashlib.sha256(fixture.read_bytes()).hexdigest(),
          'control': a, 'matrix': b, 'component_regressions': regressions,
          'settlement_regressions': settlement_regressions, 'arrival_regressions': arrival_regressions}
print(json.dumps(result, indent=2))

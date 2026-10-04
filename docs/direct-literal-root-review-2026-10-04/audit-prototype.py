"""Independently recompute prototype metrics from frozen raw events."""
import gzip
import hashlib
import json
from pathlib import Path

BASE = Path(__file__).resolve().parent / 'evidence'
manifest = json.loads((BASE / 'artifact-sha256.json').read_text())
for name, expected in manifest.items():
    assert hashlib.sha256((BASE.parent / name).read_bytes()).hexdigest() == expected, name


def independent_truths(correspondence):
    tables = [[] for _ in correspondence['actual_gate_sites']]
    for address in range(16):
        values = {name: bool(address & (1 << bit)) for bit, name in enumerate(correspondence['input_bindings'])}
        pending = list(enumerate(correspondence['actual_gate_sites']))
        while pending:
            ready = [(index, site) for index, site in pending if all(name in values for name in site['inputs'])]
            assert ready, 'cycle or missing driver'
            for index, site in ready:
                operands = [values[name] for name in site['inputs']]
                kind = site['kind']
                if kind == 'Buf':
                    assert len(operands) == 1
                    value = operands[0]
                else:
                    operation, arity = kind[:-1].split('(')
                    assert int(arity) == len(operands)
                    assert operation in ('Or', 'Nor')
                    value = any(operands) if operation == 'Or' else not any(operands)
                values[site['gate']] = value
                tables[index].append(value)
                pending.remove((index, site))
    for table, site in zip(tables, correspondence['actual_gate_sites']):
        assert table == site['truth_in_source_address_order']
    return tables


def replay(name, summary, correspondence):
    phases = json.loads((BASE / summary).read_text())
    previous = 0
    with gzip.open(BASE / name, 'rt') as stream:
        raw = [json.loads(line) for line in stream]
    assert len(raw) == len(phases) == 512
    tables = independent_truths(correspondence)
    for index, (record, reported) in enumerate(zip(raw, phases)):
        assert record['phase'] == index
        assert record['from'] == index // 32
        assert record['to'] == (index // 2) % 16
        assert record['address'] == (record['from'] if index % 2 == 0 else record['to'])
        public = [bool((record['address'] ^ 16) & (1 << bit)) for bit in range(8)]
        assert record['expected'] == public + public + [table[record['address']] for table in tables]
        assert record['sites'][8:16] == [at for _, at in correspondence['public_occurrences']]
        assert record['sites'][16:] == [site['at'] for site in correspondence['actual_gate_sites']]
        events = [[] for _ in record['sites']]
        cells = 0
        for event in record['events']:
            label = event['label']
            if label.startswith('channel'):
                channel = int(label[7:])
                assert event['position'] == record['sites'][channel]
                events[channel].append(event)
            else:
                assert label.startswith('cell')
                cells += 1
        measured = []
        for channel, changes in enumerate(events):
            target = record['expected'][channel]
            assert not changes or changes[-1]['value'] == target
            widths = [b['tick'] - a['tick'] for a, b in zip(changes, changes[1:]) if a['value'] != target]
            measured.append([int(len(changes) > 1), int(len(changes) > 1 and (previous ^ record['address']).bit_count() == 1), changes[-1]['tick'] - record['start'] if changes else 0, sum(widths), max(widths, default=0)])
        assert measured == reported['channels'], index
        assert cells == reported['physical_site_events'], index
        assert record['ticks'] == reported['ticks']
        assert record['queue_work'] == reported['queue_work']
        previous = record['address']
    return phases

old_map = json.loads((BASE / 'baseline-correspondence.json').read_text())
new_map = json.loads((BASE / 'candidate-correspondence.json').read_text())
old = replay('frozen-original-285.jsonl.gz', 'baseline-phases.json', old_map)
new = replay('frozen-original-11.jsonl.gz', 'candidate-phases.json', new_map)
old_sites = old_map['actual_gate_sites']
new_sites = new_map['actual_gate_sites']
matching = [[i for i, old_site in enumerate(old_sites) if old_site['truth_in_source_address_order'] == site['truth_in_source_address_order']] for site in new_sites]
assert all(matching)
for a, b in zip(old, new):
    assert all(b[key] <= a[key] for key in ['ticks', 'queue_work', 'physical_site_events'])
    assert all(y <= x for aa, bb in zip(a['channels'][:16], b['channels'][:16]) for x, y in zip(aa, bb))
    for metric in range(5):
        aggregate = max if metric in (2, 4) else sum
        assert aggregate(channel[metric] for channel in b['channels'][16:]) <= aggregate(channel[metric] for channel in a['channels'][16:])
    for candidate, baseline_indices in enumerate(matching):
        for baseline in baseline_indices:
            assert all(y <= x for x, y in zip(a['channels'][16 + baseline], b['channels'][16 + candidate]))
print(json.dumps({'artifact_hashes_verified': len(manifest), 'raw_phases_per_world': len(old), 'public_channels': 16, 'candidate_actual_sites': len(new_sites), 'baseline_actual_sites': len(old_sites), 'pointwise_regressions': 0, 'scope': 'prototype raw-event metrics, independently evaluated implementation truth and literal source truth; not production adoption or fresh physical/game proof'}, indent=2))

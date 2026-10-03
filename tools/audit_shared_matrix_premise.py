"""Replay an uncertified four-input matrix premise against frozen source traces."""
import argparse
import gzip
import json
from pathlib import Path

from compare_cover_probe import compare, quality, replay_events


def read(path):
    data = Path(path).read_bytes()
    return gzip.decompress(data) if str(path).endswith('.gz') else data


def audit(log, control):
    lines = read(log).decode().splitlines()
    assert len(lines) == 514
    assert lines[0].startswith('MATRIX_SOURCE ')
    assert lines[-1].startswith('MATRIX_RESULT ')
    assert all(line.startswith('MATRIX_PHASE ') for line in lines[1:-1])
    source = json.loads(lines[0].split(' ', 1)[1])
    phases = [json.loads(line.split(' ', 1)[1]) for line in lines[1:-1]]
    result = json.loads(lines[-1].split(' ', 1)[1])
    assert source['certified'] is False and result['certified'] is False
    assert result['phases'] == len(phases) == 512
    # Reuse the retained source/gate/landing audit without altering the control.
    checked_control = compare(control, control)
    assert not checked_control['setup']['regressed'] and not checked_control['target']['regressed']
    reference = control['result']['Ok']
    assert source['inputs'] == control['source_inputs']
    assert source['outputs'] == control['source_outputs']
    protocol = [(a, b, p, [a, b][p]) for a in range(16)
                for b in range(16) for p in range(2)]
    assert [(p['from'], p['to'], p['phase'], p['address']) for p in phases] == protocol
    assert [(p['from'], p['to'], p['phase'], p['address']) for p in reference['phases']] == protocol
    width = len(source['outputs'])
    words = [None] * 16
    for phase in reference['phases']:
        word = sum(int(value) << bit for bit, value in enumerate(phase['expected']))
        address = phase['address']
        assert words[address] is None or words[address] == word
        words[address] = word
    assert source['words'] == words
    rows = source.get('rows', list(range(16)))
    wanted = ([row for row, word in enumerate(words) if word] or [0]) if source.get('prune_empty', False) else list(range(16))
    assert rows == wanted
    definitions = source['definitions']
    assert len({tuple(d['position']) for d in definitions.values()}) == len(definitions)

    def expected(oracle, address):
        kind = oracle['kind']
        if kind == 'bit':
            assert 0 <= oracle['bit'] < 4 and isinstance(oracle['inverted'], bool)
            return bool(address & (1 << oracle['bit'])) ^ oracle['inverted']
        if kind == 'mismatch':
            assert 0 <= oracle['row'] < 16 and 0 < oracle['mask'] < 16
            return bool((address ^ oracle['row']) & oracle['mask'])
        if kind == 'row':
            assert 0 <= oracle['row'] < 16
            return address == oracle['row']
        assert kind == 'prefix' and 0 <= oracle['bit'] < width and 0 <= oracle['last'] < len(rows)
        return address in rows[:oracle['last'] + 1] and bool(words[address] & (1 << oracle['bit']))

    observed = {label: expected(d['oracle'], 0) for label, d in definitions.items()}
    assert source['power_on'] == observed
    public = []
    slower = []
    slower_settlement = []
    hidden_extra_transitions = 0
    for index, (phase, previous) in enumerate(zip(phases, reference['phases'])):
        if phase['ticks'] > previous['ticks']:
            slower_settlement.append({'phase': index, 'before': previous['ticks'], 'after': phase['ticks']})
        ideal = {label: expected(d['oracle'], phase['address']) for label, d in definitions.items()}
        assert phase['internal'] == ideal
        for label in definitions:
            events = [e for e in phase['events'] if e['label'] == label]
            if not label.startswith('boundary'):
                hidden_extra_transitions += max(0, len(events) - int(observed[label] != ideal[label]))
        observed = replay_events(observed, phase['events'], 'label', phase['ticks'])
        assert observed == ideal
        assert phase['row_bad'] == [row != phase['address'] for row in rows]
        assert phase['expected'] == previous['expected'] == phase['driver'] == phase['lamp']
        boundary = [dict(e, label=e['label'].removeprefix('boundary'))
                    for e in phase['events'] if e['label'].startswith('boundary')]
        public.append(dict(phase, events=boundary))
        for label in range(2 * width):
            before = [e['tick'] for e in previous['events'] if e['label'] == str(label)]
            after = [e['tick'] for e in boundary if e['label'] == str(label)]
            if (after[-1] if after else 0) > (before[-1] if before else 0):
                slower.append({'phase': index, 'channel': label,
                               'before': before[-1] if before else 0,
                               'after': after[-1] if after else 0})
    comparisons = {}
    for phase, name in enumerate(['setup', 'target']):
        old = quality(reference, phase)
        new = quality({'phases': public}, phase)
        keys = sorted(old.keys() | new.keys())
        comparisons[name] = {'control': old, 'prototype': new,
                             'regressed': [k for k in keys if new.get(k, 0) > old.get(k, 0)],
                             'improved': [k for k in keys if new.get(k, 0) < old.get(k, 0)]}
    return {'premise_replay_passed': True, 'phases': len(phases),
            'observed_implementation_sites': len(definitions),
            'all_recorded_primitive_and_intermediate_events_replayed': True,
            'hidden_extra_transitions': hidden_extra_transitions,
            'reference_expanded_certificate': checked_control['control_certified'],
            'quality': comparisons, 'public_phase_channel_arrival_regressions': slower,
            'settlement_phase_regressions': slower_settlement,
            'certified': False, 'release_qualified': False}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('log')
    parser.add_argument('control')
    args = parser.parse_args()
    records = [json.loads(line) for line in read(args.control).splitlines()]
    assert len(records) == 1
    print(json.dumps(audit(args.log, records[0]), indent=2))

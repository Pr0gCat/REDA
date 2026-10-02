"""Replay retained independent-clock traces against their source DFF pins."""
import argparse
import gzip
import json
from pathlib import Path


def audit(records):
    assert len(records) == 6 and len({r['case'] for r in records}) == 6
    for record in records:
        assert set(record['inputs']) == {'d_a', 'd_b', 'clk_a', 'clk_b'}
        assert set(record['outputs']) == {'q_a', 'q_b'}
        gates = {g['output']: g for g in record['gates']}
        assert len(gates) == 2
        for suffix in ['a', 'b']:
            gate = gates[record['outputs']['q_' + suffix]]
            assert gate['kind'] == 'dff_p'
            assert gate['inputs'][1] == 'clk_' + suffix
            assert gate['inputs'][0] in {'d_a', 'd_b', *record['outputs'].values()}
        assert record['power_on'] == [False, False]
        assert len(record['phases']) == 240
        protocol = [step for prior in range(4) for data in range(4) for step in [
            (prior, 0), (prior, 1), (prior, 3), (prior ^ 3, 3),
            (prior ^ 3, 2), (prior ^ 3, 0), (data, 0), (data, 2),
            (data, 3), (data ^ 3, 3), (data ^ 3, 1), (data ^ 3, 0),
            (data, 0), (data, 3), (data, 0)]]
        assert [(p['data'], p['clocks']) for p in record['phases']] == protocol
        q = [False, False]
        clock = [False, False]
        observed = {channel + '_q_' + suffix: False
                    for channel in ['driver', 'lamp'] for suffix in ['a', 'b']}
        for phase in record['phases']:
            assert 0 <= phase['data'] < 4 and 0 <= phase['clocks'] < 4
            assert phase['ticks'] >= 16
            before = {record['outputs']['q_' + suffix]: q[bit]
                      for bit, suffix in enumerate(['a', 'b'])}
            before.update({name: bool(phase['data'] & (1 << bit))
                           for bit, name in enumerate(['d_a', 'd_b'])})
            for bit in range(2):
                next_clock = bool(phase['clocks'] & (1 << bit))
                if next_clock and not clock[bit]:
                    gate = gates[record['outputs']['q_' + ['a', 'b'][bit]]]
                    q[bit] = before[gate['inputs'][0]]
                clock[bit] = next_clock
            last = {label: 0 for label in observed}
            for event in phase['events']:
                label = event['label']
                assert label in observed
                assert last[label] <= event['tick'] <= phase['ticks']
                assert isinstance(event['value'], bool)
                assert event['value'] != observed[label]
                observed[label] = event['value']
                last[label] = event['tick']
            assert phase['expected'] == phase['driver'] == phase['lamp'] == q
            for channel in ['driver', 'lamp']:
                assert [observed[channel + '_q_' + suffix] for suffix in ['a', 'b']] == q
    return {'cases': len(records), 'retained_phases_per_case': 240,
            'source_clock_pins_verified': True, 'public_events_replayed': True,
            'independent_state_oracle': True}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace')
    path = Path(parser.parse_args().trace)
    data = gzip.decompress(path.read_bytes()) if path.suffix == '.gz' else path.read_bytes()
    print(json.dumps(audit([json.loads(line) for line in data.splitlines()]), indent=2))

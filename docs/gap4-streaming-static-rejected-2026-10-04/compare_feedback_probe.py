"""Replay feedback traces from literal source state machines and all gate events."""
import argparse
import gzip
import json
from pathlib import Path


def audit(records, cases=None):
    if cases is None:
        cases = {f'{frontend}-feedback_{case}': case for frontend in ['native', 'yosys']
                 for case in ['counter', 'fsm', 'state_enable']}
    assert len(records) == len(cases) and {r['case'] for r in records} == set(cases)
    for record in records:
        case = cases[record['case']]
        assert case in {'counter', 'fsm', 'state_enable', 'counter4_add', 'counter4_subtract'}
        arithmetic = case.startswith('counter4_')
        gates = {g['output']: g for g in record['gates']}
        outputs = record['outputs']
        names = ['q'] if case == 'state_enable' else [f'q[{bit}]' for bit in range(4 if arithmetic else 2)]
        assert set(outputs) == set(names)
        for signal in outputs.values():
            assert gates[signal]['kind'] == 'dff_p'
            assert gates[signal]['inputs'][1] == 'clk'
        pins = ['d', 'en'] if case == 'state_enable' else ['reset', 'en' if case == 'counter' or arithmetic else 'advance']
        controls = [3, 0, 1, 2, 3, 1, 0, 1] if case == 'state_enable' else [1, 2, 2, 2, 2, 0, 3, 2]
        controls = [1 if cycle in {0, 32} else 3 if cycle == 51 else
                    0 if 17 <= cycle < 32 or cycle == 49 else 2
                    for cycle in range(64)] if arithmetic else [controls[cycle % 8] for cycle in range(64)]
        protocol = [(control, clock) for cycle in range(64)
                    for control, clock in [(controls[cycle], False),
                                           (controls[cycle], True),
                                           (controls[cycle] ^ 3, True),
                                           (controls[cycle] ^ 3, False)]]
        assert [(p['control'], p['clock']) for p in record['phases']] == protocol

        def implementation(q, control, clock):
            values = {'clk': clock, pins[0]: bool(control & 1), pins[1]: bool(control & 2)}
            values.update({outputs[port]: bool(q & (1 << bit)) for bit, port in enumerate(names)})
            pending = {name: gate for name, gate in gates.items() if gate['kind'] != 'dff_p'}
            while pending:
                ready = [name for name, gate in pending.items() if all(s in values for s in gate['inputs'])]
                assert ready, 'combinational cycle or unknown operand'
                for name in ready:
                    gate = pending.pop(name)
                    assert gate['kind'] in {'nor', 'merge'}
                    any_input = any(values[s] for s in gate['inputs'])
                    values[name] = not any_input if gate['kind'] == 'nor' else any_input
            channels = {'gate:' + signal: values[signal] for signal in gates}
            channels.update({'lamp:' + port: values[outputs[port]] for port in names})
            hidden = {}
            for output in outputs.values():
                gate = gates[output]
                for role, expected in [('MData', values[output] if clock else values[gate['inputs'][0]]),
                                       ('SData', values[output]), ('MLock', clock),
                                       ('SLock', not clock), ('InvC', not clock)]:
                    label = f'cell:{output}:{role}'
                    channels[label] = expected
                    hidden[label] = (role, expected)
            return values, channels, hidden

        q, clock = 0, False
        covered = {0}
        _, observed, _ = implementation(q, 0, clock)
        assert record['power_on'] == observed
        for phase in record['phases']:
            control, next_clock = phase['control'], phase['clock']
            if next_clock and not clock:
                if case == 'state_enable':
                    if q or control & 2:
                        q = int(bool(control & 1))
                elif control & 1:
                    q = 0
                elif control & 2:
                    q = (q + (-1 if case == 'counter4_subtract' else 1)) % (16 if arithmetic else 4 if case == 'counter' else 3)
            clock = next_clock
            covered.add(q)
            values, expected, hidden = implementation(q, control, clock)
            assert phase['expected'] == q
            assert phase['internal'] == values
            assert phase['inputs'] == {pin: values[pin] for pin in ['clk', *pins]}
            assert set(phase['cells']) == set(hidden)
            for label, (role, value) in hidden.items():
                cell = phase['cells'][label]
                assert cell['lit'] == value
                assert cell['kind'] == ('WallTorch' if role == 'InvC' else 'Repeater')
                if role in {'MData', 'SData'}:
                    assert cell['locked'] == (clock if role == 'MData' else not clock)
            assert phase['ticks'] >= 16
            # A stored bit changes once toward its captured value, or holds.
            # Final truth alone would miss an extra capture followed by recovery.
            state_labels = {f'gate:{output}' for output in outputs.values()}
            state_labels |= {f'cell:{output}:SData' for output in outputs.values()}
            state_labels |= {f'lamp:{port}' for port in names}
            for label in state_labels:
                changes = [event for event in phase['events'] if event['label'] == label]
                assert len(changes) == int(observed[label] != expected[label]), 'extra storage/public pulse'
            last = {label: 0 for label in observed}
            for event in phase['events']:
                label = event['label']
                assert label in observed
                assert last[label] <= event['tick'] <= phase['ticks']
                assert isinstance(event['value'], bool) and event['value'] != observed[label]
                observed[label] = event['value']
                last[label] = event['tick']
            assert observed == expected
        assert covered == (set(range(16)) if arithmetic else set(range(4)) if case == 'counter' else set(range(3)) if case == 'fsm' else {0, 1})
    return {'cases': len(cases), 'retained_phases_per_case': 256,
            'literal_state_oracle': True, 'all_gate_and_hidden_cell_events_replayed': True}


def compare_quality(records, baseline, cases=None):
    """Compare identical implementations after independently replaying both traces.

    This routing control cannot qualify a changed graph. Check each phase and
    channel; a faster final public event must not hide a slower earlier output.
    """
    audit(records, cases)
    audit(baseline, cases)
    old = {record['case']: record for record in baseline}
    regressions = []
    for record in records:
        before = old[record['case']]
        assert record['gates'] == before['gates'], 'quality control requires identical implemented graph'
        assert record['outputs'] == before['outputs'], 'quality control requires identical output bindings'
        assert record['power_on'] == before['power_on']
        for index, (previous, current) in enumerate(zip(before['phases'], record['phases'])):
            assert (previous['control'], previous['clock']) == (current['control'], current['clock'])
            if current['ticks'] > previous['ticks']:
                regressions.append({'case': record['case'], 'phase': index,
                                    'metric': 'settlement_ticks',
                                    'before': previous['ticks'], 'after': current['ticks']})
            for label in before['power_on']:
                earlier = [event for event in previous['events'] if event['label'] == label]
                later = [event for event in current['events'] if event['label'] == label]
                metrics = [('transitions', len(earlier), len(later)),
                           ('last_arrival_ticks', earlier[-1]['tick'] if earlier else 0,
                            later[-1]['tick'] if later else 0)]
                # Internal pulses remain visible even when final truth agrees.
                # Compare wrong-target pulse widths when transition shapes agree.
                # A longer interval at the correct value is not a wider glitch.
                if [event['value'] for event in earlier] == [event['value'] for event in later]:
                    metrics.extend((f'pulse_width_ticks:{pulse}',
                                    earlier[pulse + 1]['tick'] - earlier[pulse]['tick'],
                                    later[pulse + 1]['tick'] - later[pulse]['tick'])
                                   for pulse in range(len(earlier) - 1)
                                   if earlier[pulse]['value'] != earlier[-1]['value'])
                for metric, first, second in metrics:
                    if second > first:
                        regressions.append({'case': record['case'], 'phase': index,
                                            'channel': label, 'metric': metric,
                                            'before': first, 'after': second})
    return {'cases': len(records), 'qualified': not regressions,
            'regressions': regressions}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace')
    parser.add_argument('--baseline', help='Compare same-graph per-phase/channel quality after replaying both complete traces')
    args = parser.parse_args()
    path = Path(args.trace)
    data = gzip.decompress(path.read_bytes()) if path.suffix == '.gz' else path.read_bytes()
    records = [json.loads(line) for line in data.splitlines()]
    if args.baseline:
        path = Path(args.baseline)
        data = gzip.decompress(path.read_bytes()) if path.suffix == '.gz' else path.read_bytes()
        result = compare_quality(records, [json.loads(line) for line in data.splitlines()])
    else:
        result = audit(records)
    print(json.dumps(result, indent=2))
    if args.baseline and not result['qualified']:
        raise SystemExit(1)

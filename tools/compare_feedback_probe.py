"""Replay feedback traces from literal source state machines and all gate events."""
import argparse
import gzip
import json
from pathlib import Path


def audit(records):
    cases = {f'{frontend}-feedback_{case}' for frontend in ['native', 'yosys']
             for case in ['counter', 'fsm', 'state_enable']}
    assert len(records) == 6 and {r['case'] for r in records} == cases
    for record in records:
        case = record['case'].split('feedback_')[1]
        gates = {g['output']: g for g in record['gates']}
        outputs = record['outputs']
        names = ['q'] if case == 'state_enable' else ['q[0]', 'q[1]']
        assert set(outputs) == set(names)
        for signal in outputs.values():
            assert gates[signal]['kind'] == 'dff_p'
            assert gates[signal]['inputs'][1] == 'clk'
        pins = ['d', 'en'] if case == 'state_enable' else ['reset', 'en' if case == 'counter' else 'advance']
        controls = [3, 0, 1, 2, 3, 1, 0, 1] if case == 'state_enable' else [1, 2, 2, 2, 2, 0, 3, 2]
        protocol = [(control, clock) for cycle in range(64)
                    for control, clock in [(controls[cycle % 8], False),
                                           (controls[cycle % 8], True),
                                           (controls[cycle % 8] ^ 3, True),
                                           (controls[cycle % 8] ^ 3, False)]]
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
                    q = (q + 1) % (4 if case == 'counter' else 3)
            clock = next_clock
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
            last = {label: 0 for label in observed}
            for event in phase['events']:
                label = event['label']
                assert label in observed
                assert last[label] <= event['tick'] <= phase['ticks']
                assert isinstance(event['value'], bool) and event['value'] != observed[label]
                observed[label] = event['value']
                last[label] = event['tick']
            assert observed == expected
    return {'cases': 6, 'retained_phases_per_case': 256,
            'literal_state_oracle': True, 'all_gate_and_hidden_cell_events_replayed': True}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace')
    path = Path(parser.parse_args().trace)
    data = gzip.decompress(path.read_bytes()) if path.suffix == '.gz' else path.read_bytes()
    print(json.dumps(audit([json.loads(line) for line in data.splitlines()]), indent=2))

"""Replay falling/mixed-edge traces from literal source state transitions."""
import argparse
import gzip
import json
from pathlib import Path


def audit(records, cases=None):
    if cases is None:
        tops = ['negedge_dff', 'negedge_counter', 'mixed_edge_shift', 'hierarchy_negedge_dff']
        cases = {f'{frontend}-{top}': top for frontend in ['native', 'yosys'] for top in tops}
    assert len(records) == len(cases) and {r['case'] for r in records} == set(cases)
    for record in records:
        top = cases[record['case']]
        assert top in {'negedge_dff', 'negedge_counter', 'mixed_edge_shift', 'hierarchy_negedge_dff'}
        counter, mixed = top == 'negedge_counter', top == 'mixed_edge_shift'
        names = ['q[0]', 'q[1]'] if counter or mixed else ['q']
        pins = ['reset', 'en'] if counter else ['d']
        outputs = record['outputs']
        assert set(outputs) == set(names)
        gates = {g['output']: g for g in record['gates']}
        for bit, port in enumerate(names):
            gate = gates[outputs[port]]
            assert gate['kind'] == 'dff_p'
            if mixed and bit == 0:
                assert gate['inputs'][1] == 'clk'
            else:
                assert gate['inputs'][1] in gates, 'missing clock inverter'
                inverter = gates[gate['inputs'][1]]
                assert inverter['kind'] == 'nor' and inverter['inputs'] == ['clk']
        controls = [1, 2, 2, 2, 2, 0, 3, 2] if counter else [0, 3, 2, 1, 0, 2, 3, 1]
        protocol = [(control, clock) for cycle in range(64)
                    for control, clock in [(controls[cycle % 8] ^ 3, False),
                                           (controls[cycle % 8] ^ 3, True),
                                           (controls[cycle % 8], True),
                                           (controls[cycle % 8], False)]]
        assert [(p['control'], p['clock']) for p in record['phases']] == protocol

        def implementation(q, control, clock):
            values = {'clk': clock, **{pin: bool(control & (1 << bit)) for bit, pin in enumerate(pins)}}
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
            for bit, port in enumerate(names):
                output = outputs[port]
                gate = gates[output]
                cell_clock = clock if mixed and bit == 0 else not clock
                for role, expected in [('MData', values[output] if cell_clock else values[gate['inputs'][0]]),
                                       ('SData', values[output]), ('MLock', cell_clock),
                                       ('SLock', not cell_clock), ('InvC', not cell_clock)]:
                    label = f'cell:{output}:{role}'
                    channels[label] = expected
                    hidden[label] = (role, expected, cell_clock)
            return values, channels, hidden

        q, clock = 0, False
        _, observed, _ = implementation(q, 0, clock)
        assert record['power_on'] == observed
        covered = {q}
        for phase in record['phases']:
            control, next_clock = phase['control'], phase['clock']
            if clock and not next_clock:
                if counter:
                    if control & 1:
                        q = 0
                    elif control & 2:
                        q = (q + 1) % 4
                elif mixed:
                    q = (q & 1) * 3
                else:
                    q = int(bool(control & 1))
            elif not clock and next_clock and mixed:
                q = (q & 2) | (control & 1)
            clock = next_clock
            covered.add(q)
            values, expected, hidden = implementation(q, control, clock)
            assert phase['expected'] == q and phase['internal'] == values
            assert phase['inputs'] == {pin: values[pin] for pin in ['clk', *pins]}
            assert set(phase['cells']) == set(hidden)
            for label, (role, value, cell_clock) in hidden.items():
                cell = phase['cells'][label]
                assert cell['lit'] == value
                assert cell['kind'] == ('WallTorch' if role == 'InvC' else 'Repeater')
                if role in {'MData', 'SData'}:
                    assert cell['locked'] == (cell_clock if role == 'MData' else not cell_clock)
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
        assert covered == ({0, 1, 2, 3} if counter or mixed else {0, 1})
    return {'cases': len(cases), 'retained_phases_per_case': 256, 'literal_edge_oracle': True,
            'all_gate_and_hidden_cell_events_replayed': True, 'all_source_states_covered': True}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace')
    path = Path(parser.parse_args().trace)
    data = gzip.decompress(path.read_bytes()) if path.suffix == '.gz' else path.read_bytes()
    print(json.dumps(audit([json.loads(line) for line in data.splitlines()]), indent=2))

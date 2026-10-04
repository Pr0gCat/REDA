#!/usr/bin/env python3
"""Audit literal mixed-clock source states and every recorded physical event."""
import collections
import json
import sys
from pathlib import Path


def audit(trace):
    clocks = [False, False]
    state = [False, False]
    replay = dict(trace['power_on'])
    totals = collections.Counter()
    for number, phase in enumerate(trace['phases']):
        inputs = phase['inputs']
        next_clocks = [inputs['clk_a'], inputs['clk_b']]
        previous = state.copy()
        if not clocks[0] and next_clocks[0]:
            state[0] = False if inputs['reset_a'] else (not previous[0] if inputs['en'] else previous[0])
        if clocks[1] and not next_clocks[1]:
            state[1] = False if inputs['reset_b'] else previous[0]
        clocks = next_clocks
        assert state == phase['state'], (number, 'source state')
        values = dict(inputs)
        values.update((trace['outputs'][port], state[bit]) for bit, port in enumerate(['q_a', 'q_b']))
        pending = [gate for gate in trace['gates'] if gate['kind'] != 'dff_p']
        while pending:
            ready = [gate for gate in pending if all(name in values for name in gate['inputs'])]
            assert ready, (number, 'combinational cycle')
            for gate in ready:
                assert gate['kind'] == 'nor', gate
                values[gate['output']] = not any(values[name] for name in gate['inputs'])
                pending.remove(gate)
        assert values == phase['internal'], (number, 'gate oracle')
        expected = {'gate:' + gate['output']: values[gate['output']] for gate in trace['gates']}
        expected.update(('lamp:' + port, state[bit]) for bit, port in enumerate(['q_a', 'q_b']))
        for gate in trace['gates']:
            if gate['kind'] != 'dff_p':
                continue
            clock = values[gate['inputs'][1]]
            for role, value in {'MData': values[gate['output']] if clock else values[gate['inputs'][0]],
                                'SData': values[gate['output']], 'MLock': clock,
                                'SLock': not clock, 'InvC': not clock}.items():
                cell = gate['output'] + ':' + role
                assert phase['cells'][cell]['value'] == value, (number, cell)
                if role in ['MData', 'SData']:
                    assert phase['cells'][cell]['locked'] == (clock if role == 'MData' else not clock)
                expected['cell:' + cell] = value
        counts = collections.Counter()
        last_tick = -1
        for event in phase['events']:
            label = event['label']
            assert label in replay and 0 <= event['tick'] <= phase['ticks']
            assert event['tick'] >= last_tick, (number, 'event order')
            last_tick = event['tick']
            assert replay[label] != event['value'], (number, 'duplicate event', label)
            replay[label] = event['value']
            counts[label] += 1
        assert replay == expected, (number, 'full physical replay')
        for port in ['q_a', 'q_b']:
            assert counts['lamp:' + port] <= 1, (number, 'public pulse', port)
            assert counts['gate:' + trace['outputs'][port]] <= 1, (number, 'state pulse', port)
        totals.update(counts)
    assert len(trace['phases']) == 264
    return {'phases': len(trace['phases']), 'sites': len(replay), 'events': sum(totals.values()),
            'worst_phase_ticks_including_existing_16_tick_dwell': max(p['ticks'] for p in trace['phases']),
            'public_extra_transitions': 0, 'state_driver_extra_transitions': 0,
            'source_states_all_gates_cells_lamps_and_event_replay': 'pass'}


if __name__ == '__main__':
    traces = [json.loads(line.split(' ', 1)[1]) for line in Path(sys.argv[1]).read_text().splitlines()
              if line.startswith('MIXED_CLOCK_RESET_TRACE ')]
    assert len(traces) == 1
    print(json.dumps(audit(traces[0]), sort_keys=True))

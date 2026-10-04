"""Check every implemented gate and replay the frozen matrix boundary protocol."""
import json
import tempfile
from pathlib import Path
import sys
from audit_shared_matrix_premise import audit, read, expected_value
from compare_cover_probe import replay_events


def evaluate(graph, address):
    inputs = graph['inputs']
    assert len(inputs) == len(set(inputs)) > 0
    values = {name: bool(address & (1 << bit)) for bit, name in enumerate(inputs)}
    pending = list(graph['gates'])
    assert len({g['output'] for g in pending}) == len(pending)
    assert not set(inputs) & {g['output'] for g in pending}
    while pending:
        ready = [gate for gate in pending if all(n in values for n in gate['inputs'])]
        assert ready, 'cyclic or undriven implementation'
        for gate in ready:
            operands = [values[n] for n in gate['inputs']]
            kind = gate['kind']
            assert kind in ['nor', 'merge', 'buf', 'xor']
            assert kind != 'buf' or len(operands) == 1
            if kind == 'xor':
                assert len(operands) == 2
                value = operands[0] != operands[1]
            else:
                value = not any(operands) if kind == 'nor' else any(operands)
            values[gate['output']] = value
            pending.remove(gate)
    return values


def audit_certificate_waves(path, control=None):
    lines = read(path).decode().splitlines()
    assert lines[0].startswith('MATRIX_SOURCE ') and lines[-1].startswith('MATRIX_RESULT ')
    source = json.loads(lines[0].split(' ', 1)[1])
    assert len(source['inputs']) == len(set(source['inputs'])) > 0
    states = 1 << len(source['inputs'])
    phase_count = 2 * states * states
    assert len(lines) == phase_count + 2
    result_record = json.loads(lines[-1].split(' ', 1)[1])
    assert source['certified'] is False and result_record['certified'] is False
    assert result_record['phases'] == phase_count
    assert source['expanded_certificate_obtained'] is True
    graph = source['implementation']
    assert len(graph['inputs']) == len(source['inputs'])
    assert len(source['words']) == states
    gate_names = {gate['output'] for gate in graph['gates']}
    assert set(graph['bindings']) == gate_names
    assert len(set(graph['bindings'].values())) == len(gate_names)
    definitions = source['definitions']
    evaluations = [evaluate(graph, address) for address in range(states)]
    source_evaluations = [evaluate({'inputs': source['inputs'], 'gates': source['source_gates']}, address) for address in range(states)]
    assert source['words'] == [sum(int(values[name]) << bit for bit, name in enumerate(source['outputs'])) for values in source_evaluations]
    assert source['rows'] == ([a for a, word in enumerate(source['words']) if word] or [0])
    assert len({tuple(d['position']) for d in definitions.values()}) == len(definitions)
    assert len(graph['outputs']) == len(source['outputs'])
    for address, values in enumerate(evaluations):
        assert [values[n] for n in graph['outputs']] == [bool(source['words'][address] & (1 << bit)) for bit in range(len(graph['outputs']))]
    labels = set(definitions)
    extra = {label for label, definition in definitions.items() if definition['oracle']['kind'] == 'implemented_signal'}
    for label in extra:
        assert graph['bindings'][definitions[label]['oracle']['signal']] == label
    def ideal_sites(address):
        return {label: (evaluations[address][definition['oracle']['signal']]
                        if label in extra else expected_value(definition['oracle'], address,
                        source['words'], source['rows'], len(source['outputs'])))
                for label, definition in definitions.items()}

    previous = source['power_on']
    assert previous == ideal_sites(0)
    assert set(previous) == labels
    for signal, label in graph['bindings'].items():
        assert label in definitions and previous[label] == evaluations[0][signal]
    stripped = dict(source)
    stripped['definitions'] = {label: definition for label, definition in definitions.items() if label not in extra}
    stripped['power_on'] = {label: value for label, value in previous.items() if label not in extra}
    replay = ['MATRIX_SOURCE ' + json.dumps(stripped)]
    extra_transitions = 0
    all_hidden_transitions = 0
    protocol = [(a,b,p,[a,b][p]) for a in range(states) for b in range(states) for p in range(2)]
    for index, line in enumerate(lines[1:-1]):
        assert line.startswith('MATRIX_PHASE ')
        phase = json.loads(line.split(' ', 1)[1])
        assert (phase['from'],phase['to'],phase['phase'],phase['address']) == protocol[index]
        assert 0 <= phase['ticks'] <= 2000
        assert phase['internal'] == ideal_sites(phase['address'])
        wanted = [source_evaluations[phase['address']][name] for name in source['outputs']]
        assert phase['expected'] == phase['driver'] == phase['lamp'] == wanted
        assert phase['row_bad'] == [row != phase['address'] for row in source['rows']]
        assert set(phase['internal']) == labels
        values = evaluations[phase['address']]
        assert all(phase['internal'][label] == values[signal] for signal, label in graph['bindings'].items())
        events_by_label = {}
        for event in phase['events']:
            events_by_label.setdefault(event['label'], []).append(event)
        for label in labels:
            events = events_by_label.get(label, [])
            changes = max(0, len(events) - int(previous[label] != phase['internal'][label]))
            if not label.startswith('boundary'):
                all_hidden_transitions += changes
            if label in extra:
                extra_transitions += changes
        previous = replay_events(previous, phase['events'], 'label', phase['ticks'])
        assert previous == phase['internal']
        phase['internal'] = {label: value for label, value in phase['internal'].items() if label not in extra}
        phase['events'] = [event for event in phase['events'] if event['label'] not in extra]
        replay.append('MATRIX_PHASE ' + json.dumps(phase))
    replay.append(lines[-1])
    if control is None:
        # No invented control: this mode proves truth and event replay only.
        result = {'phases': phase_count, 'hidden_extra_transitions': all_hidden_transitions,
                  'historical_quality_compared': False, 'certified': False,
                  'release_qualified': False}
    else:
        with tempfile.TemporaryDirectory() as directory:
            sanitized = Path(directory) / 'protocol.log'
            sanitized.write_text('\n'.join(replay) + '\n')
            result = audit(sanitized, control)
        result['hidden_extra_transitions'] += extra_transitions
        result['historical_quality_compared'] = True
    result['all_implemented_gate_events_replayed'] = True
    result['implemented_gate_count'] = len(gate_names)
    result['total_observed_sites'] = len(labels)
    # Offline waveform replay does not itself prove a Rust physical certificate.
    result['expanded_certificate_claim_requires_bound_executable_and_source'] = True
    return result


if __name__ == '__main__':
    control = json.loads(read(sys.argv[2]).splitlines()[0]) if len(sys.argv) > 2 else None
    print(json.dumps(audit_certificate_waves(sys.argv[1], control), indent=2))

"""Check every implemented gate and replay the frozen matrix boundary protocol."""
import json
import tempfile
from pathlib import Path
import sys
from audit_shared_matrix_premise import audit, read
from compare_cover_probe import replay_events


def evaluate(graph, address):
    inputs = graph['inputs']
    assert len(inputs) == len(set(inputs)) == 4
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
            assert kind in ['nor', 'merge', 'buf']
            assert kind != 'buf' or len(operands) == 1
            values[gate['output']] = not any(operands) if kind == 'nor' else any(operands)
            pending.remove(gate)
    return values


def audit_certificate_waves(path, control):
    lines = read(path).decode().splitlines()
    assert len(lines) == 514
    source = json.loads(lines[0].split(' ', 1)[1])
    assert source['expanded_certificate_obtained'] is True
    graph = source['implementation']
    gate_names = {gate['output'] for gate in graph['gates']}
    assert set(graph['bindings']) == gate_names
    assert len(set(graph['bindings'].values())) == len(gate_names)
    definitions = source['definitions']
    evaluations = [evaluate(graph, address) for address in range(16)]
    assert len(graph['outputs']) == len(source['outputs'])
    for address, values in enumerate(evaluations):
        assert [values[n] for n in graph['outputs']] == [bool(source['words'][address] & (1 << bit)) for bit in range(len(graph['outputs']))]
    labels = set(definitions)
    extra = {label for label, definition in definitions.items() if definition['oracle']['kind'] == 'implemented_signal'}
    for label in extra:
        assert graph['bindings'][definitions[label]['oracle']['signal']] == label
    previous = source['power_on']
    assert set(previous) == labels
    for signal, label in graph['bindings'].items():
        assert label in definitions and previous[label] == evaluations[0][signal]
    stripped = dict(source)
    stripped['definitions'] = {label: definition for label, definition in definitions.items() if label not in extra}
    stripped['power_on'] = {label: value for label, value in previous.items() if label not in extra}
    replay = ['MATRIX_SOURCE ' + json.dumps(stripped)]
    extra_transitions = 0
    for line in lines[1:-1]:
        assert line.startswith('MATRIX_PHASE ')
        phase = json.loads(line.split(' ', 1)[1])
        assert set(phase['internal']) == labels
        values = evaluations[phase['address']]
        assert all(phase['internal'][label] == values[signal] for signal, label in graph['bindings'].items())
        for label in extra:
            events = [event for event in phase['events'] if event['label'] == label]
            extra_transitions += max(0, len(events) - int(previous[label] != phase['internal'][label]))
        previous = replay_events(previous, phase['events'], 'label', phase['ticks'])
        assert previous == phase['internal']
        phase['internal'] = {label: value for label, value in phase['internal'].items() if label not in extra}
        phase['events'] = [event for event in phase['events'] if event['label'] not in extra]
        replay.append('MATRIX_PHASE ' + json.dumps(phase))
    replay.append(lines[-1])
    with tempfile.TemporaryDirectory() as directory:
        sanitized = Path(directory) / 'protocol.log'
        sanitized.write_text('\n'.join(replay) + '\n')
        result = audit(sanitized, control)
    result['hidden_extra_transitions'] += extra_transitions
    result['all_implemented_gate_events_replayed'] = True
    result['implemented_gate_count'] = len(gate_names)
    result['total_observed_sites'] = len(labels)
    # Offline waveform replay does not itself prove a Rust physical certificate.
    result['expanded_certificate_claim_requires_bound_executable_and_source'] = True
    return result


if __name__ == '__main__':
    control = json.loads(read(sys.argv[2]).splitlines()[0])
    print(json.dumps(audit_certificate_waves(sys.argv[1], control), indent=2))

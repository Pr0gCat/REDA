"""Independently replay all recursive-search signals and compare retained histories."""
import argparse
import gzip
import json
from pathlib import Path

from compare_cover_probe import quality, replay_events


def read(path):
    data = Path(path).read_bytes()
    if str(path).endswith('.gz'):
        data = gzip.decompress(data)
    return [json.loads(line) for line in data.splitlines()]


def logic(source, address):
    values = {name: bool(address & (1 << bit))
              for bit, name in enumerate(source['inputs'])}
    remaining = list(source['gates'])
    while remaining:
        ready = [gate for gate in remaining if all(name in values for name in gate['inputs'])]
        assert ready, 'undriven or cyclic oracle graph'
        for gate in ready:
            operands = [values[name] for name in gate['inputs']]
            kind = gate['kind']
            assert kind.startswith(('Nor(', 'Or(')), kind
            assert gate['output'] not in values
            values[gate['output']] = not any(operands) if kind.startswith('Nor(') else any(operands)
            remaining.remove(gate)
    return values


def audit(records):
    source, *phases, result = records
    assert source['kind'] == 'source' and result['kind'] == 'result'
    assert source['tick_cap'] == 2000
    states = 1 << len(source['inputs'])
    protocol = [(a, b, phase, [a, b][phase]) for a in range(states)
                for b in range(states) for phase in range(2)]
    assert result['phases'] == len(phases) == 2 * states * states
    assert [(p['from'], p['to'], p['phase'], p['address']) for p in phases] == protocol
    definitions = source['definitions']
    labels = {'gate:' + gate['output'] for gate in source['gates']}
    labels |= {f'{channel}:{bit}' for channel in ['driver', 'lamp']
               for bit in range(len(source['outputs']))}
    assert set(definitions) == labels == set(source['power_on'])
    initial = logic(source, 0)
    assert source['power_on'] == {label: initial[definition[1]]
                                 for label, definition in definitions.items()}
    observed = dict(source['power_on'])
    previous_end = None
    public = []
    arrivals = []
    for phase in phases:
        assert phase['kind'] == 'phase' and 0 <= phase['ticks'] <= 2000
        if previous_end is not None:
            assert phase['start'] == previous_end, 'continuous simulator history required'
        previous_end = phase['start'] + phase['ticks']
        ideal = logic(source, phase['address'])
        events = [{**event, 'tick': event['tick'] - phase['start']}
                  for event in phase['events']]
        observed = replay_events(observed, events, 'label', phase['ticks'])
        assert observed == phase['signals'] == {
            label: ideal[definition[1]] for label, definition in definitions.items()}
        width = len(source['outputs'])
        boundary_labels = [f'{channel}:{bit}' for channel in ['driver', 'lamp']
                           for bit in range(width)]
        boundary = [{**event, 'label': str(boundary_labels.index(event['label']))}
                    for event in events if event['label'] in boundary_labels]
        public.append({**phase, 'expected': [ideal[name] for name in source['outputs']],
                       'events': boundary})
        arrivals.append([max((e['tick'] for e in events if e['label'] == label), default=0)
                         for label in boundary_labels])
    assert source['evaluations_used'] == len(source['trace'])
    assert [p['proposal_index'] for p in source['trace']] == list(range(len(source['trace'])))
    assert source['metrics']['emitted_world_fingerprint'] == source['world_fingerprint']
    return {'case': source['case'], 'budget': source['budget'], 'phases': len(phases),
            'hidden_extra_transitions': 0, 'quality': [quality({'phases': public}, p) for p in range(2)],
            'settlements': [p['ticks'] for p in phases], 'arrivals': arrivals,
            'world_fingerprint': source['world_fingerprint'],
            'evaluations_used': source['evaluations_used'], 'trace': source['trace']}


def compare(control, candidate):
    before, after = audit(control), audit(candidate)
    for key in ['inputs', 'outputs', 'gates', 'tick_cap']:
        assert control[0][key] == candidate[0][key]
    regressions = []
    improvements = []
    for phase in range(2):
        a, b = before['quality'][phase], after['quality'][phase]
        for key in sorted(a.keys() | b.keys()):
            if b.get(key, 0) > a.get(key, 0):
                regressions.append(f'quality:{phase}:{key}')
            if b.get(key, 0) < a.get(key, 0):
                improvements.append(f'quality:{phase}:{key}')
    for index, (a, b) in enumerate(zip(before['settlements'], after['settlements'])):
        if b > a:
            regressions.append(f'settlement:{index}')
        if b < a:
            improvements.append(f'settlement:{index}')
    for index, (a, b) in enumerate(zip(before['arrivals'], after['arrivals'])):
        assert len(a) == len(b)
        for channel, (old, new) in enumerate(zip(a, b)):
            if new > old:
                regressions.append(f'arrival:{index}:{channel}')
            if new < old:
                improvements.append(f'arrival:{index}:{channel}')
    return {'case': after['case'], 'budget': after['budget'], 'phases': after['phases'],
            'hidden_extra_transitions': 0, 'quality': after['quality'],
            'regressions': regressions, 'improvements': improvements,
            'evaluations_used': after['evaluations_used'], 'trace': after['trace'],
            'world_fingerprint': after['world_fingerprint'], 'release_qualified': False}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('control')
    parser.add_argument('candidate')
    args = parser.parse_args()
    print(json.dumps(compare(read(args.control), read(args.candidate)), indent=2))

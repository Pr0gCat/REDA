"""Compare a full family with frozen shipping controls; wrong controls are separate."""
import gzip
import json
import sys

from compare_matrix_probe import quality

with gzip.open(sys.argv[1], 'rt') as f:
    fixed = [json.loads(l) for l in f]
with open(sys.argv[2]) as f:
    rows = [json.loads(l) for l in f]
assert len(fixed) == len(rows) == 4096
cases = []
for encoding, (old, new) in enumerate(zip(fixed, rows)):
    words = [(encoding >> (3 * i)) & 7 for i in range(4)]
    assert old['encoding'] == new['encoding'] == encoding and old['words'] == new['words'] == words
    assert old['control'] == new['control'], encoding
    for arm in [new['control'], new['candidate']]:
        if 'generation_error' in arm:
            assert arm is new['candidate'] and arm['correct'] is False and (arm['certified'] is False) and (arm['phases'] == [])
            continue
        if arm is new['candidate'] and (not arm['correct']) and (len(arm['phases']) < 32):
            continue
        assert len(arm['phases']) == 32
        assert [(p['from'], p['to'], p['phase']) for p in arm['phases']] == [(a, b, k) for a in range(4) for b in range(4) for k in range(2)]
        correct = True
        for p in arm['phases']:
            assert p['address'] == (p['from'] if p['phase'] == 0 else p['to'])
            assert p['expected'] == [bool(words[p['address']] & 1 << bit) for bit in range(3)]
            correct &= p['driver'] == p['expected'] and p['lamp'] == p['expected']
            for kind in ['driver', 'lamp']:
                for bit, value in enumerate(p[kind]):
                    events = [e for e in p['events'] if e['label'] == f'{kind}{bit}']
                    assert all((0 <= e['tick'] <= p['ticks'] for e in events))
                    assert not events or events[-1]['value'] == value
        assert arm['correct'] == correct
    a = quality(new['control'], words) if new['control']['correct'] else None
    b = quality(new['candidate'], words) if new['candidate']['correct'] else None
    worse = [k for k in a if b[k] > a[k]] if a is not None and b is not None else None
    better = [k for k in a if b[k] < a[k]] if a is not None and b is not None else None
    cases.append({'encoding': encoding, 'words': words, 'control': a, 'candidate': b, 'regressed': worse, 'improved': better})
summary = {
    'cases': 4096,
    'control_correct': sum(c['control'] is not None for c in cases),
    'candidate_correct': sum(c['candidate'] is not None for c in cases),
    'truth_recovered': sum(c['control'] is None and c['candidate'] is not None for c in cases),
    'regressed_on_correct_controls': sum(bool(c['regressed']) for c in cases),
    'improved_without_regression_on_correct_controls': sum(
        bool(c['improved']) and not c['regressed'] for c in cases
    ),
    'tied_on_correct_controls': sum(
        c['control'] is not None and c['candidate'] is not None
        and not c['improved'] and not c['regressed'] for c in cases
    ),
    'all_fixed_controls_exact': True,
}
print(json.dumps({'summary': summary, 'cases': cases}))
if summary['candidate_correct'] != 4096 or summary['regressed_on_correct_controls']:
    sys.exit(1)

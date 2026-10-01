"""Reject a prospective target-metric policy when it violates retained caps.

Diagnostic only; this does not configure or enable automatic compilation.
Usage: python3 tools/probe_default_selection_tradeoff.py FAMILY.jsonl.gz CAPS.json
"""
import collections
import gzip
import json
import sys

from compare_matrix_probe import quality

assert len(sys.argv) == 3, __doc__
with gzip.open(sys.argv[1], 'rt') as stream:
    rows = [json.loads(line) for line in stream]
with open(sys.argv[2]) as stream:
    caps = json.load(stream)
assert len(rows) == 4096
cap_tables = {row[0]: row[1:] for row in caps['tables']}
metric_names = {
    'worst_driver_change_ticks': 'driver_last_change_ticks',
    'worst_driver_pulse_ticks': 'driver_worst_pulse_ticks',
    'total_driver_pulse_ticks': 'driver_total_pulse_ticks',
}
choices = collections.Counter()
regressions = []
for encoding, row in enumerate(rows):
    assert row['encoding'] == encoding
    assert row['words'] == [(encoding >> (3 * address)) & 7 for address in range(4)]
    player = row['candidate']
    assert player['correct'] and player['certified']
    player_quality = quality(player, row['words'])
    shipping = row['control']
    shipping_quality = quality(shipping, row['words']) if shipping['correct'] else None
    choose_player = shipping_quality is None or all(
        player_quality[metric] <= value for metric, value in shipping_quality.items()
    )
    choices['player' if choose_player else 'shipping'] += 1
    selected = player_quality if choose_player else shipping_quality
    if encoding in cap_tables:
        worse = [metric for metric, cap in zip(caps['metrics'], cap_tables[encoding])
                 if selected[metric_names.get(metric, metric)] > cap]
        if worse:
            regressions.append({'encoding': encoding, 'metrics': worse})
print(json.dumps({'cases': len(rows), 'choices': choices,
                  'retained_cap_tables': len(cap_tables),
                  'cap_regressions': len(regressions), 'witnesses': regressions[:5],
                  'setup_evaluated': False, 'automatic_selection_enabled': False}))

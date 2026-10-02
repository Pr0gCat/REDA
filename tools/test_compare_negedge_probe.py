"""Verify retained negative-edge evidence and reject edge/state/event corruption."""
import copy
import gzip
import json
from pathlib import Path
import unittest

from compare_negedge_probe import audit


class NegativeEdgeAudit(unittest.TestCase):
    def test_real_traces_and_corruptions(self):
        path = Path(__file__).resolve().parents[1] / 'docs/native-negedge-2026-10-02-trace.jsonl.gz'
        records = [json.loads(line) for line in gzip.decompress(path.read_bytes()).splitlines()]
        self.assertTrue(audit(records)['all_source_states_covered'])
        for mutation in ['polarity', 'state', 'protocol', 'internal', 'lock', 'events', 'power_on']:
            with self.subTest(mutation=mutation):
                broken = copy.deepcopy(records)
                phase = broken[0]['phases'][0]
                if mutation == 'polarity':
                    gate = next(g for g in broken[0]['gates'] if g['kind'] == 'nor' and g['inputs'] == ['clk'])
                    gate['inputs'] = ['d']
                elif mutation == 'state':
                    phase['expected'] ^= 1
                elif mutation == 'protocol':
                    phase['clock'] = not phase['clock']
                elif mutation == 'internal':
                    signal = broken[0]['gates'][0]['output']
                    phase['internal'][signal] = not phase['internal'][signal]
                elif mutation == 'lock':
                    label = next(k for k in phase['cells'] if k.endswith(':MData'))
                    phase['cells'][label]['locked'] = not phase['cells'][label]['locked']
                elif mutation == 'power_on':
                    label = next(k for k in broken[0]['power_on'] if k.startswith('lamp:'))
                    broken[0]['power_on'][label] = True
                else:
                    next(p for p in broken[0]['phases'] if p['events'])['events'].clear()
                with self.assertRaises(AssertionError):
                    audit(broken)


if __name__ == '__main__':
    unittest.main()

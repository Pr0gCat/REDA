"""Require real statement-reset traces and reject state, protocol, gate and event corruption."""
import copy
import gzip
import json
from pathlib import Path
import unittest

from compare_reset_probe import audit


class ResetAudit(unittest.TestCase):
    def test_real_traces_and_corruptions(self):
        path = Path(__file__).resolve().parents[1] / 'docs/native-statement-reset-2026-10-02-trace.jsonl.gz'
        records = [json.loads(line) for line in gzip.decompress(path.read_bytes()).splitlines()]
        self.assertEqual(audit(records)['cases'], 12)
        for index in [0, 8]:
            for mutation in ['state', 'protocol', 'clock', 'internal', 'lock', 'events', 'power_on']:
                with self.subTest(mutation=mutation):
                    broken = copy.deepcopy(records)
                    phase = broken[index]['phases'][0]
                    if mutation == 'state':
                        phase['expected'] ^= 1
                    elif mutation == 'protocol':
                        phase['control'] ^= 1
                    elif mutation == 'clock':
                        gate = next(g for g in broken[index]['gates'] if g['kind'] == 'dff_p')
                        gate['inputs'][1] = 'wrong_clock'
                    elif mutation == 'internal':
                        gate = broken[index]['gates'][0]['output']
                        phase['internal'][gate] = not phase['internal'][gate]
                    elif mutation == 'lock':
                        label = next(k for k in phase['cells'] if k.endswith(':SData'))
                        phase['cells'][label]['locked'] = not phase['cells'][label]['locked']
                    elif mutation == 'power_on':
                        label = next(iter(broken[index]['power_on']))
                        broken[index]['power_on'][label] = not broken[index]['power_on'][label]
                    else:
                        next(p for p in broken[index]['phases'] if p['events'])['events'].clear()
                    with self.assertRaises(AssertionError):
                        audit(broken)


if __name__ == '__main__':
    unittest.main()

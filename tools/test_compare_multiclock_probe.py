"""Check the independent oracle against actual retained traces and corruptions."""
import copy
import gzip
import json
from pathlib import Path
import unittest

from compare_multiclock_probe import audit


class MulticlockTraceAudit(unittest.TestCase):
    def test_real_traces_and_wrong_clock_or_missing_events(self):
        path = Path(__file__).resolve().parents[1] / 'docs' / 'native-multiclock-2026-10-02-trace.jsonl.gz'
        records = [json.loads(line) for line in gzip.decompress(path.read_bytes()).splitlines()]
        self.assertTrue(audit(records)['public_events_replayed'])
        for mutation in ['clock', 'state', 'events', 'protocol']:
            with self.subTest(mutation=mutation):
                broken = copy.deepcopy(records)
                if mutation == 'clock':
                    broken[0]['gates'][0]['inputs'][1] = 'clk_wrong'
                elif mutation == 'state':
                    broken[0]['phases'][0]['expected'][0] = True
                elif mutation == 'protocol':
                    broken[0]['phases'][0]['clocks'] = 3
                else:
                    phase = next(p for p in broken[0]['phases'] if p['events'])
                    phase['events'].clear()
                with self.assertRaises(AssertionError):
                    audit(broken)


if __name__ == '__main__':
    unittest.main()

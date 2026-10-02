"""Keep retained internal-waveform auditing tied to actual generated circuits."""
import copy
import gzip
import json
from pathlib import Path
import unittest

from compare_cover_probe import compare


class InternalTraceAudit(unittest.TestCase):
    def test_generated_trace_and_corrupted_events(self):
        docs = Path(__file__).resolve().parents[1] / 'docs'
        with gzip.open(docs / 'generator-flat-refresh-certification-2026-10-02-native-control.jsonl.gz', 'rt') as stream:
            control = json.loads(stream.readline())
        with gzip.open(docs / 'generator-cover-internal-timing-2026-10-02-pruned.jsonl.gz', 'rt') as stream:
            rows = [json.loads(line) for line in stream]
        self.assertTrue(compare(control, rows[0])['accepted'])
        self.assertFalse(compare(control, rows[1])['accepted'])
        index = next(i for i, p in enumerate(rows[1]['result']['Ok']['phases']) if p['internal_events'])
        for mutation in ['late', 'unknown', 'delete', 'repeat']:
            with self.subTest(mutation=mutation):
                row = copy.deepcopy(rows[1])
                phase = row['result']['Ok']['phases'][index]
                event = phase['internal_events'][0]
                if mutation == 'late':
                    event['tick'] = phase['ticks'] + 1
                elif mutation == 'unknown':
                    event['signal'] = 'absent'
                elif mutation == 'delete':
                    phase['internal_events'].pop(0)
                else:
                    phase['internal_events'].insert(1, copy.deepcopy(event))
                with self.assertRaises(AssertionError):
                    compare(control, row)


if __name__ == '__main__':
    unittest.main()

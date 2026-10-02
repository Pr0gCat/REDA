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

    def test_complete_landing_replay_and_operand_bindings(self):
        docs = Path(__file__).resolve().parents[1] / 'docs'
        with gzip.open(docs / 'generator-flat-refresh-certification-2026-10-02-native-control.jsonl.gz', 'rt') as stream:
            control = json.loads(stream.readline())
        with gzip.open(docs / 'generator-route-arrival-experiment-2026-10-02-landings.jsonl.gz', 'rt') as stream:
            row = json.loads(stream.readline())
        self.assertTrue(compare(control, row)['accepted'])
        index = next(i for i, p in enumerate(row['result']['Ok']['phases']) if p['landing_events'])
        for mutation in ['late', 'unknown', 'delete', 'repeat', 'binding', 'coverage']:
            with self.subTest(mutation=mutation):
                candidate = copy.deepcopy(row)
                record = candidate['result']['Ok']
                phase = record['phases'][index]
                event = phase['landing_events'][0]
                if mutation == 'late':
                    event['tick'] = phase['ticks'] + 1
                elif mutation == 'unknown':
                    event['landing'] = 'absent'
                elif mutation == 'delete':
                    phase['landing_events'].pop(0)
                elif mutation == 'repeat':
                    phase['landing_events'].insert(1, copy.deepcopy(event))
                elif mutation == 'binding':
                    record['landing_sites'][event['landing']]['source'] = 'absent'
                else:
                    site = record['landing_sites'][event['landing']]
                    removed = {key for key, value in record['landing_sites'].items()
                               if (value['receiver'], value['input_index']) == (site['receiver'], site['input_index'])}
                    for key in removed:
                        del record['landing_sites'][key]
                        del record['initial_landings'][key]
                    for p in record['phases']:
                        for key in removed:
                            del p['landings'][key]
                        p['landing_events'] = [e for e in p['landing_events'] if e['landing'] not in removed]
                with self.assertRaises(AssertionError):
                    compare(control, candidate)
        with gzip.open(docs / 'generator-route-arrival-experiment-2026-10-02-balanced-terminal.jsonl.gz', 'rt') as stream:
            failed = [json.loads(line) for line in stream][-1]
        self.assertIn('qualification: internal', failed['result']['Err'])
        self.assertFalse(compare(control, failed)['accepted'])


if __name__ == '__main__':
    unittest.main()

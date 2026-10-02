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

    def test_qualified_refresh_defaults_and_literal_rom(self):
        docs = Path(__file__).resolve().parents[1] / 'docs'

        def load(name):
            with gzip.open(docs / name, 'rt') as stream:
                return json.loads(stream.readline())

        prefix = 'generator-qualified-refresh-reduction-2026-10-02-'
        for frontend in ['native', 'yosys']:
            control = load('generator-flat-refresh-certification-2026-10-02-' + frontend + '-control.jsonl.gz')
            candidate = load(prefix + frontend + '.jsonl.gz')
            self.assertTrue(compare(control, candidate)['accepted'])
        control = load(prefix + 'rom-control.jsonl.gz')
        candidate = load(prefix + 'rom-default.jsonl.gz')
        self.assertTrue(compare(control, candidate)['accepted'])
        words = [0x15, 0x62, 0x3a, 0x41, 0x0c, 0x71, 0x26, 0x5b,
                 0x64, 0x19, 0x4f, 0x32, 0x75, 0x0a, 0x53, 0x2d]
        for row in [control, candidate]:
            phases = row['result']['Ok']['phases']
            self.assertEqual(len(phases), 512)
            for phase in phases:
                self.assertEqual(phase['expected'],
                                 [bool(words[phase['address']] & (1 << bit)) for bit in range(7)])

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

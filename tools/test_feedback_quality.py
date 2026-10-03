"""Reject masked output delays, wider glitches and extra stored/public pulses."""
import copy
import gzip
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from compare_feedback_probe import audit, compare_quality


class FeedbackQuality(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.path = Path(__file__).resolve().parents[1] / 'docs/native-feedback-2026-10-02-trace.jsonl.gz'
        cls.records = [json.loads(line) for line in gzip.decompress(cls.path.read_bytes()).splitlines()]

    def test_identity_and_masked_output_delay(self):
        self.assertTrue(compare_quality(self.records, self.records)['qualified'])
        changed = copy.deepcopy(self.records)
        for record in changed:
            for index, phase in enumerate(record['phases']):
                public = [e for e in phase['events'] if e['label'].startswith('lamp:')]
                if len(public) >= 2 and min(e['tick'] for e in public) < max(e['tick'] for e in public):
                    event = min(public, key=lambda e: e['tick'])
                    event['tick'] += 1
                    result = compare_quality(changed, self.records)
                    self.assertFalse(result['qualified'])
                    self.assertTrue(any(r.get('channel') == event['label'] and
                                        r['phase'] == index and r['metric'] == 'last_arrival_ticks'
                                        for r in result['regressions']))
                    self.assertFalse(any(r['metric'] == 'settlement_ticks' for r in result['regressions']))
                    with tempfile.TemporaryDirectory() as folder:
                        trace = Path(folder) / 'delayed.jsonl'
                        trace.write_text('\n'.join(json.dumps(r) for r in changed) + '\n')
                        command = [sys.executable, str(Path(__file__).with_name('compare_feedback_probe.py')),
                                   str(trace), '--baseline', str(self.path)]
                        process = subprocess.run(command, capture_output=True, text=True)
                        self.assertEqual(process.returncode, 1, process.stderr)
                        self.assertFalse(json.loads(process.stdout)['qualified'])
                    return
        self.fail('real trace contains no distinct public arrivals')

    def test_wrong_target_pulse_and_correct_value_gap(self):
        widened = copy.deepcopy(self.records)
        found = False
        for record in widened:
            for phase in record['phases']:
                by_label = {}
                for event in phase['events']:
                    by_label.setdefault(event['label'], []).append(event)
                pair = next((events for events in by_label.values()
                             if len(events) >= 2 and events[0]['tick'] > 0
                             and events[0]['value'] != events[-1]['value']), None)
                if pair:
                    pair[0]['tick'] -= 1
                    result = compare_quality(widened, self.records)
                    self.assertFalse(result['qualified'])
                    self.assertTrue(any(r['metric'].startswith('pulse_width_ticks:')
                                        for r in result['regressions']))
                    self.assertFalse(any(r['metric'] in {'last_arrival_ticks', 'settlement_ticks'}
                                         for r in result['regressions']))
                    found = True
                    break
            if found:
                break
        self.assertTrue(found, 'real trace contains no internal pulse')

        # Synthetic internal waveform: only its correct-value gap grows.
        baseline = copy.deepcopy(self.records)
        record = baseline[0]
        nor_outputs = {g['output'] for g in record['gates'] if g['kind'] == 'nor'}
        for phase_index, phase in enumerate(record['phases']):
            for event_index, event in enumerate(phase['events']):
                if (event['label'].removeprefix('gate:') in nor_outputs and event['tick'] > 0
                        and event['tick'] + 2 <= phase['ticks']
                        and sum(e['label'] == event['label'] for e in phase['events']) == 1):
                    phase['events'].extend([
                        dict(event, tick=event['tick'] + 1, value=not event['value']),
                        dict(event, tick=event['tick'] + 2),
                    ])
                    earlier_correct = copy.deepcopy(baseline)
                    earlier_correct[0]['phases'][phase_index]['events'][event_index]['tick'] -= 1
                    self.assertTrue(compare_quality(earlier_correct, baseline)['qualified'],
                                    'longer correct-value interval must not count as a wider glitch')
                    return
        self.fail('real trace contains no internal transition for waveform control')

    def test_extra_storage_and_public_pulses_are_rejected(self):
        record = self.records[0]
        port, output = next(iter(record['outputs'].items()))
        for label in [f'gate:{output}', f'cell:{output}:SData', f'lamp:{port}']:
            with self.subTest(channel=label):
                broken = copy.deepcopy(self.records)
                phase = broken[0]['phases'][0]
                value = phase['internal'][output]
                phase['events'].extend([
                    {'label': label, 'tick': phase['ticks'], 'value': not value},
                    {'label': label, 'tick': phase['ticks'], 'value': value},
                ])
                with self.assertRaisesRegex(AssertionError, 'extra storage/public pulse'):
                    audit(broken)


if __name__ == '__main__':
    unittest.main()

"""Retained full-circuit matrix premises and corrupted-event controls."""
import gzip
import json
from pathlib import Path
import tempfile
import unittest

from audit_shared_matrix_premise import audit


class MatrixPremiseAudit(unittest.TestCase):
    def test_complete_seven_segment_rom_and_corruptions(self):
        root = Path(__file__).resolve().parents[1]
        controls = {
            'seven': root / 'docs/native-module-instances-2026-10-02-seven-native.jsonl.gz',
            'rom': root / 'docs/generator-qualified-refresh-reduction-2026-10-02-rom-control.jsonl.gz',
        }
        for family, control_path in controls.items():
            control = json.loads(gzip.decompress(control_path.read_bytes()).splitlines()[0])
            log = root / f'docs/shared-matrix-balanced-output-2026-10-03-{family}-repeat1.log.gz'
            result = audit(log, control)
            self.assertEqual(result['phases'], 512)
            self.assertTrue(result['premise_replay_passed'])
            self.assertFalse(result['release_qualified'])
            self.assertFalse(result['certified'])
            self.assertEqual(result['hidden_extra_transitions'], 0)
            self.assertEqual(result['public_phase_channel_arrival_regressions'], [])
            self.assertEqual(result['settlement_phase_regressions'], [])
            self.assertTrue(all(not q['regressed'] for q in result['quality'].values()))
        control = json.loads(gzip.decompress(controls['seven'].read_bytes()).splitlines()[0])
        log = root / 'docs/shared-matrix-balanced-output-2026-10-03-seven-repeat1.log.gz'
        original = gzip.decompress(log.read_bytes()).decode().splitlines()
        for corruption in ['missing_event', 'wrong_internal', 'false_certificate', 'truncated']:
            lines = list(original)
            if corruption == 'false_certificate':
                result = json.loads(lines[-1].split(' ', 1)[1])
                result['certified'] = True
                lines[-1] = 'MATRIX_RESULT ' + json.dumps(result)
            elif corruption == 'truncated':
                del lines[-2]
            else:
                index = next(i for i, line in enumerate(lines) if line.startswith('MATRIX_PHASE ')
                             and json.loads(line.split(' ', 1)[1])['events'])
                phase = json.loads(lines[index].split(' ', 1)[1])
                if corruption == 'missing_event':
                    del phase['events'][0]
                else:
                    label = next(iter(phase['internal']))
                    phase['internal'][label] = not phase['internal'][label]
                lines[index] = 'MATRIX_PHASE ' + json.dumps(phase)
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / 'corrupt.log'
                path.write_text('\n'.join(lines) + '\n')
                with self.assertRaises(AssertionError, msg=corruption):
                    audit(path, control)


if __name__ == '__main__':
    unittest.main()

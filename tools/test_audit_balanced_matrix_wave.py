"""Complete matrix waveform bindings and deliberately corrupted records."""
import gzip
import json
from pathlib import Path
import tempfile
import unittest

from audit_balanced_matrix_wave import audit_certificate_waves


class BalancedMatrixWaveAudit(unittest.TestCase):
    def test_complete_bindings_and_corruptions(self):
        root = Path(__file__).resolve().parents[1]
        controls = {
            'seven': 'native-module-instances-2026-10-02-seven-native',
            'rom': 'generator-qualified-refresh-reduction-2026-10-02-rom-control',
        }
        for family, reference in controls.items():
            control = json.loads(gzip.decompress((root / f'docs/{reference}.jsonl.gz').read_bytes()).splitlines()[0])
            log = root / f'docs/balanced-matrix-expanded-certificate-2026-10-03-{family}-repeat1.log.gz'
            result = audit_certificate_waves(log, control)
            self.assertTrue(result['all_implemented_gate_events_replayed'])
            self.assertEqual(result['hidden_extra_transitions'], 0)
            self.assertEqual(result['phases'], 512)
            self.assertEqual(result['public_phase_channel_arrival_regressions'], [])
            self.assertEqual(result['settlement_phase_regressions'], [])
            self.assertFalse(result['release_qualified'])
        original = gzip.decompress((root / 'docs/balanced-matrix-expanded-certificate-2026-10-03-seven-repeat1.log.gz').read_bytes()).decode().splitlines()
        control = json.loads(gzip.decompress((root / f"docs/{controls['seven']}.jsonl.gz").read_bytes()).splitlines()[0])
        for corruption in ['missing_binding', 'wrong_graph', 'wrong_sample', 'missing_event', 'no_certificate', 'truncated']:
            lines = list(original)
            header = json.loads(lines[0].split(' ', 1)[1])
            if corruption == 'missing_binding':
                header['implementation']['bindings'].pop(next(iter(header['implementation']['bindings'])))
            elif corruption == 'wrong_graph':
                gate = next(g for g in header['implementation']['gates'] if g['kind'] == 'buf')
                gate['kind'] = 'nor'
            elif corruption == 'no_certificate':
                header['expanded_certificate_obtained'] = False
            elif corruption == 'truncated':
                del lines[-2]
            else:
                index = next(i for i, line in enumerate(lines) if line.startswith('MATRIX_PHASE ')
                             and any(e['label'].startswith('implemented:') for e in json.loads(line.split(' ', 1)[1])['events']))
                phase = json.loads(lines[index].split(' ', 1)[1])
                label = next(e['label'] for e in phase['events'] if e['label'].startswith('implemented:'))
                if corruption == 'wrong_sample':
                    phase['internal'][label] = not phase['internal'][label]
                else:
                    phase['events'].pop(next(i for i, e in enumerate(phase['events']) if e['label'] == label))
                lines[index] = 'MATRIX_PHASE ' + json.dumps(phase)
            lines[0] = 'MATRIX_SOURCE ' + json.dumps(header)
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / 'corrupt.log'
                path.write_text('\n'.join(lines) + '\n')
                with self.assertRaises(AssertionError, msg=corruption):
                    audit_certificate_waves(path, control)


if __name__ == '__main__':
    unittest.main()

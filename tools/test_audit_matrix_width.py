"""Width-independent replay and corruption checks; no physical-quality claim."""
import copy
import json
import tempfile
import unittest
from pathlib import Path
from audit_balanced_matrix_wave import audit_certificate_waves
from audit_shared_matrix_premise import expected_value


def record(width):
    states = 1 << width
    names = [f'a{bit}' for bit in range(width)]
    words = [address & 1 for address in range(states)]
    rows = [address for address in range(states) if words[address]]
    definitions = {f'boundary{bit}': {'position': [bit, 0, 0],
        'oracle': {'kind': 'prefix', 'bit': 0, 'last': len(rows) - 1}}
        for bit in range(2)}
    definitions.update({f'input{bit}': {'position': [bit, 1, 0],
        'oracle': {'kind': 'bit', 'bit': bit, 'inverted': False}}
        for bit in range(width)})
    def values(address):
        return {label: expected_value(d['oracle'], address, words, rows, 1)
                for label, d in definitions.items()}
    gates = [{'output': 'q', 'inputs': [names[0]], 'kind': 'buf'}]
    source = {'inputs': names, 'outputs': ['q'], 'source_gates': gates,
        'words': words, 'rows': rows, 'definitions': definitions,
        'power_on': values(0), 'certified': False,
        'expanded_certificate_obtained': True,
        'implementation': {'inputs': names, 'outputs': ['q'], 'gates': gates,
            'bindings': {'q': 'boundary0'}}}
    phases = []
    previous = values(0)
    for before in range(states):
        for after in range(states):
            for phase, address in enumerate([before, after]):
                current = values(address)
                events = [{'tick': 1, 'label': label, 'value': value}
                          for label, value in current.items() if previous[label] != value]
                phases.append({'from': before, 'to': after, 'phase': phase,
                    'address': address, 'ticks': int(bool(events)),
                    'internal': current, 'expected': [bool(words[address])],
                    'driver': [bool(words[address])], 'lamp': [bool(words[address])],
                    'row_bad': [row != address for row in rows], 'events': events})
                previous = current
    return source, phases


class MatrixWidthReplay(unittest.TestCase):
    def replay(self, source, phases):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'wave.log'
            path.write_text('MATRIX_SOURCE ' + json.dumps(source) + '\n' +
                ''.join('MATRIX_PHASE ' + json.dumps(p) + '\n' for p in phases) +
                'MATRIX_RESULT ' + json.dumps({'phases': 2 * (1 << len(source['inputs'])) ** 2,
                                              'certified': False}) + '\n')
            return audit_certificate_waves(path)

    def test_full_width_protocols(self):
        for width in [4, 5, 6]:
            source, phases = record(width)
            result = self.replay(source, phases)
            self.assertEqual(result['phases'], 2 * (1 << width) ** 2)
            self.assertFalse(result['release_qualified'])
            self.assertFalse(result['historical_quality_compared'])

    def test_corruptions_rejected(self):
        source, phases = record(5)
        with self.assertRaises(AssertionError):
            self.replay(source, phases[:-1])
        corrupted = copy.deepcopy(phases)
        next(p for p in corrupted if p['events'])['events'] = []
        with self.assertRaises(AssertionError):
            self.replay(source, corrupted)
        corrupted = copy.deepcopy(source)
        corrupted['words'][31] = 0
        with self.assertRaises(AssertionError):
            self.replay(corrupted, phases)
        with self.assertRaises(AssertionError):
            expected_value({'kind': 'bit', 'bit': 5, 'inverted': False},
                           0, source['words'], source['rows'], 1)
        with self.assertRaises(AssertionError):
            expected_value({'kind': 'mismatch', 'row': 32, 'mask': 1},
                           0, source['words'], source['rows'], 1)


if __name__ == '__main__':
    unittest.main()

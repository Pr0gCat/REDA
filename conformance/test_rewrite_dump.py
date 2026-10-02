"""Dump contract tests; set REDA_DUMP_FIXTURE_DIR to mc_dump's test fixtures."""
import os
import tempfile
import unittest
from pathlib import Path

from circuit_conformance import (
    parse_dump, evaluate_expected, block_state_string, build_phase_commands,
    expected_block_at, read_state,
)


class RewriteDumpTest(unittest.TestCase):
    def parse_text(self, text):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'dump.txt'
            path.write_text(text)
            return parse_dump(path)

    def test_unknown_observation_and_wrong_arity_are_rejected(self):
        for text in (
            'SIZE 1 1 1\nGATEOUT y 0 0 0 Solid\nGATE y - nor\n',
            'SIZE 1 1 1\nINPUT a 0 0 0\nGATE y a andnot\n',
            'SIZE 1 1 1\nGATE y - buf\n',
            'SIZE 1 1 1\nBLOCKPROP 0 0 0 mode subtract\n',
        ):
            with self.subTest(text=text), self.assertRaises(ValueError):
                self.parse_text(text)

    def test_invalid_output_port_ordinals_and_bindings_are_rejected(self):
        base = 'SIZE 1 1 1\nINPUT a 0 0 0\nOUTPUT y 0 0 0\n'
        for tail in (
            'OUTPUTPORT 1 y a 0 0 0\n',
            'OUTPUTPORT 0 y missing 0 0 0\n',
            'OUTPUTPORT 0 y a 0 0 0\nOUTPUTPORT 0 y a 0 0 0\n',
        ):
            with self.subTest(tail=tail), self.assertRaises(ValueError):
                self.parse_text(base + tail)

    def test_rewritten_gate_observation_mismatch_is_rejected(self):
        base = ('SIZE 2 1 1\nINPUT a 0 0 0\nOUTPUT y 1 0 0\n'
                'BLOCK 1 0 0 Solid none none false 1 minecraft:stone\n'
                'OUTPUTPORT 0 y n 1 0 0\n')
        for tail in (
            'GATE n a buf\nGATEOUT n 1 0 0 Repeater\n',
            'GATE n a buf\nGATEOUT missing 1 0 0 Repeater\n',
            'GATE n missing buf\nGATEOUT n 1 0 0 Solid\n',
        ):
            with self.subTest(tail=tail), self.assertRaises(ValueError):
                self.parse_text(base + tail)

    def test_legacy_nor_merge_dump_still_evaluates_and_reads(self):
        dump = self.parse_text('SIZE 2 1 1\nINPUT a 0 0 0\nGATE n a nor\nGATE y n,a merge\nGATEOUT n 0 0 0\nGATEOUT y 1 0 0\nOUTPUT y 1 0 0\n')
        for a in (0, 1):
            values = evaluate_expected(dump.gates, {'a': a}, dump.output_bindings)
            self.assertEqual(values['n'], 1-a)
            self.assertEqual(values['y'], 1)
        self.assertEqual(expected_block_at(dump.gates[0]), 'minecraft:redstone_wall_torch')
        self.assertEqual(expected_block_at(dump.gates[1]), 'minecraft:redstone_wire')

    def test_actual_permuted_boundary_dump(self):
        directory = os.environ.get('REDA_DUMP_FIXTURE_DIR')
        if not directory:
            self.skipTest('generate mc_dump test fixtures')
        dump = parse_dump(Path(directory) / 'permuted.txt')
        for address in range(4):
            rhs,lhs = address & 1, (address >> 1) & 1
            values = evaluate_expected(dump.gates, {'rhs':rhs, 'lhs':lhs}, dump.output_bindings)
            self.assertEqual([values[key] for key in dump.outputs], [rhs, rhs or lhs, lhs])

    def test_actual_rust_rewrite_dumps_preserve_truth_ports_and_components(self):
        directory = os.environ.get('REDA_DUMP_FIXTURE_DIR')
        if not directory:
            self.skipTest('generate mc_dump test fixtures and set REDA_DUMP_FIXTURE_DIR')
        expected = [
            lambda a,b: [not a, not b, not a or not b],
            lambda a,b: [a, a, b],
            lambda a,b: [not a]*3,
            lambda a,b: [a and not b, not a, not b],
            lambda a,b: [1, 0, 1],
        ]
        seen_components, seen_gate_kinds = set(), set()
        for index, truth in enumerate(expected):
            dump = parse_dump(Path(directory) / f'rewrite-{index}.txt')
            self.assertEqual(len(dump.outputs), 3)
            self.assertEqual([dump.output_display_name(key) for key in dump.outputs], ['public0','public1','public2'])
            self.assertEqual(len(set(dump.outputs.values())), 3)
            self.assertFalse(any(g.name == 'removed_internal' for g in dump.gates))
            blocks = {(b.x,b.y,b.z): b for b in dump.blocks}
            commands = build_phase_commands(dump.blocks, (0,0,0))
            for gate in dump.gates:
                seen_gate_kinds.add(gate.kind)
                seen_components.add(gate.observation_kind)
                block = blocks[dump.gate_outputs[gate.name]]
                self.assertEqual(gate.observation_kind, block.kind)
                self.assertEqual(expected_block_at(gate), block.name)
                if block.kind == 'Comparator':
                    self.assertEqual(block.properties['mode'], 'subtract')
                    self.assertIn('mode=subtract', block_state_string(block))
                    self.assertTrue(any(block.name in c and 'mode=subtract' in c for c in commands))
            for address in range(4):
                a,b = address & 1, (address >> 1) & 1
                values = evaluate_expected(dump.gates, {'left':a, 'right':b}, dump.output_bindings)
                self.assertEqual([values[key] for key in dump.outputs], list(map(int,truth(a,b))))
                states = {}
                for gate in dump.gates:
                    pos = dump.gate_outputs[gate.name]
                    prop = ('power' if gate.observation_kind == 'RedstoneWire' else 'powered' if gate.observation_kind in ('Repeater','Comparator') else 'lit')
                    raw = '0' if prop == 'power' and not values[gate.name] else '1' if prop == 'power' else str(bool(values[gate.name])).lower()
                    states[(*pos, expected_block_at(gate), prop)] = raw
                for key,pos in dump.outputs.items():
                    states[(*pos, 'minecraft:redstone_lamp', 'lit')] = str(bool(values[key])).lower()
                class Client:
                    def command(self, command):
                        parts = command.split()
                        x,y,z = map(int,parts[3:6])
                        block_id, _, condition = parts[6].partition('[')
                        if not condition:
                            return 'Test passed.' if any(k[:4] == (x,y,z,block_id) for k in states) else 'Test failed.'
                        prop,raw = condition.rstrip(']').split('=')
                        return 'Test passed.' if states.get((x,y,z,block_id,prop)) == raw else 'Test failed.'
                lamps,gates = read_state(Client(), dump, (0,0,0))
                self.assertEqual(lamps, {key:values[key] for key in dump.outputs})
                self.assertEqual(gates, {g.name:values[g.name] for g in dump.gates})
        self.assertTrue({'Comparator','Repeater','WallTorch','RedstoneWire'} <= seen_components)
        self.assertTrue({'nor','merge','buf','andnot'} <= seen_gate_kinds)


if __name__ == '__main__':
    unittest.main()

"""Offline coverage for exported physical blocks and fail-before-mutation."""
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import circuit_conformance as circuit


class PlacementCommandsTest(unittest.TestCase):
    def test_all_supported_static_exports_are_placed_with_actual_identity(self):
        kinds = ["Solid", "Glass", "Lamp", "WallTorch", "Torch", "Repeater", "Comparator", "RedstoneWire"]
        names = ["stone", "tinted_glass", "redstone_lamp", "redstone_wall_torch", "redstone_torch", "repeater", "comparator", "redstone_wire"]
        blocks = [circuit.Block(i * 2, 0, 0, kind, "east", "floor", False, 1, "minecraft:" + name)
                  for i, (kind, name) in enumerate(zip(kinds, names))]
        commands = circuit.build_phase_commands(blocks, (10, 20, 30))
        self.assertEqual(len(commands), len(blocks))
        for block in blocks:
            prefix = f"setblock {10 + block.x} 20 30 {block.name}"
            self.assertEqual(sum(command.startswith(prefix) for command in commands), 1)
        self.assertLess(next(i for i, c in enumerate(commands) if "tinted_glass" in c),
                        next(i for i, c in enumerate(commands) if "repeater" in c))

    def test_original_command_sequence_and_lever_skip_are_preserved(self):
        kinds = ["Solid", "Lamp", "WallTorch", "Torch", "Repeater", "Comparator", "RedstoneWire"]
        names = ["stone", "redstone_lamp", "redstone_wall_torch", "redstone_torch", "repeater", "comparator", "redstone_wire"]
        states = ["minecraft:stone", "minecraft:redstone_lamp[lit=false]",
                  "minecraft:redstone_wall_torch[facing=east,lit=true]", "minecraft:redstone_torch[lit=true]",
                  "minecraft:repeater[facing=east,delay=1,locked=false,powered=false]",
                  "minecraft:comparator[facing=east,mode=compare,powered=false]", "minecraft:redstone_wire"]
        blocks = [circuit.Block(i * 2, 0, 0, kind, "east", "floor", False, 1, "minecraft:" + name)
                  for i, (kind, name) in enumerate(zip(kinds, names))]
        blocks.append(circuit.Block(30, 0, 0, "Lever", "east", "floor", False, 1, "minecraft:lever"))
        self.assertEqual(circuit.build_phase_commands(list(reversed(blocks)), (0, 0, 0)),
                         [f"setblock {i * 2} 0 0 {state} replace" for i, state in enumerate(states)])

    def test_unknown_export_rejects_before_rcon_or_world_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "unknown.dump"
            path.write_text("SIZE 1 1 1\nBLOCK 0 0 0 Unknown none none false 1 minecraft:stone\n")
            with patch.object(circuit, "RconClient") as client, self.assertRaisesRegex(ValueError, "Unknown"):
                circuit.run(SimpleNamespace(dump=path))
            client.assert_not_called()
        unknown = circuit.Block(0, 0, 0, "Unknown", "none", "none", False, 1, "minecraft:stone")
        with self.assertRaisesRegex(ValueError, "Unknown"):
            circuit.build_phase_commands([unknown], (0, 0, 0))


if __name__ == "__main__":
    unittest.main()

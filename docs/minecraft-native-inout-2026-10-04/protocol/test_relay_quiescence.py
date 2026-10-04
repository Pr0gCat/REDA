"""Hidden route progress must prevent premature quiescence."""
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch
import circuit_conformance as circuit


class RelayQuiescenceTest(unittest.TestCase):
    def poll(self, relay_states, ceiling=10):
        clock = [0.0]
        def sleep(seconds):
            clock[0] += seconds
        observed = ({"y": 0}, {"n": 1})
        with patch.object(circuit, "read_state", return_value=observed), \
             patch.object(circuit, "read_relay_state", side_effect=relay_states) as relay, \
             patch.object(circuit.time, "monotonic", side_effect=lambda: clock[0]), \
             patch.object(circuit.time, "sleep", side_effect=sleep):
            result = circuit.poll_until_quiescent(None, None, (0,0,0), 0, ceiling, .25, "test")
            return result, relay.call_count

    def test_hidden_relay_change_resets_streak_with_stable_gates(self):
        low = {(0,0,0,"powered"): False}
        high = {(0,0,0,"powered"): True}
        result, count = self.poll([low] * 4 + [high] * 5)
        self.assertEqual(count, 9)
        self.assertEqual(result, ({"y": 0}, {"n": 1}))

    def test_slow_final_relay_scan_cannot_pass_after_original_ceiling(self):
        clock = [0.0]
        calls = [0]
        def relay_read(*args):
            calls[0] += 1
            clock[0] += .2
            return {(0,0,0,"powered"): False}
        def sleep(seconds):
            clock[0] += seconds
        with patch.object(circuit, "read_state", return_value=({}, {})), \
             patch.object(circuit, "read_relay_state", side_effect=relay_read), \
             patch.object(circuit.time, "monotonic", side_effect=lambda: clock[0]), \
             patch.object(circuit.time, "sleep", side_effect=sleep):
            with self.assertRaises(circuit.CircuitMismatch):
                circuit.poll_until_quiescent(None, None, (0,0,0), 0, 1.9, .25, "slow read")
        self.assertEqual(calls[0], 5)
        self.assertAlmostEqual(clock[0], 2.0)

    def test_oscillating_relay_keeps_original_ceiling_failure(self):
        with self.assertRaises(circuit.CircuitMismatch):
            self.poll([{0: i % 2} for i in range(20)], ceiling=1)

    def test_actual_repeater_and_comparator_properties_and_missing_relay(self):
        blocks = [circuit.Block(1,2,3,"Repeater","east","none",False,1,"minecraft:repeater"),
                  circuit.Block(4,2,3,"Comparator","east","none",False,1,"minecraft:comparator")]
        client = Mock()
        client.command.side_effect = lambda command: "Test passed" if "=false]" in command else "Test failed"
        states = circuit.read_relay_state(client, SimpleNamespace(blocks=blocks), (10,20,30))
        self.assertEqual(states, {(1,2,3,"powered"):False, (1,2,3,"locked"):False, (4,2,3,"powered"):False})
        self.assertTrue(any("11 22 33 minecraft:repeater[locked=false]" in call.args[0]
                            for call in client.command.call_args_list))
        client.command.return_value = "Test failed"
        client.command.side_effect = None
        with self.assertRaises(circuit.RegionNotReady):
            circuit.read_relay_state(client, SimpleNamespace(blocks=blocks), (0,0,0))


if __name__ == "__main__":
    unittest.main()

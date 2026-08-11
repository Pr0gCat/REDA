import unittest
from pathlib import Path


CONFORMANCE_DIR = Path(__file__).parent


class UsageExamplesTest(unittest.TestCase):
    def test_circuit_conformance_uses_current_26_2_server_example(self):
        source = (CONFORMANCE_DIR / "circuit_conformance.py").read_text(encoding="utf-8")

        self.assertIn("--properties ../minecraft-server-26.2/server/server.properties", source)
        self.assertIn("--out results/and4_26.2.json --label 26.2", source)
        self.assertNotIn("--properties ../minecraft-server/server/server.properties", source)


if __name__ == "__main__":
    unittest.main()

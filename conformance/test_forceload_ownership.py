"""Primitive cleanup must leave an unrelated circuit's loaded chunks intact."""
import unittest

from harness import forceload_region, forceload_release


class LoadedChunks:
    def __init__(self):
        self.chunks = {(0, 0), (9375, 6250)}

    def command(self, command):
        parts = command.split()
        if parts == ["forceload", "remove", "all"]:
            self.chunks.clear()
            return
        _, operation, x0, z0, x1, z1 = parts
        x0, z0, x1, z1 = map(int, (x0, z0, x1, z1))
        chunks = {
            (x, z)
            for x in range(x0 // 16, x1 // 16 + 1)
            for z in range(z0 // 16, z1 // 16 + 1)
        }
        if operation == "add":
            self.chunks.update(chunks)
        elif operation == "remove":
            self.chunks.difference_update(chunks)
        else:
            raise AssertionError(command)


class ForceloadOwnershipTest(unittest.TestCase):
    def test_cleanup_preserves_unrelated_chunks(self):
        for slots in (1, 32):
            with self.subTest(slots=slots):
                client = LoadedChunks()
                unrelated = client.chunks.copy()
                forceload_region(client, slots)
                self.assertGreater(len(client.chunks), len(unrelated))
                forceload_release(client, slots)
                self.assertEqual(client.chunks, unrelated)


if __name__ == "__main__":
    unittest.main()

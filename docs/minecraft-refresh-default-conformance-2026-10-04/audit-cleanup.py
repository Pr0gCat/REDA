"""Verify the cleared game region with real queries, releasing own tickets."""
from pathlib import Path
import gzip
import json
import sys

root = Path(__file__).resolve().parent
sys.path.insert(0, str(root / "protocol"))
from circuit_conformance import read_properties, wait_for_region_loaded
from rcon import RconClient

origin = (190000, 151, 160000)
size = (106, 5, 49)
world = json.loads(gzip.decompress((root / "candidate-world.json.gz").read_bytes()))
positions = {tuple(origin[axis] + at[axis] for axis in range(3)) for at, _ in world["cells"]}
positions.update((origin[0] + x, origin[1] + y, origin[2] + z)
                 for x in (-3, size[0] + 3)
                 for y in (-3, size[1] + 3)
                 for z in (-3, size[2] + 3))
chunks = [(x * 16, z * 16)
          for x in range((origin[0] - 8) // 16, (origin[0] + size[0] + 8) // 16 + 1)
          for z in range((origin[2] - 8) // 16, (origin[2] + size[2] + 8) // 16 + 1)]
props = read_properties(Path(sys.argv[1]))
responses = []


class LoggedRconClient(RconClient):
    def command(self, command):
        response = super().command(command)
        responses.append({"command": command, "response": response})
        return response


result = {"origin": origin, "size": size, "original_occupied_sites": 1094,
          "air_sites_checked": len(positions), "chunks": len(chunks),
          "scope": "all original occupied positions and eight bounding samples; not exhaustive bounding volume",
          "responses": responses, "accepted": False}
with LoggedRconClient("127.0.0.1", int(props["rcon.port"]), props["rcon.password"]) as client:
    owned = False
    try:
        for x, z in chunks:
            assert "not marked" in client.command(f"forceload query {x} {z}")
        response = client.command("forceload add 189992 159992 190114 160057")
        assert response.startswith("Marked 45 chunks"), response
        owned = True
        wait_for_region_loaded(client, origin, size)
        for x, y, z in sorted(positions):
            assert client.command(f"execute if block {x} {y} {z} minecraft:air") == "Test passed", (x, y, z)
        result["all_original_occupied_sites_air"] = True
    finally:
        if owned:
            result["release_acknowledgement"] = client.command("forceload remove 189992 159992 190114 160057")
        for x, z in chunks:
            assert "not marked" in client.command(f"forceload query {x} {z}")
        result["all_test_region_chunks_unforced_after_audit"] = True
        (root / "cleanup-audit.json").write_text(json.dumps(result, indent=2) + "\n")
result["accepted"] = True
(root / "cleanup-audit.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps({key: value for key, value in result.items() if key != "responses"}))

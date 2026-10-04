"""Run the existing harness with lossless logging of real RCON replies."""
from pathlib import Path
import json
import sys
import time

root = Path(__file__).resolve().parent
sys.path.insert(0, "/home/seith/REDA/target/qualification/shared-inout-game-producer/final-protocol")
import circuit_conformance as driver
from rcon import RconClient

trace = (root / "final-rcon.jsonl").open("w")


class LoggedRconClient(RconClient):
    def command(self, command):
        response = super().command(command)
        trace.write(json.dumps({"monotonic_seconds": time.monotonic(), "command": command, "response": response}) + "\n")
        trace.flush()
        return response


# Additional physical observations do not participate in or weaken the
# unchanged gate/lamp settling conditions.
import json as observation_json
pinout = observation_json.loads((root / "expected-trace.json").read_text())["pinout"]
trace_metadata = observation_json.loads((root / "expected-trace.json").read_text())
read_sites = sorted({tuple(p) for bus in pinout["inouts"] for p in bus["channel"] + [bus["readback"], bus["external_pad"]]} | {tuple(item["position"]) for item in trace_metadata["physical_output_inventory"] if item["kind"] == "RedstoneWire"})
assert len(trace_metadata["physical_output_inventory"]) == len(driver.parse_dump(root / "candidate.dump").outputs) + len(trace_metadata["internal_injections"])
assert {tuple(item["position"]) for item in trace_metadata["internal_injections"]} <= set(read_sites)
original_read_state = driver.read_state
def complete_read_state(client, dump, origin):
    result = original_read_state(client, dump, origin)
    for point in read_sites:
        absolute = tuple(a+b for a,b in zip(origin, point))
        driver.read_merge_junction(client, absolute)
    return result
driver.read_state = complete_read_state

driver.RconClient = LoggedRconClient
try:
    raise SystemExit(driver.main())
finally:
    trace.close()

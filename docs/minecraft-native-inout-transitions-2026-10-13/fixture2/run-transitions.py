"""M1+M2 driver: drive all 4096 declared-state transitions (Eulerian walk)
through the real game, with lossless RCON logging. Every poll's full-signal
read (lamps+gates+relays) is already recorded by poll_until_quiescent, so the
same run yields both the transition results (M1) and the waveform traces (M2).

Reuses the frozen final protocol unchanged except: the vector schedule is the
4097-vertex Eulerian walk from transition-schedule.txt, and the extra channel/
readback/external-pad reads are added exactly as run-final.py does (they do not
participate in or weaken the settling condition)."""
from pathlib import Path
import json
import sys
import time

root = Path(__file__).resolve().parent
sys.path.insert(0, "/home/seith/REDA/target/qualification/shared-inout-game-producer/final-protocol")
import circuit_conformance as driver
from rcon import RconClient

trace = (root / "transition-rcon.jsonl").open("w")


class LoggedRconClient(RconClient):
    def command(self, command):
        response = super().command(command)
        trace.write(json.dumps({"monotonic_seconds": time.monotonic(), "command": command, "response": response}) + "\n")
        trace.flush()
        return response


# Extra physical observations, identical to run-final.py: channel/readback/
# external-pad reads that do not change the gate/lamp settling condition.
import json as observation_json
pinout = observation_json.loads((root / "expected-trace.json").read_text())["pinout"]
trace_metadata = observation_json.loads((root / "expected-trace.json").read_text())
read_sites = sorted({tuple(p) for bus in pinout["inouts"] for p in bus["channel"] + [bus["readback"], bus["external_pad"]]} | {tuple(item["position"]) for item in trace_metadata["physical_output_inventory"] if item["kind"] == "RedstoneWire"})
original_read_state = driver.read_state
def complete_read_state(client, dump, origin):
    result = original_read_state(client, dump, origin)
    for point in read_sites:
        absolute = tuple(a+b for a,b in zip(origin, point))
        driver.read_merge_junction(client, absolute)
    return result
driver.read_state = complete_read_state

# A transient world-readiness flake (a repeater read matching neither powered
# state because its chunk is momentarily not loaded) aborts a multi-hour run.
# Re-forceload and re-read the same poll up to a bounded number of times before
# giving up. This re-reads the world; it does NOT change the settling condition
# (still REQUIRED_STABLE_POLLS agreeing full-signal polls) or any physical rule.
original_read_relay_state = driver.read_relay_state
def resilient_read_relay_state(client, dump, origin):
    last = None
    for attempt in range(4):
        try:
            return original_read_relay_state(client, dump, origin)
        except driver.RegionNotReady as error:
            last = error
            driver.forceload(client, origin, dump.size)
            time.sleep(0.5)
    raise last
driver.read_relay_state = resilient_read_relay_state

# The transition schedule: 4097 seven-bit vectors covering all 4096 transitions.
import os
schedule = (root / "transition-schedule.txt").read_text().split()
smoke = os.environ.get("SMOKE_LIMIT")
if smoke:
    schedule = schedule[:int(smoke)]
    assert len(schedule) >= 2, len(schedule)
else:
    assert len(schedule) == 4097, len(schedule)

driver.RconClient = LoggedRconClient
sys.argv = [
    "circuit_conformance",
    "--dump", str(root / "candidate.dump"),
    "--properties", "/home/seith/REDA/minecraft-server-26.2/server/server.properties",
    "--out", str(root / "transition-result.json"),
    "--label", "all-4096-transitions-native-inout",
    "--origin", "231000,151,180000",
    "--vectors", ",".join(schedule),
]
try:
    raise SystemExit(driver.main())
finally:
    trace.close()

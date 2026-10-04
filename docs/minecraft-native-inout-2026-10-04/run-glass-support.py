"""Run the existing harness with lossless logging of real RCON replies."""
from pathlib import Path
import json
import sys
import time

root = Path(__file__).resolve().parent
sys.path.insert(0, "/home/seith/REDA/docs/minecraft-native-parameter-hierarchy-2026-10-04/protocol")
import circuit_conformance as driver
from rcon import RconClient

trace = (root / "glass-support-rcon.jsonl").open("w")


class LoggedRconClient(RconClient):
    def command(self, command):
        response = super().command(command)
        trace.write(json.dumps({"monotonic_seconds": time.monotonic(), "command": command, "response": response}) + "\n")
        trace.flush()
        return response


# Extend placement for the exported transparent support; retain original physics,
# ordering, observations, settling and assertions.
original_block_state_string = driver.block_state_string
def supported_block_state_string(block):
    if block.kind == "Glass":
        if block.name != "minecraft:glass":
            raise ValueError("unexpected Glass identity")
        return block.name
    return original_block_state_string(block)
driver.block_state_string = supported_block_state_string
driver.CATEGORY_ORDER.insert(0, "Glass")
original_build_phase_commands = driver.build_phase_commands
def complete_build_phase_commands(blocks, origin):
    unknown = {block.kind for block in blocks} - set(driver.CATEGORY_ORDER)
    if unknown:
        raise ValueError(f"unplaced exported block kinds: {sorted(unknown)}")
    return original_build_phase_commands(blocks, origin)
driver.build_phase_commands = complete_build_phase_commands

driver.RconClient = LoggedRconClient
try:
    raise SystemExit(driver.main())
finally:
    trace.close()

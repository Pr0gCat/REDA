"""Run the existing harness with lossless logging of real RCON replies."""
from pathlib import Path
import json
import sys
import time

root = Path(__file__).resolve().parent
sys.path.insert(0, "/home/seith/REDA/docs/minecraft-native-parameter-hierarchy-2026-10-04/protocol")
import circuit_conformance as driver
from rcon import RconClient

trace = (root / "targeted-rcon.jsonl").open("w")


class LoggedRconClient(RconClient):
    def command(self, command):
        response = super().command(command)
        trace.write(json.dumps({"monotonic_seconds": time.monotonic(), "command": command, "response": response}) + "\n")
        trace.flush()
        return response


driver.RconClient = LoggedRconClient
try:
    raise SystemExit(driver.main())
finally:
    trace.close()

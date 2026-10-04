"""Run the existing harness with lossless logging of real RCON replies."""
from pathlib import Path
import json
import sys
import time

root = Path(__file__).resolve().parent
sys.path.insert(0, "/home/seith/REDA/target/qualification/shared-inout-game-producer/reader-transition-protocol")
import circuit_conformance as driver
from rcon import RconClient

trace = (root / "reader-wave-rcon.jsonl").open("w")


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
    properties = driver.read_properties(root.parents[2] / "minecraft-server-26.2/server/server.properties")
    dump = driver.parse_dump(root / "candidate.dump")
    relay_points = [(9,1,11),(10,1,25),(11,1,39),(26,1,39),(41,1,39),(56,1,39),(71,1,39)]
    samples = []
    with LoggedRconClient("127.0.0.1", int(properties["rcon.port"]), properties["rcon.password"]) as client:
        try:
            for _ in range(16):
                relay = [driver.read_powered_block(client,(230000+x,151+y,180000+z),"minecraft:repeater") for x,y,z in relay_points]
                torch = driver.read_torch(client,(230080,152,180041))
                lamp = driver.read_lamp(client,(230119,151,180055))
                samples.append({"time":time.monotonic(),"relay":relay,"n11":torch,"y":lamp})
                time.sleep(0.1)
        finally:
            driver.clear_region(client,(230000,151,180000),dump.size)
            driver.forceload_release(client,(230000,151,180000),dump.size)
    (root / "reader-wave-observations.json").write_text(json.dumps({"relay_points":relay_points,"samples":samples},indent=2)+"\n")
    trace.close()

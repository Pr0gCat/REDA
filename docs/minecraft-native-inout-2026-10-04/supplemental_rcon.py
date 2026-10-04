"""Prepared source only. Explicit read/cleanup modes; never builds or changes inputs."""
import argparse
import json
from pathlib import Path
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(ROOT/'docs/minecraft-native-parameter-hierarchy-2026-10-04/protocol'))
import circuit_conformance as h
from rcon import RconClient

p = argparse.ArgumentParser()
p.add_argument('--properties', type=Path, required=True)
p.add_argument('--host', default='127.0.0.1')
p.add_argument('--mode', choices=['read','cleanup'], required=True)
p.add_argument('--raw-state', type=int)
p.add_argument('--out', type=Path, default=HERE/'cleanup-audit.json')
p.add_argument('--raw-file', type=Path)
a = p.parse_args()
if a.mode == 'cleanup':
    assert not a.out.exists(), 'preserve prior cleanup evidence; select a new --out path'
props = {}
for line in a.properties.read_text().splitlines():
    if line and not line.startswith('#') and '=' in line:
        k,v = line.split('=',1); props[k] = v
d = h.parse_dump(HERE/'candidate.dump')
t = json.loads((HERE/'expected-trace.json').read_text())
origin = (230000,151,180000)
absolute = lambda pos: tuple(x+y for x,y in zip(origin,pos))
responses = []
class Logged(RconClient):
    def command(self, command):
        response = super().command(command)
        entry = {'monotonic_seconds':time.monotonic(),'command':command,'response':response}
        responses.append(entry)
        raw_file = a.raw_file or HERE/('supplemental-rcon.jsonl' if a.mode=='read' else 'cleanup-rcon.jsonl')
        with raw_file.open('a') as f:
            f.write(json.dumps(entry)+'\n')
        return response
c = Logged(a.host,int(props.get('rcon.port','25575')),props['rcon.password'])
c.connect()
try:
    if a.mode == 'read':
        assert a.raw_state is not None and 0 <= a.raw_state < 128
        # Verify the supplied physical state before any supplemental read.
        for bit,n in enumerate(t['control_order']):
            x,y,z = absolute(d.inputs[n])
            value = 'true' if a.raw_state & (1<<bit) else 'false'
            assert c.command(f'execute if block {x} {y} {z} minecraft:lever[powered={value}]').strip() == 'Test passed'
        bus = t['pinout']['inouts'][0]
        positions = bus['channel']+[bus['readback'],bus['external_pad']]
        positions += [item['position'] for item in t['internal_injections']]
        for pos in positions:
            x,y,z = absolute(pos)
            zero = c.command(f'execute if block {x} {y} {z} minecraft:redstone_wire[power=0]')
            if not zero.strip().startswith('Test passed'):
                assert c.command(f'execute if block {x} {y} {z} minecraft:redstone_wire').strip() == 'Test passed'
    else:
        positions = {absolute((b.x,b.y,b.z)) for b in d.blocks}
        sx,_,sz = d.size
        chunks = {(x,z) for x in range((origin[0]-8)//16,(origin[0]+sx+8)//16+1)
                         for z in range((origin[2]-8)//16,(origin[2]+sz+8)//16+1)}
        failures, checked = [], 0
        try:
            h.forceload(c,origin,d.size)
            h.wait_for_region_loaded(c,origin,d.size,timeout=15)
            for x,y,z in sorted(positions):
                response = c.command(f'execute if block {x} {y} {z} minecraft:air')
                checked += 1
                if response.strip() != 'Test passed': failures.append({'position':[x,y,z],'response':response})
        except Exception as error:
            failures.append({'stage':'air_audit','error':str(error)})
        finally:
            try:
                h.forceload_release(c,origin,d.size)
            except Exception as error:
                failures.append({'stage':'release','error':str(error)})
        queries_ok = True
        for x,z in sorted(chunks):
            try:
                if 'not marked' not in c.command(f'forceload query {x*16} {z*16}'): queries_ok = False
            except Exception as error:
                queries_ok = False
                failures.append({'stage':'query','error':str(error)})
        air_ok = checked == len(positions) and not failures
        result = {'accepted':air_ok and queries_ok,'all_original_occupied_sites_air':air_ok,
                  'all_test_region_chunks_unforced_after_audit':queries_ok,
                  'air_sites_checked':checked,'region_chunks':len(chunks),'failures':failures,'responses':responses}
        a.out.write_text(json.dumps(result,indent=2)+'\n')
        assert result['accepted'], 'cleanup audit failed; evidence preserved'
finally:
    c.close()

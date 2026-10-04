from pathlib import Path
import sys,json
sys.path.insert(0,'docs/minecraft-native-parameter-hierarchy-2026-10-04/protocol')
import circuit_conformance as d
from rcon import RconClient
root=Path('target/qualification/shared-inout-game-producer');p=d.read_properties(Path('minecraft-server-26.2/server/server.properties'))
points=[(6,1,11,'minecraft:redstone_wire','power'),(7,1,11,'minecraft:repeater','powered'),(8,1,11,'minecraft:redstone_wire','power')]
for x,y,z in [(9,1,11),(10,1,25),(11,1,39),(26,1,39),(41,1,39),(56,1,39),(71,1,39)]:
 points.extend([(x,y,z,'minecraft:repeater','powered'),(x,y,z,'minecraft:repeater','locked')])
points += [(79,1,40,'minecraft:redstone_wire','power'),(79,1,41,'minecraft:stone',None),(80,1,41,'minecraft:redstone_wall_torch','lit')]
for direction in ['north','south','east','west']:
 points.append((79,1,40,'minecraft:redstone_wire',direction))

records=[]
with RconClient('127.0.0.1',int(p['rcon.port']),p['rcon.password']) as c:
 try:
  for x,y,z,b,prop in points:
   values=range(16) if prop=='power' else ['none','side','up'] if prop in ['north','south','east','west'] else ['true','false'] if prop else [None]
   for v in values:
    state=b+(f'[{prop}={v}]' if prop else '')
    cmd=f'execute if block {230000+x} {151+y} {180000+z} {state}';reply=c.command(cmd)
    records.append({'position':[x,y,z],'state':state,'command':cmd,'response':reply})
    if reply.startswith('Test passed'): print(x,y,z,state)
 finally:
  d.clear_region(c,(230000,151,180000),(170,7,99));d.forceload_release(c,(230000,151,180000),(170,7,99))
(root/'reader-route-probe.json').write_text(json.dumps(records,indent=2)+'\n')

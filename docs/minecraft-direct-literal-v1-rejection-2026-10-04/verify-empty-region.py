"""Verify every cell the unchanged game harness may clear, without block writes."""
import json,sys
from pathlib import Path
root=Path(__file__).resolve().parent
sys.path.insert(0,str(root/'protocol'))
from circuit_conformance import parse_dump,read_properties,forceload,forceload_release,wait_for_region_loaded
from rcon import RconClient
origin=(220000,151,180000)
dump=parse_dump(root/'candidate.dump');size=dump.size
lo=tuple(value-3 for value in origin);hi=tuple(value+extent+3 for value,extent in zip(origin,size))
props=read_properties(Path(sys.argv[1]))
responses=[];accepted=False
class LoggedClient(RconClient):
 def command(self,command):
  response=super().command(command)
  responses.append({'command':command,'response':response})
  return response
try:
 with LoggedClient('127.0.0.1',int(props['rcon.port']),props['rcon.password']) as client:
  try:
   forceload(client,origin,size)
   wait_for_region_loaded(client,origin,size)
   for x in range(lo[0],hi[0]+1):
    for z in range(lo[2],hi[2]+1):
     command='execute '+' '.join(f'if block {x} {y} {z} minecraft:air' for y in range(lo[1],hi[1]+1))
     response=client.command(command)
     assert response.strip().startswith('Test passed'),(x,z,response)
   accepted=True
  finally:
   forceload_release(client,origin,size)
finally:
 (root/'full-empty-preflight.json').write_text(json.dumps({'origin':origin,'size':size,'lo':lo,'hi':hi,'cells_checked':(hi[0]-lo[0]+1)*(hi[1]-lo[1]+1)*(hi[2]-lo[2]+1),'accepted':accepted,'responses':responses},indent=2)+'\n')
print('Every cell in exact harness clear region is air:',accepted)

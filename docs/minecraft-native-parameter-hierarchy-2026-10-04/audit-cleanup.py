from pathlib import Path
import json,sys
root=Path(__file__).resolve().parent
sys.path.insert(0,str(root/'protocol'))
from circuit_conformance import parse_dump,read_properties,wait_for_region_loaded,forceload,forceload_release
from rcon import RconClient
origin=(210000,151,180000);dump=parse_dump(root/'candidate.dump');ox,oy,oz=origin;sx,sy,sz=dump.size
chunks=[(x*16,z*16) for x in range((ox-8)//16,(ox+sx+8)//16+1) for z in range((oz-8)//16,(oz+sz+8)//16+1)]
positions={(ox+b.x,oy+b.y,oz+b.z) for b in dump.blocks}
positions.update((ox+x,oy+y,oz+z) for x in (-3,sx+3) for y in (-3,sy+3) for z in (-3,sz+3))
props=read_properties(Path(sys.argv[1]));responses=[]
class LoggedRconClient(RconClient):
 def command(self,command):
  response=super().command(command);responses.append({'command':command,'response':response});return response
result={'origin':origin,'size':dump.size,'original_occupied_sites':len(dump.blocks),'air_sites_checked':len(positions),'chunks':len(chunks),'responses':responses,'accepted':False}
with LoggedRconClient('127.0.0.1',int(props['rcon.port']),props['rcon.password']) as client:
 owned=False
 try:
  for x,z in chunks: assert 'not marked' in client.command(f'forceload query {x} {z}')
  forceload(client,origin,dump.size);owned=True
  wait_for_region_loaded(client,origin,dump.size)
  for x,y,z in sorted(positions):assert client.command(f'execute if block {x} {y} {z} minecraft:air')=='Test passed',(x,y,z)
  result['all_original_occupied_sites_air']=True
 finally:
  if owned: forceload_release(client,origin,dump.size)
  for x,z in chunks:assert 'not marked' in client.command(f'forceload query {x} {z}')
  result['all_test_region_chunks_unforced_after_audit']=True
  (root/'cleanup-audit.json').write_text(json.dumps(result,indent=2)+'\n')
result['accepted']=True
(root/'cleanup-audit.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({k:v for k,v in result.items() if k!='responses'}))

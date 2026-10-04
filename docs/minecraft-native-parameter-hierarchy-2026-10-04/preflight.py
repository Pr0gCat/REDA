from pathlib import Path
import json,sys
root=Path(__file__).resolve().parent
sys.path.insert(0,str(root/'protocol'))
from circuit_conformance import parse_dump,read_properties
from rcon import RconClient
origin=(210000,151,180000)
dump=parse_dump(root/'candidate.dump')
ox,oy,oz=origin;sx,sy,sz=dump.size
chunks=[(x*16,z*16) for x in range((ox-8)//16,(ox+sx+8)//16+1) for z in range((oz-8)//16,(oz+sz+8)//16+1)]
props=read_properties(Path(sys.argv[1]));responses=[]
with RconClient('127.0.0.1',int(props['rcon.port']),props['rcon.password']) as client:
 for x,z in chunks:
  cmd=f'forceload query {x} {z}';response=client.command(cmd);responses.append({'command':cmd,'response':response});assert 'not marked' in response,response
(root/'preflight.json').write_text(json.dumps({'origin':origin,'size':dump.size,'chunks':chunks,'all_region_chunks_previously_unforced':True,'responses':responses},indent=2)+'\n')
print('Real RCON preflight: region chunks unforced.')

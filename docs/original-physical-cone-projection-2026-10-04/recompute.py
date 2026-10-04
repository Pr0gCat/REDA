import json,gzip,collections,sys,copy,hashlib
from pathlib import Path
r=Path(sys.argv[1]);report=json.loads((r/'identity-result.json').read_text());root=json.loads((r/'root-manifest.json').read_text());source=json.loads((r/'source-netlist.json').read_text());sections=['placements','boundaries','output_boundary_aliases','connections','routes','repeater_handovers','junctions','observations'];union={};all_records={k:[] for k in sections};gate_checks=[]
def frozen_world(p):
 with gzip.open(p,'rt') as f:w=json.load(f)
 return tuple(w['size']),{tuple(at):state for at,state in w['cells']},w['fingerprint']
size,original,original_hash=frozen_world(r/'original-whole-world.json.gz');ss,sealed,sealed_hash=frozen_world(r/'root-world.json.gz');assert size==ss and original==sealed and original_hash==sealed_hash
for proof in report['leaves']:
 i=proof['ordinal'];c=json.loads((r/f'leaf{i}-manifest.json').read_text());leaf=json.loads((r/f'leaf{i}-source-netlist.json').read_text());origin=proof['origin'];ins={b:a for a,b in proof['mapping_instances']};inp={b:a for a,b in proof['mapping_inputs']};out={b:a for a,b in proof['mapping_outputs']};routes={b:a for a,b in proof['mapping_routes']}
 def at(p):return {k:p[k]+origin[k] for k in 'xyz'}
 def primitive(p):return {'instance':ins[p['instance']],'node':p['node']}
 def connection(p):
  key=next(iter(p));v=dict(p[key]);v['instance']=ins[v['instance']];return {key:v}
 def endpoint(p):
  key=next(iter(p));v=p[key]
  return {key:inp[v] if key=='PrimaryInput' else out[v] if key=='DeclaredOutput' else primitive(v) if key=='PrimitiveOutput' else connection(v) if key=='Landing' else ins[v]}
 def obs(p):
  key=next(iter(p));v=p[key]
  return {key:inp[v] if key=='PrimaryInput' else out[v] if key=='DeclaredOutput' else primitive(v) if key=='PrimitiveOutput' else ins[v]}
 def owner(p):
  if p is None:return None
  key=next(iter(p));v=p[key];return {key:primitive(v) if key=='Primitive' else routes[v] if key=='Route' else inp[v]}
 def delayed(p):return None if p is None else {'at':at(p['at']),'owner':owner(p['owner'])}
 def sink(p):return {'route':routes[p['route']],'ordinal':p['ordinal']}
 def blocks(bs):
  for b in bs:b['at']=at(b['at'])
 for gidx,g in enumerate(leaf['gates']):
  matches=[(j,x) for j,x in enumerate(source['gates']) if x==g];assert len(matches)==1
  actual=next(x for x in c['instances']['instances'] if x['logical_gate']==gidx);parent=next(x for x in root['instances']['instances'] if x['id']==ins[actual['id']]);assert parent['logical_gate']==matches[0][0] and parent['implementation']==actual['implementation'] and parent['role']==actual['role'];gate_checks.append(matches[0][0])
 assert [source['inputs'][inp[j]] for j in range(len(leaf['inputs']))]==leaf['inputs'];assert [source['outputs'][out[j]] for j in range(len(leaf['outputs']))]==leaf['outputs']
 for section in sections:
  items=copy.deepcopy(c[section])
  for p in items:
   if section=='placements':p['id']=primitive(p['id']);p['anchor']=at(p['anchor']);p['delayed']=delayed(p['delayed']);blocks(p['blocks'])
   elif section=='boundaries':p['endpoint']=endpoint(p['endpoint']);p['delayed']=delayed(p['delayed']);blocks(p['blocks'])
   elif section=='output_boundary_aliases':p[0]=out[p[0]];p[1]=out[p[1]]
   elif section=='connections':p['id']=connection(p['id']);p['source']=endpoint(p['source']);p['landing']=endpoint(p['landing']);p['route']=routes[p['route']];p['sink']=sink(p['sink'])
   elif section=='routes':
    p['id']=routes[p['id']];p['source']=endpoint(p['source']);blocks(p['cells']);blocks(p['floors'])
    for b in p['branches']:
     b['sink']=sink(b['sink']);b['target']={'Connection':connection(b['target']['Connection'])} if 'Connection' in b['target'] else {'DeclaredOutput':out[b['target']['DeclaredOutput']]};b['root']=at(b['root']);b['path']=[at(a) for a in b['path']];b['terminal']['at']=at(b['terminal']['at']);b['terminal']['sink']=sink(b['terminal']['sink']);b['terminal']['delayed_owner']=owner(b['terminal']['delayed_owner'])
   elif section=='repeater_handovers':
    p[0]=sink(p[0])
    for h in p[1]:h['primitive']=primitive(h['primitive']);h['support']=at(h['support'])
   elif section=='junctions':p['id']=ins[p['id']];p['at']=at(p['at']);p['contributors']=[endpoint(x) for x in p['contributors']];blocks(p['cells'])
   elif section=='observations':p['site']['id']=obs(p['site']['id']);p['site']['at']=at(p['site']['at']);p['site']['logical_owner']=None if p['site']['logical_owner'] is None else ins[p['site']['logical_owner']]
  all_records[section]+=items
 _,cells,_=frozen_world(r/f'leaf{i}-world.json.gz')
 for xyz,state in cells.items():
  xyz=tuple(xyz[j]+origin['xyz'[j]] for j in range(3));assert xyz in original and state==original[xyz];assert xyz not in union or union[xyz]==state;union[xyz]=state
for section in sections:
 canonical=lambda xs:collections.Counter(json.dumps(x,sort_keys=True,separators=(',',':')) for x in xs)
 assert canonical(all_records[section])==canonical(root[section]),section
assert sorted(gate_checks)==list(range(len(source['gates']))),'gate cones overlap or omit source gates'
assert union==original,'projection omits or adds original occupied cells'
result={'accepted':True,'independently_recomputed':True,'original_occupied_cells':len(original),'source_gate_ownership_exact_once':len(gate_checks),'declared_outputs':len(source['outputs']),'full_owned_record_roundtrip_counts':{s:len(root[s]) for s in sections},'all_record_payloads_exact_after_inverse_typed_id_and_origin_translation':True,'all_occupied_cell_states_exact':True,'original_union_sealed_world_hash':original_hash,'quality_optimization':False}
(r/'independent-identity-result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))

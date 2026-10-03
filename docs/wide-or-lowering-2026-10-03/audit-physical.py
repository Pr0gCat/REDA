import pathlib,json,gzip,collections,copy,hashlib
p=pathlib.Path(__file__).parent
protocol=json.loads((p/'physical-protocol.json').read_text())
def expected(record,bits):
 values={name:bool(bits&(1<<i)) for i,name in enumerate(record['inputs'])};out={}
 for gate in record['gates']:
  inputs=[values[s] for s in gate['inputs']]
  assert gate['kind'] in ['nor','merge']
  value=not any(inputs) if gate['kind']=='nor' else any(inputs)
  values[gate['output']]=value;out['gate:'+gate['output']]=value
 assert values['merged']==(bits!=0)
 out['lamp:merged']=bits!=0
 return out
def audit(record):
 n=record['count'];limit=(1<<n)-1
 pairs=[(f,t) for f in range(limit+1) for t in range(limit+1)] if n<=5 else [(f,t) for f in range(limit+1) for t in [*(f^(1<<bit) for bit in range(n)),f^limit]]
 schedule=[bits for pair in pairs for bits in pair]
 assert [phase['bits'] for phase in record['phases']]==schedule
 assert set(schedule)==set(range(limit+1))
 state=record['power_on'].copy();assert state==expected(record,0)
 for phase in record['phases']:
  prior=-1
  for event in phase['events']:
   assert event['label'] in state
   assert prior<=event['tick']<=phase['ticks'];prior=event['tick']
   state[event['label']]=event['value']
  assert state==phase['actual']==expected(record,phase['bits'])
 return len(schedule)
records={};corruptions=[];summaries=[]
for n in protocol['cases']:
 group=[]
 for repeat in range(1,protocol['repeats']+1):
  raw=(p/f'physical-{n}-r{repeat}.jsonl.gz').read_bytes();lines=gzip.decompress(raw).splitlines();assert len(lines)==1
  r=json.loads(lines[0]);phases=audit(r);group.append(r);records[(n,repeat)]=r
  summaries.append({'count':n,'repeat':repeat,'phases':phases,'world_fingerprint':r['world_fingerprint'],'artifact_sha256':hashlib.sha256(pathlib.Path(r['artifact']).read_bytes()).hexdigest()})
 canonical=lambda r:{k:v for k,v in r.items() if k not in ['repeat','artifact']}
 assert all(canonical(r)==canonical(group[0]) for r in group)
 # A last event that changes its channel makes removing it observable.
 r=group[0];state=r['power_on'].copy();witness=None;public=None
 for i,phase in enumerate(r['phases']):
  before=state.copy()
  for j,e in enumerate(phase['events']):
   if e['value']!=state[e['label']]:witness=(i,j)
   if e['label']=='lamp:merged':public=(i,j)
   state[e['label']]=e['value']
  if witness is not None and public is not None:break
 assert witness is not None and public is not None
 for kind in ['wrong_truth','missing_event','wrong_public_event']:
  broken=copy.deepcopy(r)
  if kind=='wrong_truth':broken['phases'][0]['actual']['lamp:merged']=not broken['phases'][0]['actual']['lamp:merged']
  elif kind=='missing_event':i,j=witness;broken['phases'][i]['events'].pop(j)
  else:i,j=public;broken['phases'][i]['events'][j]['value']=not broken['phases'][i]['events'][j]['value']
  try:audit(broken)
  except AssertionError:corruptions.append({'count':n,'kind':kind,'rejected':True})
  else:raise AssertionError(f'corruption survived: {n} {kind}')
result={'qualified':True,'worlds':len(summaries),'phases':sum(r['phases'] for r in summaries),'all_lowered_gate_and_public_events_replayed':True,'three_repeats_complete_records_equal':True,'corruption_controls':corruptions,'worlds_detail':summaries,'scope':'wide OR support only; not complete ROM/Mux, arbitrary composition or eight-item completion'}
(p/'physical-audit.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({k:v for k,v in result.items() if k not in ['worlds_detail','corruption_controls']}))

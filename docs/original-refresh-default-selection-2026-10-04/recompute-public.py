import gzip, json, sys
from pathlib import Path
p=Path(sys.argv[1]); profiles=[]
for name in ['baseline','candidate']:
 file=p/f'{name}-phases.json'; text=file.read_text() if file.exists() else gzip.decompress(file.with_suffix('.json.gz').read_bytes()).decode(); rows=json.loads(text)['phases']; previous=0; previous_truth=[False,False,True,False,False,False,False,True,False]
 with gzip.open(p/f'{name}-events.jsonl.gz','rt') as f:
  for i,line in enumerate(f):
   raw=json.loads(line); row=rows[i]
   assert (raw['from'],raw['to'],raw['phase'])==(row['from'],row['to'],row['phase'])
   address=row['address']; a,b,c,d,e,z=[bool(address&(1<<bit)) for bit in range(6)]
   truth=[a or b,False,True,c and d,c and d,c,e!=z,not(e or z),z]
   channels=[]
   for kind in ['JunctionOutput','DeclaredOutput']:
    for ordinal,target in enumerate(truth):
     es=[x for x in raw['events'] if x[1]=={kind:ordinal}]
     metric=[int(len(es)>1),int(len(es)>1 and (previous^address).bit_count()==1),es[-1][0] if es else 0,0,0]
     for left,right in zip(es,es[1:]):
      if left[4]!=target:
       width=right[0]-left[0]; metric[3]+=width;metric[4]=max(metric[4],width)
     assert (es[-1][4] if es else previous_truth[ordinal])==target
     channels.append(metric)
   assert channels==row['channels'],(name,i,'channels')
   assert sum('PrimitiveOutput' in x[1] for x in raw['events'])==row['cell_boolean_events']
   assert all(0<=x[0]<=row['ticks'] for x in raw['events'])
   previous=address;previous_truth=truth
  assert i+1==len(rows)==8192
 profiles.append(rows)
regressions=[]
for i,(b,c) in enumerate(zip(*profiles)):
 for k in ['ticks','processed_queue_events','cell_boolean_events']:
  if c[k]>b[k]:regressions.append([i,k])
 for j,(bm,cm) in enumerate(zip(b['channels'],c['channels'])):
  for k,(bv,cv) in enumerate(zip(bm,cm)):
   if cv>bv:regressions.append([i,j,k])
result={'phases':8192,'raw_public_waveforms_recomputed':True,'raw_cell_boolean_events_recomputed':True,'queue_work_policy':'compare frozen producer counter per phase','regressions':regressions,'accepted':not regressions}
(p/'public-recompute-result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result));assert not regressions

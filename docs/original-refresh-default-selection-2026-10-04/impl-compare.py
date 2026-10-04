from pathlib import Path
import json,gzip,hashlib,collections,sys
r=Path(sys.argv[1]);bdoc=json.loads((r/'baseline-actual-implementation.json').read_text());cdoc=json.loads((r/'candidate-actual-implementation.json').read_text());b=bdoc['actual_implementation'];c=cdoc['actual_implementation']
assert len(b['inputs'])==len(c['inputs'])
map_signal=dict(zip(c['inputs'],b['inputs']));remaining=list(b['gates']);pending=list(c['gates']);pairs=[]
while pending:
 progress=False
 for cg in list(pending):
  if not all(i in map_signal for i in cg['inputs']):continue
  mapped_inputs=[map_signal[i] for i in cg['inputs']]
  matches=[bg for bg in remaining if bg['kind']==cg['kind'] and bg['inputs']==mapped_inputs]
  assert len(matches)==1,('nonbijective graph signature',cg,matches)
  bg=matches[0];assert cg['output'] not in map_signal
  map_signal[cg['output']]=bg['output'];pairs.append({'baseline':bg,'candidate':cg});remaining.remove(bg);pending.remove(cg);progress=True
 assert progress,'cyclic/unmapped candidate gates'
assert not remaining and len(pairs)==len(b['gates'])==len(c['gates'])
assert [map_signal[o] for o in c['outputs']]==b['outputs'],'ordered outputs/constants/aliases mismatch'
assert bdoc['source']==cdoc['source']
assert b==bdoc['source'],'baseline actual implementation differs from source'
for name,doc in [('baseline',bdoc),('candidate',cdoc)]:
 with gzip.open(r/f'{name}-world.json.gz','rt') as f:w=json.load(f)
 assert w['fingerprint']==doc['world_fingerprint'],'extraction does not bind executed world'
proof={'complete_bijection':True,'gates':pairs,'primary_inputs':list(zip(c['inputs'],b['inputs'])),'ordered_outputs':list(zip(c['outputs'],b['outputs'])),'source_constant_origins':[g for g in b['gates'] if not g['inputs']],'aliased_output_ordinals':[[i for i,x in enumerate(b['outputs']) if x==o] for o in dict.fromkeys(b['outputs']) if b['outputs'].count(o)>1],'baseline_world':bdoc['world_fingerprint'],'candidate_world':cdoc['world_fingerprint'],'method':'unique exact gate-kind and recursively mapped ordered-input signatures; verify all actual ordered output bindings; display prefixes not used','unmatched_candidate_gates':[]}
(r/'impl-bijection.json').write_text(json.dumps(proof,indent=2)+'\n')
def truth(address):
 values={n:bool(address&(1<<i)) for i,n in enumerate(b['inputs'])};pending=list(b['gates'])
 while pending:
  progressed=False
  for g in list(pending):
   if not all(i in values for i in g['inputs']):continue
   v=any(values[i] for i in g['inputs']);assert g['kind'].startswith(('Nor(','Or(')),g
   values[g['output']]=not v if g['kind'].startswith('Nor(') else v
   pending.remove(g);progressed=True
  assert progressed
 return values
names=[g['output'] for g in b['gates']];state={name:truth(0)[name] for name in names};cs=dict(state);previous=0;reg=[];counts=collections.Counter();maxdelta=collections.defaultdict(int);checks=0
with gzip.open(r/'baseline-events.jsonl.gz','rt') as bf,gzip.open(r/'candidate-events.jsonl.gz','rt') as cf:
 for index,(bl,cl) in enumerate(zip(bf,cf,strict=True)):
  be=json.loads(bl);ce=json.loads(cl);assert [be[k] for k in ['from','to','phase']]==[ce[k] for k in ['from','to','phase']]
  address=be['from'] if be['phase']==0 else be['to'];target=truth(address)
  def grouped(e,candidate):
   out={name:[] for name in names}
   for tick,identity,pos,label,value in e['events']:
    if 'InstanceOutput' not in identity:continue
    assert label.startswith('gate:');signal=label[5:];signal=map_signal[signal] if candidate else signal
    assert signal in out;out[signal].append((tick,value))
   return out
  bg=grouped(be,False);cg=grouped(ce,True)
  def metric(events,value):
   pulses=[z[0]-a[0] for a,z in zip(events,events[1:]) if a[1]!=value]
   return [int(len(events)>1),int(len(events)>1 and (previous^address).bit_count()==1),events[-1][0] if events else 0,sum(pulses),max(pulses,default=0)]
  for name in names:
   for events,s in [(bg[name],state),(cg[name],cs)]:
    if events:s[name]=events[-1][1]
    assert s[name]==target[name],('independent gate end truth',index,name,s[name],target[name])
   bm=metric(bg[name],target[name]);cm=metric(cg[name],target[name]);checks+=1
   for mi,label in enumerate(['glitch','single_input_glitch','arrival','pulse_total','pulse_worst']):
    if cm[mi]>bm[mi]:
     key=f'{name}:{label}';counts[key]+=1;maxdelta[key]=max(maxdelta[key],cm[mi]-bm[mi])
     if len(reg)<100:reg.append({'phase_index':index,'from':be['from'],'to':be['to'],'phase':be['phase'],'gate':name,'metric':label,'baseline':bm[mi],'candidate':cm[mi]})
  previous=address
 assert index+1==8192
result={'accepted':not counts,'phases':8192,'mapped_gates':len(names),'phase_gate_checks':checks,'independent_all_gate_end_truth':True,'bijection_complete':True,'regression_counts':dict(counts),'maximum_delta':dict(maxdelta),'first_100_regressions':reg,'metrics':['glitch','single_input_glitch','arrival','pulse_total','pulse_worst'],'unmapped_gates':[]}
(r/'impl-full-wave-result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))

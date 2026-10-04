import gzip,json,hashlib
from pathlib import Path
import sys
P=Path(sys.argv[1])
results=[]
for base in ['mixed','sixteen_direct','sixteen_inverted','unused_inputs_constant']:
 for suffix in ['', '_five']:
  name=base+suffix
  mp=P/f'production-{name}-correspondence.json';sp=P/f'production-{name}-phases.json'
  c=json.loads(mp.read_text()); summaries=json.loads(sp.read_text()); n=len(c['source_inputs']); states=1<<n
  tables=[[] for _ in c['actual_gate_sites']]
  for address in range(states):
   values={s:bool(address&(1<<i)) for i,s in enumerate(c['input_bindings'])};pending=list(enumerate(c['actual_gate_sites']))
   while pending:
    ready=[(i,s) for i,s in pending if all(k in values for k in s['inputs'])];assert ready
    for i,s in ready:
     operands=[values[k] for k in s['inputs']];kind=s['kind']
     if kind=='Buf':assert len(operands)==1; value=operands[0]
     else:
      op,arity=kind[:-1].split('(');assert int(arity)==len(operands) and op in ('Or','Nor');value=any(operands) if op=='Or' else not any(operands)
     values[s['gate']]=value;tables[i].append(value);pending.remove((i,s))
  assert all(t==s['truth_in_source_address_order'] for t,s in zip(tables,c['actual_gate_sites']))
  files=list(P.glob(f'production-{name}-[0-9]*.jsonl.gz'));assert len(files)==1
  previous=0;count=0
  with gzip.open(files[0],'rt') as stream:
   for index,line in enumerate(stream):
    r=json.loads(line);a=r['address'];expected=([bool(a&1),not bool(a&4),True,False,not bool(a&4)] if base=='mixed' else [bool(a&2)]*16 if base=='sixteen_direct' else [not bool(a&8)]*16 if base=='sixteen_inverted' else [True,False]);width=len(expected)
    assert r['phase']==index and r['from']==index//(2*states) and r['to']==(index//2)%states and a==(r['from'] if index%2==0 else r['to'])
    assert r['expected']==expected+expected+[t[a] for t in tables]
    assert r['sites'][width:2*width]==[pos for _,pos in c['public_occurrences']]
    assert r['sites'][2*width:]==[s['at'] for s in c['actual_gate_sites']]
    events=[[] for _ in r['sites']];cells=0
    for e in r['events']:
     if e['label'].startswith('channel'):
      k=int(e['label'][7:]);assert e['position']==r['sites'][k];events[k].append(e)
     else:assert e['label'].startswith('cell');cells+=1
    measured=[]
    for k,changes in enumerate(events):
     target=r['expected'][k];assert not changes or changes[-1]['value']==target
     widths=[b['tick']-a['tick'] for a,b in zip(changes,changes[1:]) if a['value']!=target]
     measured.append([int(len(changes)>1),int(len(changes)>1 and (previous^r['address']).bit_count()==1),changes[-1]['tick']-r['start'] if changes else 0,sum(widths),max(widths,default=0)])
    s=summaries[index];assert measured==s['channels'] and cells==s['cells'] and r['ticks']==s['ticks'] and r['queue_work']==s['work'];previous=a;count+=1
  assert count==len(summaries)==2*states*states
  results.append({'case':name,'phases':count,'independent_truth_sites':len(tables),'hashes':{f.name:hashlib.sha256(f.read_bytes()).hexdigest() for f in [mp,sp,files[0]]}})
  print(name,count,flush=True)
out={'cases':results,'total_phases':sum(r['phases'] for r in results),'raw_metric_mismatches':0,'truth_mismatches':0}
assert out['total_phases']==10240
Path(__file__).with_name('generic-independent-audit.json').write_text(json.dumps(out,indent=2)+'\n')

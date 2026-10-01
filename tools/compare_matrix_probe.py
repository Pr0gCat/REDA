"""Compare all 4096 tables against untouched frozen physical controls, per metric."""
import gzip,json,sys

def quality(arm,words):
 assert arm['correct'] and len(arm['phases'])==32
 assert [(p['from'],p['to'],p['phase']) for p in arm['phases']]==[(a,b,k) for a in range(4) for b in range(4) for k in range(2)]
 for p in arm['phases']:
  assert p['address']==(p['from'] if p['phase']==0 else p['to'])
  assert p['expected']==[bool(words[p['address']]&(1<<bit)) for bit in range(3)]
  assert p['driver']==p['expected'] and p['lamp']==p['expected']
 q={'worst_settle_ticks':0,'total_settle_ticks':0}
 for kind in ['driver','lamp']:
  for key in ['glitches','single_input_glitches','last_change_ticks','total_pulse_ticks','worst_pulse_ticks']:q[kind+'_'+key]=0
 for p in arm['phases']:
  if p['phase']!=1:continue
  q['worst_settle_ticks']=max(q['worst_settle_ticks'],p['ticks']);q['total_settle_ticks']+=p['ticks']
  for kind in ['driver','lamp']:
   for bit,expected in enumerate(p['expected']):
    ev=[e for e in p['events'] if e['label']==f'{kind}{bit}']
    assert all(0<=e['tick']<=p['ticks'] for e in ev)
    assert not ev or ev[-1]['value']==expected
    assert ev or bool(words[p['from']]&(1<<bit))==expected
    q[kind+'_glitches']+=int(len(ev)>1)
    q[kind+'_single_input_glitches']+=int(len(ev)>1 and (p['from']^p['to']).bit_count()==1)
    q[kind+'_last_change_ticks']=max(q[kind+'_last_change_ticks'],max((e['tick'] for e in ev),default=0))
    for a,b in zip(ev,ev[1:]):
     if a['value']!=expected:
      width=b['tick']-a['tick'];q[kind+'_total_pulse_ticks']+=width;q[kind+'_worst_pulse_ticks']=max(q[kind+'_worst_pulse_ticks'],width)
 return q

with gzip.open(sys.argv[1],'rt') as f:fixed=[json.loads(l) for l in f]
with open(sys.argv[2]) as f:rows=[json.loads(l) for l in f]
assert len(fixed)==len(rows)==4096
cases=[]
for encoding,(old,new) in enumerate(zip(fixed,rows)):
 assert old["encoding"]==encoding and old["words"]==[(encoding>>(3*i))&7 for i in range(4)]
 assert old['encoding']==new['encoding'] and old['words']==new['words']
 assert old['control']==new['control'],new['encoding']
 a=quality(old['control'],old['words'])
 b=quality(new['candidate'],new['words']) if new['candidate']['correct'] else None
 worse=[k for k in a if b[k]>a[k]] if b else None
 better=[k for k in a if b[k]<a[k]] if b else None
 cases.append({'encoding':new['encoding'],'words':new['words'],'control':a,'candidate':b,'regressed':worse,'improved':better})
summary={'cases':4096,'correct':sum(c['candidate'] is not None for c in cases),'regressed':sum(bool(c['regressed']) for c in cases),'improved_without_regression':sum(bool(c['improved']) and not c['regressed'] for c in cases),'tied':sum(c['candidate'] is not None and not c['improved'] and not c['regressed'] for c in cases),'all_fixed_controls_exact':True}
print(json.dumps({'summary':summary,'cases':cases}))

if summary["correct"] != 4096 or summary["regressed"]:
 sys.exit(1)

"""Audit continuous setup history and target tradeoffs of prospective selection.

Diagnostic only. Usage: python3 tools/probe_default_setup_tradeoff.py FAMILY.jsonl.gz
"""
import gzip,json,sys
from compare_matrix_probe import quality

def setup(arm,words):
 q={k:0 for k in quality(arm,words)};previous=0
 for p in arm['phases']:
  if p['phase']==0:
   q['worst_settle_ticks']=max(q['worst_settle_ticks'],p['ticks']);q['total_settle_ticks']+=p['ticks']
   for kind in ['driver','lamp']:
    for bit,expected in enumerate(p['expected']):
     ev=[e for e in p['events'] if e['label']==f'{kind}{bit}']
     assert all(0<=e["tick"]<=p["ticks"] for e in ev)
     assert not ev or ev[-1]["value"]==expected
     assert ev or bool(words[previous]&(1<<bit))==expected
     q[kind+'_glitches']+=int(len(ev)>1)
     q[kind+'_single_input_glitches']+=int(len(ev)>1 and (previous^p['address']).bit_count()==1)
     q[kind+'_last_change_ticks']=max(q[kind+'_last_change_ticks'],max((e['tick'] for e in ev),default=0))
     for a,b in zip(ev,ev[1:]):
      if a['value']!=expected:
       width=b['tick']-a['tick'];q[kind+'_total_pulse_ticks']+=width;q[kind+'_worst_pulse_ticks']=max(q[kind+'_worst_pulse_ticks'],width)
  previous=p['address']
 return q

assert len(sys.argv)==2, __doc__
rows=[json.loads(l) for l in gzip.open(sys.argv[1],'rt')]
assert len(rows)==4096
reg_candidate=[];glitches=[];witness=[]
for encoding,row in enumerate(rows):
 assert row["encoding"]==encoding and row["words"]==[(encoding>>(3*a))&7 for a in range(4)]
 candidate=row['candidate'];control=row['control']; cq=quality(candidate,row['words']);sq=quality(control,row['words']) if control['correct'] else None
 if sq is None or all(cq[k]<=v for k,v in sq.items()):continue
 selected=setup(control,row['words']);previous=setup(candidate,row['words']);worse=[k for k,v in previous.items() if selected[k]>v]
 if worse:reg_candidate.append({'encoding':row['encoding'],'metrics':worse})
 if selected['driver_glitches'] or selected['lamp_glitches']:glitches.append(row['encoding'])
 witness.append({'encoding':row['encoding'],'words':row['words'],'target_stock_worse':[k for k,v in cq.items() if sq[k]>v],'target_player_worse':[k for k,v in sq.items() if cq[k]>v]})
print(json.dumps({'shipping_choices':len(witness),'setup_regressions_against_player':reg_candidate,'setup_glitched_choices':glitches,'target_tradeoffs':witness,'setup_evaluated':True,'automatic_selection_enabled':False},indent=2))

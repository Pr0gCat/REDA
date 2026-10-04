#!/usr/bin/env python3
import csv,hashlib,json,platform,sys
from pathlib import Path
BASE=Path(__file__).resolve().parent
class FloatingBus(Exception): pass
class ContentionBus(Exception): pass

def drivers(state):
    return [(bool(state & (1 << (2*i))),bool(state & (1 << (2*i+1)))) for i in range(3)]

def reference(ds):
    active=[value for present,value in ds if present]
    if not active: return 'Z'
    if any(value != active[0] for value in active[1:]): return 'X'
    return '1' if active[0] else '0'

def candidate(ds):
    high=any(p and v for p,v in ds)
    low=any(p and not v for p,v in ds)
    return dict(value=int(high),present=high or low,conflict=high and low,floating=not(high or low),valid=(high or low) and not(high and low))

def observe(result):
    if result['conflict']: raise ContentionBus('opposite active drive values')
    if result['floating']: raise FloatingBus('no active driver')
    return result['value']

counts={k:0 for k in ['0','1','X','Z']}
rows=[]
negative=0
for state in range(64):
    ds=drivers(state); expected=reference(ds); actual=candidate(ds)
    counts[expected]+=1
    try:
        observed=observe(actual)
        assert expected in ['0','1'] and observed==int(expected)
        diagnosis='valid'
    except (FloatingBus,ContentionBus) as error:
        diagnosis=type(error).__name__
        assert (expected,diagnosis) in [('Z','FloatingBus'),('X','ContentionBus')]
        negative+=1
    rows.append(dict(state=state,drivers=ds,expected=expected,resolver=actual,diagnosis=diagnosis))
(BASE/'states.json').write_text(json.dumps(rows,indent=2)+'\n')
with (BASE/'transitions.csv').open('w',newline='') as f:
    out=csv.writer(f);out.writerow(['from','to','phase','expected','value','present','conflict','floating','valid','checked_diagnosis'])
    for before in range(64):
        for after in range(64):
            for phase,state in enumerate([before,after]):
                row=rows[state]; r=candidate(drivers(state))
                assert r==row['resolver'] and reference(drivers(state))==row['expected']
                out.writerow([before,after,phase,row['expected'],*[int(r[k]) for k in ['value','present','conflict','floating','valid']],row['diagnosis']])
# Independent negative mutations must be caught by checked-observation oracle.
mutations=[]
for name,state,change in [('floating_defaults_zero',0,dict(floating=False,present=True,valid=True)),('conflict_returns_high',19,dict(conflict=False,valid=True))]:
    r=candidate(drivers(state));r.update(change)
    try: observe(r)
    except (FloatingBus,ContentionBus): raise AssertionError('mutation did not actually remove diagnostic')
    assert reference(drivers(state)) in ['X','Z']
    mutations.append(dict(name=name,state=state,reference=reference(drivers(state)),mutated_observation=observe(r),caught_by_domain_oracle=True))
(BASE/'negative-mutations.json').write_text(json.dumps(mutations,indent=2)+'\n')
summary=dict(states=64,ordered_transitions=4096,settled_endpoint_rows=8192,class_counts=counts,negative_checked_states=negative,model_equivalence=True,standard_simulator_evidence=False,physical_evidence=False,inout_support_claim=False,python=sys.version,platform=platform.platform())
(BASE/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
(BASE/'sha256.json').write_text(json.dumps({p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(BASE.iterdir()) if p.is_file() and p.name!='sha256.json'},indent=2)+'\n')
print(json.dumps(summary))

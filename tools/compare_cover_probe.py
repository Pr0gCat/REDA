import argparse,json
from collections import defaultdict

def quality(arm,kind):
 ps=arm['phases'];width=len(ps[0]['expected']);q=defaultdict(int);previous=0
 for p in ps:
  if p['phase']==kind:
   q['worst_settle_ticks']=max(q['worst_settle_ticks'],p['ticks']);q['total_settle_ticks']+=p['ticks']
   for channel in range(2):
    name=['driver','lamp'][channel];any_glitch=False
    for bit,expected in enumerate(p['expected']):
     events=[e for e in p['events'] if int(e['label'])==channel*width+bit]
     glitched=len(events)>1;any_glitch|=glitched
     q[name+'_glitches']+=glitched;q[name+'_single_input_glitches']+=glitched and (previous^p['address']).bit_count()==1
     q[name+'_last_change_ticks']=max(q[name+'_last_change_ticks'],max((e['tick'] for e in events),default=0))
     for a,b in zip(events,events[1:]):
      if a['value']!=expected:
       width_ticks=b['tick']-a['tick'];q[name+'_total_pulse_ticks']+=width_ticks;q[name+'_worst_pulse_ticks']=max(q[name+'_worst_pulse_ticks'],width_ticks)
    q[name+'_glitch_transitions']+=any_glitch
  previous=p['address']
 return dict(q)
def replay_events(previous, events, key, ticks):
    measured=dict(previous);last_tick={name:0 for name in measured}
    for event in events:
        name=event[key]
        assert name in measured
        assert 0<=last_tick[name]<=event['tick']<=ticks
        assert isinstance(event['value'],bool) and event['value']!=measured[name]
        measured[name]=event['value'];last_tick[name]=event['tick']
    return measured

def compare(control_row, candidate_row):
    control=control_row['result']['Ok']
    if 'Err' in candidate_row['result']:
        return {'polarity':candidate_row['polarity'],'accepted':False,'generation_error':candidate_row['result']['Err']}
    arm=candidate_row['result']['Ok']
    assert control_row['source_inputs']==candidate_row['source_inputs']
    assert control_row['source_outputs']==candidate_row['source_outputs']
    states=1<<len(control_row['source_inputs'])
    assert len(control['phases'])==len(arm['phases'])==2*states*states
    protocol=[(a,b,p,[a,b][p]) for a in range(states) for b in range(states) for p in range(2)]
    for record in [control,arm]:
        assert [(p['from'],p['to'],p['phase'],p['address']) for p in record['phases']]==protocol
        def evaluate(gates, inputs, address):
            values={name:bool(address&(1<<i)) for i,name in enumerate(inputs)}
            pending=list(gates)
            while pending:
                ready=[g for g in pending if all(s in values for s in g['inputs'])]
                assert ready, 'cyclic or undriven recorded graph'
                for g in ready:
                    operands=[values[s] for s in g['inputs']]
                    assert g['kind'] in ['nor','or','merge']
                    value=any(operands)
                    values[g['output']]=not value if g['kind']=='nor' else value
                    pending.remove(g)
            return values
        # Graphs are fixed throughout the ordered protocol. Reuse their Boolean
        # truth values; every recorded state and event is still checked below.
        source_values_by_address=[evaluate(record['source_gates'],control_row['source_inputs'],address) for address in range(states)]
        internal_by_address=[evaluate(record['implemented_gates'],record['implemented_inputs'],address) for address in range(states)]
        previous_internal=internal_by_address[0]
        if 'landing_sites' in record:
            sites=record['landing_sites'];previous_landings=record['initial_landings']
            assert set(sites)==set(previous_landings)
            gates={g['output']:g for g in record['implemented_gates']}
            observed=set()
            for name,site in sites.items():
                gate=gates[site['receiver']];index=site['input_index']
                assert 0<=index<len(gate['inputs']) and gate['inputs'][index]==site['source']
                assert gate['kind']==site['receiver_kind']
                assert site['role'] in ['terminal','last_repeater']
                if site['role']=='last_repeater':assert site['state']['kind']=='Repeater'
                if site['state']['kind']=='Repeater':assert previous_landings[name]==previous_internal[site['source']]
                observed.add((site['receiver'],index))
            assert observed=={(g['output'],i) for g in gates.values() for i in range(len(g['inputs']))}, 'missing gate-input route observations'
        previous=record['phases'][0]['expected']
        width=len(previous)
        for p in record['phases']:
            source_values=source_values_by_address[p['address']]
            assert p['expected']==[source_values[s] for s in control_row['source_outputs']]
            internal=internal_by_address[p['address']]
            assert set(p['internal'])=={g['output'] for g in record['implemented_gates']}
            assert all(value==internal[s] for s,value in p['internal'].items())
            if 'internal_events' in p:
                measured=replay_events({s:previous_internal[s] for s in p['internal']},p['internal_events'],'signal',p['ticks'])
                assert measured==p['internal'], 'internal events do not explain the retained state'
            if 'landing_sites' in record:
                assert set(p['landings'])==set(record['landing_sites'])
                measured=replay_events(previous_landings,p['landing_events'],'landing',p['ticks'])
                assert measured==p['landings'], 'landing events do not explain the retained state'
                for name,site in record['landing_sites'].items():
                    if site['state']['kind']=='Repeater':
                        assert measured[name]==internal[site['source']]
                previous_landings=measured
            previous_internal=internal
            assert p['driver']==p['lamp']==p['expected']
            assert all(0<=int(e['label'])<2*width for e in p['events'])

            for channel in range(2):
                for bit,expected in enumerate(p['expected']):
                    events=[e for e in p['events'] if int(e['label'])==channel*width+bit]
                    assert events or previous[bit]==expected
                    assert not events or events[-1]['value']==expected
                    assert all(0<=e['tick']<=p['ticks'] for e in events)
                    assert [e['tick'] for e in events]==sorted(e['tick'] for e in events)
            previous=p['expected']
    assert [p['expected'] for p in control['phases']]==[p['expected'] for p in arm['phases']]
    result={'polarity':candidate_row['polarity'],'certified':arm['strict_certified'],'control_certified':control['strict_certified']}
    for kind,name in enumerate(['setup','target']):
        a=quality(control,kind);b=quality(arm,kind)
        keys=sorted(a.keys()|b.keys())
        result[name]={'control':a,'candidate':b,'regressed':[k for k in keys if b.get(k,0)>a.get(k,0)],'improved':[k for k in keys if b.get(k,0)<a.get(k,0)]}
    result['accepted']=result['certified'] and not result['setup']['regressed'] and not result['target']['regressed']
    return result

if __name__=='__main__':
    parser=argparse.ArgumentParser(description='Independently audit complete retained setup/target cover probe waveforms')
    parser.add_argument('control');parser.add_argument('candidate');args=parser.parse_args()
    with open(args.control) as f:controls=[json.loads(l) for l in f]
    assert len(controls)==1
    with open(args.candidate) as f:results=[compare(controls[0],json.loads(l)) for l in f]
    assert results, "candidate file contains no complete records"
    print(json.dumps(results,indent=2))

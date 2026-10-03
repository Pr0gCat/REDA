import json
from pathlib import Path

def audit(path):
    records=[json.loads(line) for line in Path(path).read_text().splitlines()]
    assert len(records)==6
    by_depth={}
    for record in records:
        assert record['depth'] in (0,32) and record['repeat'] in (1,2,3)
        assert len({tuple(p) for p in record['outputs']})==3
        assert len(record['source_gates'])==record['depth']+1
        def evaluate(inputs,gates,address):
            values={name:bool(address&(1<<bit)) for bit,name in enumerate(inputs)}
            pending=list(gates)
            while pending:
                ready=[gate for gate in pending if all(name in values for name in gate['inputs'])]
                assert ready
                for gate in ready:
                    args=[values[name] for name in gate['inputs']]
                    assert gate['kind'] in ('nor','merge','buf')
                    values[gate['output']]=not any(args) if gate['kind']=='nor' else any(args) if gate['kind']=='merge' else args[0]
                    pending.remove(gate)
            return values
        def expected(address):
            values=evaluate(record['implemented_inputs'],record['implemented_gates'],address)
            result={'gate:'+gate['output']:values[gate['output']] for gate in record['implemented_gates']}
            a,b=bool(address&1),bool(address&2)
            outputs=[a,not(a or b),a]
            source=evaluate(['a','b'],record['source_gates'],address)
            assert [source['a'],source['y'],source['a']]==outputs
            assert [values[name] for name in record['implemented_outputs']]==outputs
            result.update({'input:0':a,'input:1':b})
            result.update({f'output:{bit}':value for bit,value in enumerate(outputs)})
            return result,outputs
        observed,unused=expected(0)
        assert record['power_on']==observed
        protocol=[(f,t,phase,address) for f in range(4) for t in range(4) for phase,address in enumerate([f,t])]
        assert len(record['phases'])==32
        for row,(f,t,phase,address) in zip(record['phases'],protocol):
            assert (row['from'],row['to'],row['phase'],row['address'])==(f,t,phase,address)
            final,outputs=expected(address)
            assert row['internal']==final and row['drivers']==outputs
            assert row['ticks']<=2000
            last={}
            for event in row['events']:
                label=event['label']
                assert label in observed and 0<=event['tick']<=row['ticks']
                assert event['tick']>=last.get(label,0)
                assert event['value']!=observed[label]
                observed[label]=event['value'];last[label]=event['tick']
            assert observed==final,'full event replay must reach independently computed implementation truth'
        normalized={key:value for key,value in record.items() if key not in ('repeat','artifact')}
        previous=by_depth.setdefault(record['depth'],normalized)
        assert previous==normalized,'repeats must preserve exact waves and world identity'
    return {'worlds':len(records),'continuous_phases':sum(len(r['phases']) for r in records),'all_implementation_and_public_events_replayed':True,'duplicate_outputs_have_distinct_positions':True,'repeats_exact':True,'worst_settlement_ticks':max(row['ticks'] for r in records for row in r['phases'])}

if __name__=='__main__':
    import sys
    print(json.dumps(audit(sys.argv[1]),indent=2))

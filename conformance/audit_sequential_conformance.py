#!/usr/bin/env python3
"""Read-only terminal audit against frozen Rust evidence; no Minecraft connection."""
import argparse
import gzip
import hashlib
import json
import re
from pathlib import Path


def require(condition, message):
    if not condition:
        raise ValueError(message)


def load_lines(path):
    with gzip.open(path, 'rt') as stream:
        return [json.loads(line) for line in stream]


def expected_values(phase, reference):
    values = {'gate:'+gate['output']:phase['internal'][gate['output']] for gate in reference['gates']}
    values.update({'lamp:'+name:phase['state'][bit] for bit,name in enumerate(('q_a','q_b'))})
    for label,cell in phase['cells'].items():
        values['cell:'+label] = cell['value']
        if not label.endswith(':InvC'):
            values['cell:'+label+':locked'] = cell['locked']
    for gate in reference['gates']:
        if gate['kind'] == 'dff_p':
            values['gate:'+gate['output']+':locked'] = phase['cells'][gate['output']+':SData']['locked']
    return values


def parse_capture(response, sites):
    fields = {name:int(value) for name,value in re.findall(r'\b(t[01]|s\d+[pvl]):\s*(-?\d+)\b',response)}
    required = {'t0','t1'} | {f's{i}{suffix}' for i,site in enumerate(sites)
                for suffix in ('p','v','l') if suffix != 'l' or site['block_id']=='minecraft:repeater'}
    require(set(fields)==required and fields['t0']==fields['t1'], 'raw snapshot fields or atomic tick mismatch')
    values = {}
    for index,site in enumerate(sites):
        require(fields[f's{index}p']==1, 'raw snapshot missing actual block')
        value=fields[f's{index}v'];require(value in (0,1), 'raw non-Boolean value')
        values[site['label']]=bool(1-value if site['block_id']=='minecraft:redstone_wire' else value)
        if site['block_id']=='minecraft:repeater':
            value=fields[f's{index}l'];require(value in (0,1), 'raw non-Boolean lock')
            values[site['label']+':locked']=bool(value)
    return {'game_tick':fields['t0'],'values':values}


def audit(directory, reference_path, source_snapshot=None, source_manifest=None):
    report=json.loads((directory/'report.json').read_text())
    require(report['status']=='pass', 'terminal status is not pass')
    storage_command='data remove storage reda_sequential_20261004:trace snapshot'
    storage_response='Modified storage reda_sequential_20261004:trace'
    known_error='cleanup command was not acknowledged: '+storage_command+': '+storage_response
    errors={key:value for key,value in report.items() if 'error' in key}
    adjudication=errors=={'storage_remove_error':known_error}
    require('active_phase' not in report and (not errors or adjudication), 'incomplete phase or unadjudicated cleanup error')
    require(report['protocol']=='mixed-clock-reset-feedback/v1', 'wrong protocol')
    require(report['settle_cap_game_ticks']==2000 and report['dwell_game_ticks']==16, 'changed settlement/dwell contract')
    source_resolutions={}
    manifest=json.loads(source_manifest.read_text()) if source_manifest is not None else None
    if manifest is not None:
        require(set(manifest['bindings'])==set(report['source_sha256']), 'runtime source manifest coverage differs')
        for name,digest in manifest['runtime_python_sha256'].items():
            require(hashlib.sha256((source_manifest.parent/name).read_bytes()).hexdigest()==digest, 'runtime Python source differs: '+name)
    for name,digest in report['source_sha256'].items():
        path=Path(name)
        if manifest is not None:
            binding=manifest['bindings'][name]
            require(binding['sha256']==digest, 'runtime source manifest digest differs from original report')
            path=Path(binding['path'])
            if not path.is_absolute():path=source_manifest.parent/path
        elif source_snapshot is not None and (source_snapshot/path.name).exists():path=source_snapshot/path.name
        require(hashlib.sha256(path.read_bytes()).hexdigest()==digest, 'source binding changed: '+name)
        source_resolutions[name]=str(path)
    for name,digest in report['datapack_sha256'].items():
        require(hashlib.sha256((directory/'snapshot-pack'/name).read_bytes()).hexdigest()==digest, 'datapack binding changed: '+name)
    with gzip.open(reference_path,'rt') as stream:
        reference=next(json.loads(line.split('MIXED_CLOCK_RESET_TRACE ',1)[1]) for line in stream if line.startswith('MIXED_CLOCK_RESET_TRACE '))
    dump_path=next(Path(path) for name,path in source_resolutions.items() if name.endswith('.dump'))
    rows=[line.split() for line in dump_path.read_text().splitlines()]
    gates={row[1]:{'output':row[1],'inputs':row[2].split(','),'kind':row[3]} for row in rows if row[0]=='GATE'}
    require(gates=={gate['output']:gate for gate in reference['gates']}, 'qualified lowered graph differs')
    labels={row[2]:row[1] for row in rows if row[0]=='OUTLABEL'}
    require({label:name for name,label in labels.items()}==reference['outputs'], 'qualified output bindings differ')
    blocks={tuple(map(int,row[1:4])):row[-1] for row in rows if row[0]=='BLOCK'}
    expected_sites={}
    for row in rows:
        if row[0] not in ('GATEOUT','OUTPUT','DFFCELL'):continue
        position=tuple(map(int,row[3:6] if row[0]=='DFFCELL' else row[2:5]))
        label=('cell:'+row[1]+':'+row[2] if row[0]=='DFFCELL' else 'gate:'+row[1] if row[0]=='GATEOUT' else 'lamp:'+labels[row[1]])
        expected_sites[label]={'position':[a+b for a,b in zip(report['origin'],position)],'block_id':blocks[position]}
    require({site['label']:{key:site[key] for key in ('position','block_id')} for site in report['sites']}==expected_sites, 'actual observed site positions/components differ from dump')
    protocol=[{'clk_a':bool(clocks&1),'clk_b':bool(clocks&2),'reset_a':bool(changed&1),
               'reset_b':bool(changed&2),'en':bool(changed&4)} for control in range(8)
              for clocks in (0,1,3,2,0,3,0,2,3,1,0) for changed in (control,control^7,control)]
    require(len(report['phases'])==len(reference['phases'])==len(protocol)==264, 'missing protocol phases')
    state=[False,False];clocks=[False,False]
    for inputs,phase in zip(protocol,reference['phases']):
        previous=state.copy()
        if not clocks[0] and inputs['clk_a']:state[0]=False if inputs['reset_a'] else not previous[0] if inputs['en'] else previous[0]
        if clocks[1] and not inputs['clk_b']:state[1]=False if inputs['reset_b'] else previous[0]
        clocks=[inputs['clk_a'],inputs['clk_b']]
        require(phase['state']==state, 'frozen reference differs from simultaneous previous-state source oracle')
    targets=[expected_values(phase,reference) for phase in reference['phases']]
    sites=report['sites'];require(len(sites)==27 and len({site['label'] for site in sites})==27, 'incomplete observation sites')
    require({site['label'] for site in sites}=={name for name in targets[0] if not name.endswith(':locked')}, 'missing public/internal/cell observations')
    ticks=load_lines(directory/'ticks.jsonl.gz');commands=load_lines(directory/'commands.jsonl.gz')
    require(bool(ticks), 'no ticks')
    groups={name:[] for name in ['power_on',*range(264)]}
    previous=None
    for sample in ticks:
        require(sample['phase'] in groups, 'unknown tick phase')
        require(set(sample['values'])==set(targets[0]) and all(type(value) is bool for value in sample['values'].values()), 'tick omitted values/locks')
        events=[] if previous is None else [{'label':name,'value':value} for name,value in sample['values'].items() if previous['values'][name]!=value]
        require(sample['events']==events, 'tick event replay differs')
        if previous is not None:
            delta=sample['game_tick']-previous['game_tick']
            require(delta in (0,1), 'skipped/reversed game tick')
            require(delta==1 or sample['phase']!=previous['phase'] or (sample['phase']=='power_on' and len(groups['power_on'])==1), 'duplicate tick within phase')
        groups[sample['phase']].append(sample);previous=sample
    expected_phase_order=['power_on',*range(264)]
    observed_order=[]
    for sample in ticks:
        if not observed_order or observed_order[-1]!=sample['phase']:observed_order.append(sample['phase'])
    require(observed_order==expected_phase_order, 'tick phase order differs')
    for index,(record,frozen,inputs,target) in enumerate(zip(report['phases'],reference['phases'],protocol,targets)):
        require(record['phase']==index and record['inputs']==inputs==frozen['inputs'], 'source protocol differs')
        require(record['expected_state']==frozen['state'] and record['final']==target, 'source/public/internal/cell/lock target differs')
        samples=groups[index];require(bool(samples), 'phase has no tick samples')
        start=samples[0]['game_tick'];duration=samples[-1]['game_tick']-start
        require(len(samples)==duration+1 and duration==record['ticks']==record['matched_at']+16, 'phase tick/dwell coverage differs')
        require(0<=record['matched_at']<=2000 and samples[record['matched_at']]['values']==target, 'invalid target match')
        require(all(sample['values']!=target for sample in samples[:record['matched_at']]), 'earlier target match omitted')
        require(samples[-1]['values']==target, 'final snapshot target differs')
        events=[{'tick':sample['game_tick']-start,**event} for sample in samples for event in sample['events']]
        require(events==record['events'], 'phase event replay differs')
    power=report['power_on'];samples=groups['power_on']
    require(power['inputs']=={name:False for name in protocol[0]} and power['expected_state']==[False,False], 'power-on source state differs')
    require(power['final']==targets[0] and samples[-1]['values']==targets[0], 'power-on target differs')
    require(len(samples)==power['ticks']+2 and samples[-1]['game_tick']-samples[0]['game_tick']==power['ticks']==power['matched_at'], 'power-on tick coverage differs')
    require([{'tick':sample['game_tick']-samples[0]['game_tick'],**event} for sample in samples for event in sample['events']]==power['events'], 'power-on events differ')
    captures=[parse_capture(record['response'],sites) for record in commands if record['command'].startswith('data get storage reda_sequential_20261004:trace snapshot')]
    cursor=0
    for sample in ticks:
        frame={'game_tick':sample['game_tick'],'values':sample['values']}
        while cursor<len(captures) and captures[cursor]!=frame:cursor+=1
        require(cursor<len(captures), 'tick snapshot missing from raw RCON capture')
        cursor+=1
    steps=[record for record in commands if record['command']=='tick step 1']
    require(len(steps)==ticks[-1]['game_tick']-ticks[0]['game_tick'] and all('Stepping 1 tick' in record['response'] for record in steps), 'game-step acknowledgement/coverage differs')
    cleanup_index=next((i for i,record in enumerate(commands) if record['command']=='tick unfreeze'),None)
    require(cleanup_index is not None, 'no cleanup boundary')
    cleanup=commands[cleanup_index:]
    prefixes={'tick unfreeze':('The game is running normally',),'fill ':('Successfully filled ','No blocks were filled'),
              'forceload remove ':('Unmarked ',),'datapack disable ':('Disabling data pack ',),
              'data remove storage ':('Removed ',),'reload':('Reloading!',)}
    found=set()
    cleanup_order=[]
    for record in cleanup:
        key=next((key for key in prefixes if record['command'].startswith(key)),None)
        exact_storage_success=record['command']==storage_command and record['response']==storage_response
        require(key is not None and (record['response'].strip().startswith(prefixes[key]) or exact_storage_success), 'cleanup command rejected or unrecognised: '+record['command'])
        found.add(key)
        if not cleanup_order or cleanup_order[-1]!=key:cleanup_order.append(key)
    require(found==set(prefixes), 'incomplete cleanup commands')
    require(cleanup_order==list(prefixes), 'cleanup command order differs')
    if adjudication:
        require(sum(record['command']==storage_command and record['response']==storage_response for record in cleanup)==1, 'cleanup false-positive does not have one exact raw acknowledgement')
    disable=next(record['command'] for record in cleanup if record['command'].startswith('datapack disable '))
    pack_name=json.loads(disable.removeprefix('datapack disable ')).removeprefix('file/')
    jar=next(Path(name) for name in report['source_sha256'] if name.endswith('/server.jar'))
    require(not (jar.parent/'world/datapacks'/pack_name).exists(), 'owned datapack directory remains')
    return {'status':'qualified_with_driver_cleanup_ack_false_positive' if adjudication else 'pass',
            'original_report_errors':errors,'adjudication':'only the exact actual storage acknowledgement was misclassified; original report retained' if adjudication else None,
            'source_resolutions':source_resolutions,'phases':264,'sites':27,'values_and_locks':len(targets[0]),'tick_frames':len(ticks),
            'game_ticks':len(steps),'raw_captures':len(captures),'cleanup_commands':len(cleanup),
            'observation':'game-tick boundaries; not intra-tick events or simulator heap emptiness',
            'evidence_sha256':{str(path):hashlib.sha256(path.read_bytes()).hexdigest() for path in [directory/'report.json',directory/'ticks.jsonl.gz',directory/'commands.jsonl.gz',reference_path,Path(__file__)]}}


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run',type=Path,required=True)
    parser.add_argument('--reference',type=Path,default=Path('docs/mixed-clock-reset-feedback-2026-10-04/repeat1.log.gz'))
    parser.add_argument('--source-snapshot',type=Path)
    parser.add_argument('--source-manifest',type=Path)
    arguments=parser.parse_args()
    print(json.dumps(audit(arguments.run,arguments.reference,arguments.source_snapshot,arguments.source_manifest),indent=2))

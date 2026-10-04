#!/usr/bin/env python3
"""Tick-boundary native mixed-clock reset/feedback conformance in real Minecraft.

A frozen world and native datapack capture every observed site atomically.
The stock server exposes no scheduled-event heap: settlement means complete
oracle target match, followed by the unchanged sixteen-game-tick Design H dwell.
All intermediate tick snapshots, including pulses and lock changes, are retained.
"""
from __future__ import annotations
import argparse
import gzip
import hashlib
import json
import re
import shutil
import time
import zipfile
from dataclasses import dataclass
from pathlib import Path

from circuit_conformance import (
    CircuitMismatch, RegionNotReady, block_id_present, build_phase_commands,
    clear_region, evaluate_expected, expected_block_at, forceload, forceload_release,
    lever_state_string, parse_dump, read_lamp, read_merge_junction, read_powered_block,
    read_properties, read_torch, verify_positions_placed, verify_region_is_air,
    wait_for_region_loaded,
)
from rcon import RconClient

NAMESPACE = 'reda_sequential_20261004'
STORAGE = NAMESPACE + ':trace'
ROLES = {'MData', 'SData', 'MLock', 'SLock', 'InvC'}
# Bound to the qualified native graph and output bindings retained in
# docs/mixed-clock-reset-feedback-2026-10-04/repeat1.log.gz. Physical placement
# may vary; this dedicated literal oracle must never accept different logic.
QUALIFIED_GRAPH_SHA256 = 'b5326c31cbad10b4fedc5474a3f38d544dbd293b3454b9575123b343b6ef1b30'


def mixed_clock_graph_signature(dump, public):
    graph = {'gates':sorted((gate.name,gate.kind,list(gate.inputs)) for gate in dump.gates),
             'outputs':sorted(public.items())}
    return hashlib.sha256(json.dumps(graph,separators=(',',':')).encode()).hexdigest()


@dataclass(frozen=True)
class Site:
    label: str
    position: tuple[int, int, int]
    block_id: str


def make_sites(dump, origin):
    def absolute(position):
        return tuple(a+b for a,b in zip(origin, position))
    gates = {gate.name: gate for gate in dump.gates}
    sites = [Site('gate:'+name, absolute(pos), expected_block_at(gates[name]))
             for name,pos in dump.gate_outputs.items()]
    sites += [Site('lamp:'+dump.output_display_name(name), absolute(pos), 'minecraft:redstone_lamp')
              for name,pos in dump.outputs.items()]
    ids = {'Repeater':'minecraft:repeater','Torch':'minecraft:redstone_torch',
           'WallTorch':'minecraft:redstone_wall_torch'}
    for gate in dump.gates:
        if gate.kind == 'dff_p':
            if {role for name,role in dump.sequential_cells if name == gate.name} != ROLES:
                raise ValueError('each DFF needs all five physical cell observations')
    sites += [Site('cell:'+name+':'+role, absolute(pos), ids[component])
              for (name,role),(pos,component) in sorted(dump.sequential_cells.items())]
    if len({site.label for site in sites}) != len(sites):
        raise ValueError('observation labels must be unique')
    return sites


def property_for(site):
    if site.block_id == 'minecraft:redstone_wire':
        return 'power=0'
    return ('powered' if site.block_id in ('minecraft:repeater','minecraft:comparator') else 'lit') + '=true'


def capture_commands(sites):
    commands = [f'data modify storage {STORAGE} set value {{}}',
                f'execute store result storage {STORAGE} t0 int 1 run time query gametime']
    for index,site in enumerate(sites):
        position = ' '.join(map(str,site.position))
        base = f'execute if block {position} {site.block_id}'
        for suffix,predicate in [('p',base), ('v',base+'['+property_for(site)+']')]:
            commands.append(f'execute store success storage {STORAGE} s{index}{suffix} int 1 run {predicate}')
        if site.block_id == 'minecraft:repeater':
            commands.append(f'execute store success storage {STORAGE} s{index}l int 1 run {base}[locked=true]')
    commands.append(f'execute store result storage {STORAGE} t1 int 1 run time query gametime')
    return commands


def parse_snapshot(response, sites):
    fields = {name:int(value) for name,value in re.findall(r'\b(t[01]|s\d+[pvl]):\s*(-?\d+)\b',response)}
    required = {'t0','t1'} | {f's{i}{suffix}' for i,site in enumerate(sites)
                for suffix in ('p','v','l') if suffix != 'l' or site.block_id == 'minecraft:repeater'}
    if set(fields) != required or fields['t0'] != fields['t1']:
        raise RegionNotReady('snapshot missing fields or spans different game ticks')
    values = {}
    for index,site in enumerate(sites):
        if fields[f's{index}p'] != 1:
            raise CircuitMismatch('observed block disappeared: '+site.label)
        value = fields[f's{index}v']
        if value not in (0,1):
            raise RegionNotReady('snapshot Boolean is not zero or one')
        if site.block_id == 'minecraft:redstone_wire':
            value = 1-value
        values[site.label] = bool(value)
        if site.block_id == 'minecraft:repeater':
            lock = fields[f's{index}l']
            if lock not in (0,1):
                raise RegionNotReady('snapshot lock is not zero or one')
            values[site.label+':locked'] = bool(lock)
    return {'game_tick': fields['t0'], 'values':values}


def read_site(client, site):
    if site.block_id == 'minecraft:redstone_wire': return read_merge_junction(client,site.position)
    if site.block_id == 'minecraft:redstone_lamp': return read_lamp(client,site.position)
    if site.block_id in ('minecraft:repeater','minecraft:comparator'):
        return read_powered_block(client,site.position,site.block_id)
    return read_torch(client,site.position,site.block_id)


def read_locked(client,site):
    position = ' '.join(map(str,site.position))
    for value in (True,False):
        if client.command(f'execute if block {position} minecraft:repeater[locked={str(value).lower()}]').strip().startswith('Test passed'):
            return value
    return None


def protocol():
    for control in range(8):
        for clocks in [0,1,3,2,0,3,0,2,3,1,0]:
            for changed in [control,control ^ 7,control]:
                yield {'clk_a': bool(clocks & 1), 'clk_b': bool(clocks & 2),
                       'reset_a':bool(changed & 1),'reset_b':bool(changed & 2), 'en':bool(changed & 4)}


class MixedClockOracle:
    def __init__(self,dump):
        if set(dump.inputs) != {'clk_a','clk_b','reset_a','reset_b','en'}:
            raise ValueError('mixed-clock protocol needs its exact five source inputs')
        self.dump = dump
        self.public = {dump.output_display_name(name):dump.output_bindings.get(name,name) for name in dump.outputs}
        if set(self.public) != {'q_a','q_b'}:
            raise ValueError('mixed-clock protocol needs both source output labels')
        if mixed_clock_graph_signature(dump,self.public) != QUALIFIED_GRAPH_SHA256:
            raise ValueError('mixed-clock literal oracle requires the qualified lowered graph and output bindings')
        self.dffs = [gate for gate in dump.gates if gate.kind == 'dff_p']
        if {gate.name for gate in self.dffs} != set(self.public.values()):
            raise ValueError('both source states must be actual observed DFFs')
        self.state = [False,False]
        self.clocks = [False,False]
        self.primary = {name:False for name in dump.inputs}

    def transition(self,inputs):
        previous = self.state.copy()
        next_clocks = [inputs['clk_a'],inputs['clk_b']]
        if not self.clocks[0] and next_clocks[0]:
            self.state[0] = False if inputs['reset_a'] else (not previous[0] if inputs['en'] else previous[0])
        if self.clocks[1] and not next_clocks[1]:
            self.state[1] = False if inputs['reset_b'] else previous[0]
        self.clocks = next_clocks
        self.primary = dict(inputs)
        return self.expected()

    def expected(self):
        primary = dict(self.primary)
        primary.update({self.public[name]:self.state[bit] for bit,name in enumerate(('q_a','q_b'))})
        pending = [gate for gate in self.dump.gates if gate.kind != 'dff_p']
        known = set(primary)
        order = []
        while pending:
            ready = [gate for gate in pending if set(gate.inputs) <= known]
            if not ready: raise ValueError('unknown input or combinational cycle in dump')
            for gate in ready:
                pending.remove(gate); order.append(gate); known.add(gate.name)
        values = evaluate_expected(order,primary)
        expected = {'gate:'+gate.name:bool(values[gate.name]) for gate in self.dump.gates}
        expected.update({'lamp:'+name:self.state[bit] for bit,name in enumerate(('q_a','q_b'))})
        for gate in self.dffs:
            clock = bool(values[gate.inputs[1]])
            expected['gate:'+gate.name+':locked'] = not clock
            roles = {'MData':bool(values[gate.name]) if clock else bool(values[gate.inputs[0]]),
                     'SData':bool(values[gate.name]), 'MLock':clock,'SLock':not clock,'InvC':not clock}
            for role,value in roles.items():
                label = 'cell:'+gate.name+':'+role
                expected[label] = value
                if role != 'InvC':
                    expected[label+':locked'] = clock if role == 'MData' else not clock if role == 'SData' else False
        return expected


class TickCapture:
    def __init__(self,client,sites):
        self.client,self.sites = client,sites
        self.current = self.capture()

    def capture(self):
        self.client.command('function '+NAMESPACE+':capture')
        return parse_snapshot(self.client.command('data get storage '+STORAGE),self.sites)

    def step(self):
        target = self.current['game_tick']+1
        response = self.client.command('tick step 1')
        if 'Stepping 1 tick' not in response: raise RegionNotReady('tick step was not acknowledged')
        deadline = time.monotonic()+10
        while True:
            sample = self.capture()
            if sample['game_tick'] == target:
                self.current = sample
                return sample
            if sample['game_tick'] != target-1 or time.monotonic() >= deadline:
                raise RegionNotReady('tick-step sampling lost exclusive tick control')
            time.sleep(0.005)


def prepare_pack(directory,jar,sites):
    with zipfile.ZipFile(jar) as archive:
        version = json.loads(archive.read('version.json'))
    pack = version['pack_version']
    directory.mkdir(parents=True,exist_ok=False)
    try:
        (directory/'pack.mcmeta').write_text(json.dumps({'pack':{'description':'REDA temporary frozen-tick sequential observation',
            'min_format':[pack['data_major'],pack['data_minor']], 'max_format':pack['data_major']}},indent=2)+'\n')
        path = directory/'data'/NAMESPACE/'function'/'capture.mcfunction'
        path.parent.mkdir(parents=True,exist_ok=True)
        path.write_text('\n'.join(capture_commands(sites))+'\n')
    except Exception:
        shutil.rmtree(directory)
        raise
    return version


def run(args):
    directory = args.out
    report = {'protocol':'mixed-clock-reset-feedback/v1','status':'running','phases':[]}
    client = raw = log = None
    out_owned = pack_owned = pack_loaded = loaded = frozen = False
    previous = None
    try:
        directory.mkdir(parents=True,exist_ok=False)
        out_owned = True
        dump = parse_dump(args.dump)
        origin = tuple(map(int,args.origin.split(',')))
        if len(origin) != 3: raise ValueError('origin must have exactly three coordinates')
        sites = make_sites(dump,origin)
        oracle = MixedClockOracle(dump)
        report.update({'origin':origin,'sites':[site.__dict__ for site in sites],
            'settle_cap_game_ticks':2000,'dwell_game_ticks':16,
            'settlement_criterion':'full oracle target match, then existing 16 tick dwell with final target match',
            'observation':'every actual game-tick boundary, frozen atomic capture; not intra-tick transitions',
            'source_sha256':{str(path):hashlib.sha256(path.read_bytes()).hexdigest() for path in
                (args.dump,Path(__file__),Path(__file__).with_name('circuit_conformance.py'),args.jar)}})
        report['minecraft'] = prepare_pack(args.pack,args.jar,sites)
        pack_owned = True
        shutil.copytree(args.pack,directory/'snapshot-pack')
        report['datapack_sha256'] = {str(path.relative_to(args.pack)):hashlib.sha256(path.read_bytes()).hexdigest()
                                    for path in args.pack.rglob('*') if path.is_file()}
        raw = gzip.open(directory/'ticks.jsonl.gz','wt')
        log = gzip.open(directory/'commands.jsonl.gz','wt')
        props = read_properties(args.properties)
        client = RconClient(args.host,int(props.get('rcon.port','25575')),props['rcon.password'])
        original_command = client.command
        def recorded(command):
            response = original_command(command)
            log.write(json.dumps({'command':command,'response':response})+'\n'); log.flush()
            return response
        client.command = recorded
        client.connect()
        client.command('reload'); pack_loaded = True
        client.command('tick freeze'); frozen = True
        forceload(client,origin,dump.size); loaded = True
        wait_for_region_loaded(client,origin,dump.size,timeout=60)
        clear_region(client,origin,dump.size)
        verify_region_is_air(client,origin,dump.size)
        for command in build_phase_commands([block for block in dump.blocks if block.kind != 'Lever'],origin):
            client.command(command)
        levers = {name:next(block for block in dump.blocks if block.kind == 'Lever' and (block.x,block.y,block.z)==pos)
                  for name,pos in dump.inputs.items()}
        def set_inputs(inputs):
            for name,value in inputs.items():
                block = levers[name]
                position = ' '.join(str(a+b) for a,b in zip(origin,(block.x,block.y,block.z)))
                client.command(f'setblock {position} {lever_state_string(block,value)} replace')
        set_inputs(oracle.primary)
        missing = verify_positions_placed(client,[(site.position,site.block_id) for site in sites])
        if missing: raise RegionNotReady('initial placement failed: '+'; '.join(missing))
        ticks = TickCapture(client,sites)
        parity = {}
        for site in sites:
            value = read_site(client,site)
            if value is None or bool(value) != ticks.current['values'][site.label]:
                raise RegionNotReady('batch/direct reader value mismatch: '+site.label)
            parity[site.label] = bool(value)
            if site.block_id == 'minecraft:repeater':
                locked = read_locked(client,site)
                if locked is None or locked != ticks.current['values'][site.label+':locked']:
                    raise RegionNotReady('batch/direct lock reader mismatch: '+site.label)
                parity[site.label+':locked'] = locked
        if ticks.capture() != ticks.current: raise RegionNotReady('world advanced during frozen reader parity')
        report['batch_direct_reader_parity'] = parity
        previous = ticks.current
        def observe(phase, sample):
            nonlocal previous
            events = [{'label':name,'value':value} for name,value in sample['values'].items()
                      if previous['values'][name] != value]
            raw.write(json.dumps({'phase':phase,**sample,'events':events})+'\n');raw.flush()
            previous = sample
            return events
        observe('power_on',ticks.current)
        for phase,inputs in [('power_on',dict(oracle.primary)), *enumerate(protocol())]:
            expected = oracle.expected() if phase == 'power_on' else oracle.transition(inputs)
            set_inputs(inputs)
            start = ticks.current['game_tick']
            events = []
            after_inputs = ticks.capture()
            if after_inputs['game_tick'] != ticks.current['game_tick']:
                raise RegionNotReady('inputs advanced a frozen game tick')
            ticks.current = after_inputs
            events.extend({'tick':0,**event} for event in observe(phase,after_inputs))
            report['active_phase'] = {'phase':phase,'inputs':inputs,'expected_state':oracle.state.copy(),
                                      'expected':expected,'start_game_tick':start}
            for _ in range(2000):
                if ticks.current['values'] == expected: break
                sample = ticks.step(); events.extend({'tick':sample['game_tick']-start,**event} for event in observe(phase,sample))
            if ticks.current['values'] != expected:
                report['failure_snapshot'] = ticks.current
                raise CircuitMismatch('settlement cap reached at phase '+str(phase))
            matched_at = ticks.current['game_tick']-start
            if phase != 'power_on':
                for _ in range(16):
                    sample=ticks.step();events.extend({'tick':sample['game_tick']-start,**event} for event in observe(phase,sample))
            if ticks.current['values'] != expected:
                wrong={name:{'expected':value,'actual':ticks.current['values'][name]} for name,value in expected.items() if ticks.current['values'][name]!=value}
                raise CircuitMismatch('final state mismatch at phase '+str(phase)+': '+json.dumps(wrong))
            record={'phase':phase,'inputs':inputs,'expected_state':oracle.state.copy(),'matched_at':matched_at,
                'ticks':ticks.current['game_tick']-start,'final':ticks.current['values'],'events':events}
            if phase == 'power_on': report['power_on']=record
            else: report['phases'].append(record)
            (directory/'report.json').write_text(json.dumps(report,indent=2)+'\n')
            print(json.dumps({'phase':phase,'ticks':record['ticks'],'events':len(events)}),flush=True)
        report.pop('active_phase',None)
        report['status']='pass'
    except Exception as error:
        report['status']='fail';report['error_type']=type(error).__name__;report['error']=str(error)
        raise
    finally:
        if frozen:
            try: client.command('tick unfreeze')
            except Exception as error: report['unfreeze_error']=str(error)
        if loaded:
            try:
                clear_region(client,origin,dump.size);forceload_release(client,origin,dump.size)
            except Exception as error: report['cleanup_error']=str(error)
        if pack_owned:
            if pack_loaded and client is not None:
                try:
                    client.command('datapack disable '+json.dumps('file/'+args.pack.name))
                    client.command('data remove storage '+STORAGE)
                except Exception as error: report['datapack_unload_error']=str(error)
            try: shutil.rmtree(args.pack)
            except Exception as error: report['datapack_remove_error']=str(error)
            if pack_loaded and client is not None:
                try: client.command('reload')
                except Exception as error: report['datapack_reload_error']=str(error)
        if client is not None: client.close()
        if raw is not None: raw.close()
        if log is not None: log.close()
        if out_owned: (directory/'report.json').write_text(json.dumps(report,indent=2)+'\n')



if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dump',type=Path,required=True)
    parser.add_argument('--properties',type=Path,required=True)
    parser.add_argument('--jar',type=Path,required=True)
    parser.add_argument('--pack',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    parser.add_argument('--origin',default='230000,151,100000')
    parser.add_argument('--host',default='127.0.0.1')
    run(parser.parse_args())

#!/usr/bin/env python3
"""Tick-boundary native FSM/counter conformance in real Minecraft.

General FSM/counter fixtures (`feedback_fsm`, `reset_fsm`, `reset_counter`,
`feedback_counter`, `conditional_state_enable`, `feedback_state_enable`,
`negedge_counter`) are driven by a literal state oracle that encodes each
fixture's source semantics -- the case table, the counter arithmetic, the
enable condition, and the clock edge -- bound to the qualified lowered graph
and output bindings of the fixture's own dump. A mutant that keeps the same
input, DFF, and public names but changes data logic is rejected.

Input changes occur in a frozen game tick. Native datapack snapshots cover
each sampled game-tick boundary atomically. Settlement is full observation
agreement within the existing 2000-game-tick cap, followed by the existing
16-game-tick dwell and final agreement. All intermediate tick snapshots,
including pulses and lock changes, are retained.
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

if __package__:
    from .circuit_conformance import (
        CircuitMismatch, RegionNotReady, build_phase_commands,
        clear_region, evaluate_expected, expected_block_at, forceload, forceload_release,
        lever_state_string, parse_dump, read_lamp, read_merge_junction, read_powered_block,
        read_properties, read_torch, verify_positions_placed, verify_region_is_air,
        wait_for_region_loaded,
    )
    from .rcon import RconClient
else:
    from circuit_conformance import (
        CircuitMismatch, RegionNotReady, build_phase_commands,
        clear_region, evaluate_expected, expected_block_at, forceload, forceload_release,
        lever_state_string, parse_dump, read_lamp, read_merge_junction, read_powered_block,
        read_properties, read_torch, verify_positions_placed, verify_region_is_air,
        wait_for_region_loaded,
    )
    from rcon import RconClient

NAMESPACE = 'reda_fsm_counter_20261015'
STORAGE = NAMESPACE + ':trace'
SNAPSHOT_PATH = 'snapshot'
ROLES = {'MData', 'SData', 'MLock', 'SLock', 'InvC'}

# Bound to the qualified lowered graph and output bindings of each fixture,
# produced by mc_dump from the committed source. Physical placement may vary;
# this dedicated literal oracle must never accept different logic.
QUALIFIED_GRAPH_SHA256 = {
    'feedback_fsm': '0dd54a6dfca9bd7d1af00bc4a3115b739cf239eec05503f0b55ab63d0c55218d',
    'reset_fsm': '0dd54a6dfca9bd7d1af00bc4a3115b739cf239eec05503f0b55ab63d0c55218d',
    'reset_counter': 'ad04cbecd7305aa8fbdd7c63a05801b74d63c068c8ec6fc978459060b46b7cc1',
    'feedback_counter': 'ad04cbecd7305aa8fbdd7c63a05801b74d63c068c8ec6fc978459060b46b7cc1',
    'conditional_state_enable': 'de21f1e0d86c6a214d5b876e860a82e87eb1cacbae06d1c9c8e354bd5447b56d',
    'feedback_state_enable': 'de21f1e0d86c6a214d5b876e860a82e87eb1cacbae06d1c9c8e354bd5447b56d',
    'negedge_counter': '3cff5ff4e81d5a7b707f4fdfc25c19a926d5aabfa65780814aec6236cc9daf59',
}

# Literal source semantics, one entry per fixture, transcribed from
# tests/fixtures/<name>.sv. `pins` are the two control inputs in source
# order (control bit 0 -> pins[0], bit 1 -> pins[1]); `edge` is the clock
# edge the always_ff block reacts to; `width` is the state bit count.
FIXTURES = {
    'feedback_fsm': {'pins': ('reset', 'advance'), 'edge': 'pos', 'width': 2,
                     'next': lambda q, reset, advance: 0 if reset else ([1, 2, 0, 0][q] if advance else q)},
    'reset_fsm': {'pins': ('reset', 'advance'), 'edge': 'pos', 'width': 2,
                  'next': lambda q, reset, advance: 0 if reset else ([1, 2, 0, 0][q] if advance else q)},
    'reset_counter': {'pins': ('reset', 'en'), 'edge': 'pos', 'width': 2,
                      'next': lambda q, reset, en: 0 if reset else ((q + 1) % 4 if en else q)},
    'feedback_counter': {'pins': ('reset', 'en'), 'edge': 'pos', 'width': 2,
                         'next': lambda q, reset, en: 0 if reset else ((q + 1) % 4 if en else q)},
    'conditional_state_enable': {'pins': ('d', 'en'), 'edge': 'pos', 'width': 1,
                                 'next': lambda q, d, en: d if q or en else q},
    'feedback_state_enable': {'pins': ('d', 'en'), 'edge': 'pos', 'width': 1,
                              'next': lambda q, d, en: d if q or en else q},
    'negedge_counter': {'pins': ('reset', 'en'), 'edge': 'neg', 'width': 2,
                        'next': lambda q, reset, en: 0 if reset else ((q + 1) % 4 if en else q)},
}


def graph_signature(dump, public):
    graph = {'gates': sorted((gate.name, gate.kind, list(gate.inputs)) for gate in dump.gates),
             'outputs': sorted(public.items())}
    return hashlib.sha256(json.dumps(graph, separators=(',', ':')).encode()).hexdigest()


@dataclass(frozen=True)
class Site:
    label: str
    position: tuple[int, int, int]
    block_id: str


def make_sites(dump, origin):
    def absolute(position):
        return tuple(a + b for a, b in zip(origin, position))
    gates = {gate.name: gate for gate in dump.gates}
    sites = [Site('gate:' + name, absolute(pos), expected_block_at(gates[name]))
             for name, pos in dump.gate_outputs.items()]
    sites += [Site('lamp:' + dump.output_display_name(name), absolute(pos), 'minecraft:redstone_lamp')
              for name, pos in dump.outputs.items()]
    ids = {'Repeater': 'minecraft:repeater', 'Torch': 'minecraft:redstone_torch',
           'WallTorch': 'minecraft:redstone_wall_torch'}
    for gate in dump.gates:
        if gate.kind == 'dff_p':
            if {role for name, role in dump.sequential_cells if name == gate.name} != ROLES:
                raise ValueError('each DFF needs all five physical cell observations')
    sites += [Site('cell:' + name + ':' + role, absolute(pos), ids[component])
              for (name, role), (pos, component) in sorted(dump.sequential_cells.items())]
    if len({site.label for site in sites}) != len(sites):
        raise ValueError('observation labels must be unique')
    return sites


def property_for(site):
    if site.block_id == 'minecraft:redstone_wire':
        return 'power=0'
    return ('powered' if site.block_id in ('minecraft:repeater', 'minecraft:comparator') else 'lit') + '=true'


def capture_commands(sites):
    commands = [f'data modify storage {STORAGE} {SNAPSHOT_PATH} set value {{}}',
                f'execute store result storage {STORAGE} {SNAPSHOT_PATH}.t0 int 1 run time query gametime']
    for index, site in enumerate(sites):
        position = ' '.join(map(str, site.position))
        base = f'execute if block {position} {site.block_id}'
        for suffix, predicate in [('p', base), ('v', base + '[' + property_for(site) + ']')]:
            commands.append(f'execute store success storage {STORAGE} {SNAPSHOT_PATH}.s{index}{suffix} int 1 run {predicate}')
        if site.block_id == 'minecraft:repeater':
            commands.append(f'execute store success storage {STORAGE} {SNAPSHOT_PATH}.s{index}l int 1 run {base}[locked=true]')
    commands.append(f'execute store result storage {STORAGE} {SNAPSHOT_PATH}.t1 int 1 run time query gametime')
    return commands


def parse_snapshot(response, sites):
    fields = {name: int(value) for name, value in re.findall(r'\b(t[01]|s\d+[pvl]):\s*(-?\d+)\b', response)}
    required = {'t0', 't1'} | {f's{i}{suffix}' for i, site in enumerate(sites)
                               for suffix in ('p', 'v', 'l') if suffix != 'l' or site.block_id == 'minecraft:repeater'}
    if set(fields) != required or fields['t0'] != fields['t1']:
        raise RegionNotReady('snapshot missing fields or spans different game ticks')
    values = {}
    for index, site in enumerate(sites):
        if fields[f's{index}p'] != 1:
            raise CircuitMismatch('observed block disappeared: ' + site.label)
        value = fields[f's{index}v']
        if value not in (0, 1):
            raise RegionNotReady('snapshot Boolean is not zero or one')
        if site.block_id == 'minecraft:redstone_wire':
            value = 1 - value
        values[site.label] = bool(value)
        if site.block_id == 'minecraft:repeater':
            lock = fields[f's{index}l']
            if lock not in (0, 1):
                raise RegionNotReady('snapshot lock is not zero or one')
            values[site.label + ':locked'] = bool(lock)
    return {'game_tick': fields['t0'], 'values': values}


def read_site(client, site):
    if site.block_id == 'minecraft:redstone_wire': return read_merge_junction(client, site.position)
    if site.block_id == 'minecraft:redstone_lamp': return read_lamp(client, site.position)
    if site.block_id in ('minecraft:repeater', 'minecraft:comparator'):
        return read_powered_block(client, site.position, site.block_id)
    return read_torch(client, site.position, site.block_id)


def read_locked(client, site):
    position = ' '.join(map(str, site.position))
    for value in (True, False):
        if client.command(f'execute if block {position} minecraft:repeater[locked={str(value).lower()}]').strip().startswith('Test passed'):
            return value
    return None


def protocol(fixture):
    """The frozen producer protocol for the fixture: 8 cycles of a control
    word, four sub-steps per cycle, matching tests/feedback_physical.rs and
    tests/negedge_physical.rs exactly so the game trajectory is comparable
    to the frozen Rust reference phase by phase."""
    spec = FIXTURES[fixture]
    controls = [3, 0, 1, 2, 3, 1, 0, 1] if spec['pins'] == ('d', 'en') else [1, 2, 2, 2, 2, 0, 3, 2]
    for cycle in range(8):
        control = controls[cycle]
        substeps = [(control, False), (control, True), (control ^ 3, True), (control ^ 3, False)] \
            if spec['edge'] == 'pos' else \
            [(control ^ 3, False), (control ^ 3, True), (control, True), (control, False)]
        for control, clk in substeps:
            yield {'clk': clk, spec['pins'][0]: bool(control & 1), spec['pins'][1]: bool(control & 2)}


class FsmCounterOracle:
    def __init__(self, dump, fixture):
        spec = FIXTURES[fixture]
        self.fixture = fixture
        self.dump = dump
        self.pins = spec['pins']
        self.edge = spec['edge']
        self.width = spec['width']
        if set(dump.inputs) != {'clk', *self.pins}:
            raise ValueError('fixture protocol needs its exact source inputs')
        self.public = {dump.output_display_name(name): dump.output_bindings.get(name, name)
                       for name in dump.outputs}
        labels = [f'q[{bit}]' for bit in range(self.width)] if self.width > 1 else ['q']
        if sorted(self.public) != sorted(labels):
            raise ValueError('fixture protocol needs all source output labels')
        if graph_signature(dump, self.public) != QUALIFIED_GRAPH_SHA256[fixture]:
            raise ValueError('literal oracle requires the qualified lowered graph and output bindings')
        self.dffs = [gate for gate in dump.gates if gate.kind == 'dff_p']
        # State bit i is the DFF bound to public label q[i] (or q for 1-bit).
        self.state_dffs = [self.public[label] for label in labels]
        if set(self.state_dffs) != {gate.name for gate in self.dffs}:
            raise ValueError('every source state must be an actual observed DFF')
        self.state = [False] * self.width
        self.clock = False
        self.primary = {name: False for name in dump.inputs}

    def transition(self, inputs):
        clock = inputs['clk']
        if (self.edge == 'pos' and not self.clock and clock) or (self.edge == 'neg' and self.clock and not clock):
            q = sum(bit << index for index, bit in enumerate(self.state))
            q = FIXTURES[self.fixture]['next'](q, inputs[self.pins[0]], inputs[self.pins[1]])
            self.state = [(q >> index) & 1 == 1 for index in range(self.width)]
        self.clock = clock
        self.primary = dict(inputs)
        return self.expected()

    def seed_state(self, values):
        """The fixture source leaves the power-up state unspecified (no
        initial value in the always_ff); the qualified Rust physical control
        reads the power-on state from the loaded world rather than assuming
        one (tests/feedback_physical.rs power_on capture). The game control
        does the same: observe the settled physical state and seed the
        literal oracle with it. The transition function stays literal."""
        for index, dff in enumerate(self.state_dffs):
            self.state[index] = bool(values['gate:' + dff])

    def expected(self):
        primary = dict(self.primary)
        primary.update({dff: self.state[index] for index, dff in enumerate(self.state_dffs)})
        pending = [gate for gate in self.dump_gates() if gate.kind != 'dff_p']
        known = set(primary)
        order = []
        while pending:
            ready = [gate for gate in pending if set(gate.inputs) <= known]
            if not ready: raise ValueError('unknown input or combinational cycle in dump')
            for gate in ready:
                pending.remove(gate); order.append(gate); known.add(gate.name)
        values = evaluate_expected(order, primary)
        expected = {'gate:' + gate.name: bool(values[gate.name]) for gate in self.dump_gates()}
        labels = [f'q[{bit}]' for bit in range(self.width)] if self.width > 1 else ['q']
        for bit, label in enumerate(labels):
            expected['lamp:' + label] = self.state[bit]
        for gate in self.dffs:
            clock = bool(values[gate.inputs[1]])
            expected['gate:' + gate.name + ':locked'] = not clock
            roles = {'MData': bool(values[gate.name]) if clock else bool(values[gate.inputs[0]]),
                     'SData': bool(values[gate.name]), 'MLock': clock, 'SLock': not clock, 'InvC': not clock}
            for role, value in roles.items():
                label = 'cell:' + gate.name + ':' + role
                expected[label] = value
                if role != 'InvC':
                    expected[label + ':locked'] = clock if role == 'MData' else not clock if role == 'SData' else False
        return expected

    def dump_gates(self):
        return self.dump.gates


def cleanup_response_matches(command, response, prefixes):
    response = response.strip()
    return response.startswith(prefixes) or (
        command == 'data remove storage ' + STORAGE + ' ' + SNAPSHOT_PATH
        and response == 'Modified storage ' + STORAGE)


class TickCapture:
    def __init__(self, client, sites):
        self.client, self.sites = client, sites
        self.current = self.capture()

    def capture(self):
        response = self.client.command('function ' + NAMESPACE + ':capture')
        if response.strip() != 'Running function ' + NAMESPACE + ':capture' and not response.strip().startswith('Executed '):
            raise RegionNotReady('native snapshot function was not executed: ' + response)
        return parse_snapshot(self.client.command('data get storage ' + STORAGE + ' ' + SNAPSHOT_PATH), self.sites)

    def step(self):
        target = self.current['game_tick'] + 1
        response = self.client.command('tick step 1')
        if 'Stepping 1 tick' not in response: raise RegionNotReady('tick step was not acknowledged')
        deadline = time.monotonic() + 10
        while True:
            sample = self.capture()
            if sample['game_tick'] == target:
                self.current = sample
                return sample
            if sample['game_tick'] != target - 1 or time.monotonic() >= deadline:
                raise RegionNotReady('tick-step sampling lost exclusive tick control')
            time.sleep(0.005)


def prepare_pack(directory, jar, sites):
    with zipfile.ZipFile(jar) as archive:
        version = json.loads(archive.read('version.json'))
    pack = version['pack_version']
    directory.mkdir(parents=True, exist_ok=False)
    try:
        (directory / 'pack.mcmeta').write_text(json.dumps({'pack': {'description': 'REDA temporary frozen-tick FSM/counter observation',
            'min_format': [pack['data_major'], pack['data_minor']], 'max_format': pack['data_major']}}, indent=2) + '\n')
        path = directory / 'data' / NAMESPACE / 'function' / 'capture.mcfunction'
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('\n'.join(capture_commands(sites)) + '\n')
    except Exception:
        shutil.rmtree(directory)
        raise
    return version


def run(args):
    directory = args.out
    report = {'protocol': 'fsm-counter/v1', 'fixture': args.fixture, 'status': 'running', 'phases': []}
    client = raw = log = None
    out_owned = pack_owned = pack_loaded = loaded = frozen = False
    cleaning_up = False
    previous = None
    try:
        directory.mkdir(parents=True, exist_ok=False)
        out_owned = True
        dump = parse_dump(args.dump)
        origin = tuple(map(int, args.origin.split(',')))
        if len(origin) != 3: raise ValueError('origin must have exactly three coordinates')
        sites = make_sites(dump, origin)
        oracle = FsmCounterOracle(dump, args.fixture)
        report.update({'origin': origin, 'sites': [site.__dict__ for site in sites],
            'settle_cap_game_ticks': 2000, 'dwell_game_ticks': 16,
            'settlement_criterion': 'power_on: quiescence (16 identical consecutive full snapshots), then the literal oracle is seeded from the observed physical state and full agreement is required; protocol phases: full oracle target match, then existing 16 tick dwell with final target match',
            'observation': 'every actual game-tick boundary, frozen atomic capture; not intra-tick transitions',
            'source_sha256': {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in
                (args.dump, Path(__file__), Path(__file__).with_name('circuit_conformance.py'), args.jar)}})
        report['minecraft'] = prepare_pack(args.pack, args.jar, sites)
        pack_owned = True
        shutil.copytree(args.pack, directory / 'snapshot-pack')
        report['datapack_sha256'] = {str(path.relative_to(args.pack)): hashlib.sha256(path.read_bytes()).hexdigest()
                                      for path in args.pack.rglob('*') if path.is_file()}
        raw = gzip.open(directory / 'ticks.jsonl.gz', 'wt')
        log = gzip.open(directory / 'commands.jsonl.gz', 'wt')
        props = read_properties(args.properties)
        client = RconClient(args.host, int(props.get('rcon.port', '25575')), props['rcon.password'])
        original_command = client.command
        def recorded(command):
            response = original_command(command)
            log.write(json.dumps({'command': command, 'response': response}) + '\n'); log.flush()
            if cleaning_up:
                prefixes = ('Successfully filled ', 'No blocks were filled') if command.startswith('fill ') else (
                    ('Unmarked ',) if command.startswith('forceload remove ') else None)
                if prefixes is not None and not response.strip().startswith(prefixes):
                    raise RegionNotReady('cleanup command was not acknowledged: ' + command + ': ' + response)
            return response
        client.command = recorded
        client.connect()
        client.command('reload'); pack_loaded = True
        client.command('tick freeze'); frozen = True
        forceload(client, origin, dump.size); loaded = True
        wait_for_region_loaded(client, origin, dump.size, timeout=60)
        clear_region(client, origin, dump.size)
        verify_region_is_air(client, origin, dump.size)
        for command in build_phase_commands([block for block in dump.blocks if block.kind != 'Lever'], origin):
            client.command(command)
        levers = {name: next(block for block in dump.blocks if block.kind == 'Lever' and (block.x, block.y, block.z) == pos)
                  for name, pos in dump.inputs.items()}
        def set_inputs(inputs):
            for name, value in inputs.items():
                block = levers[name]
                position = ' '.join(str(a + b) for a, b in zip(origin, (block.x, block.y, block.z)))
                client.command(f'setblock {position} {lever_state_string(block, value)} replace')
        set_inputs(oracle.primary)
        missing = verify_positions_placed(client, [(site.position, site.block_id) for site in sites])
        if missing: raise RegionNotReady('initial placement failed: ' + '; '.join(missing))
        ticks = TickCapture(client, sites)
        parity = {}
        for site in sites:
            value = read_site(client, site)
            if value is None or bool(value) != ticks.current['values'][site.label]:
                raise RegionNotReady('batch/direct reader value mismatch: ' + site.label)
            parity[site.label] = bool(value)
            if site.block_id == 'minecraft:repeater':
                locked = read_locked(client, site)
                if locked is None or locked != ticks.current['values'][site.label + ':locked']:
                    raise RegionNotReady('batch/direct lock reader mismatch: ' + site.label)
                parity[site.label + ':locked'] = locked
        if ticks.capture() != ticks.current: raise RegionNotReady('world advanced during frozen reader parity')
        report['batch_direct_reader_parity'] = parity
        previous = ticks.current
        def observe(phase, sample):
            nonlocal previous
            events = [{'label': name, 'value': value} for name, value in sample['values'].items()
                      if previous['values'][name] != value]
            raw.write(json.dumps({'phase': phase, **sample, 'events': events}) + '\n'); raw.flush()
            previous = sample
            return events
        observe('power_on', ticks.current)
        for phase, inputs in [('power_on', dict(oracle.primary)), *enumerate(protocol(args.fixture))]:
            if phase == 'power_on':
                expected = None
            else:
                expected = oracle.transition(inputs)
            set_inputs(inputs)
            start = ticks.current['game_tick']
            events = []
            after_inputs = ticks.capture()
            if after_inputs['game_tick'] != ticks.current['game_tick']:
                raise RegionNotReady('inputs advanced a frozen game tick')
            ticks.current = after_inputs
            events.extend({'tick': 0, **event} for event in observe(phase, after_inputs))
            if phase == 'power_on':
                prev = None
                stable = 0
                for _ in range(2000):
                    if ticks.current['values'] == prev:
                        stable += 1
                        if stable >= 16:
                            break
                    else:
                        stable = 0
                        prev = ticks.current['values']
                    sample = ticks.step(); events.extend({'tick': sample['game_tick'] - start, **event} for event in observe(phase, sample))
                if stable < 16:
                    report['failure_snapshot'] = ticks.current
                    raise CircuitMismatch('power-on quiescence cap reached')
                oracle.seed_state(ticks.current['values'])
                expected = oracle.expected()
                report['active_phase'] = {'phase': phase, 'inputs': inputs, 'expected_state': oracle.state.copy(),
                                          'expected': expected, 'start_game_tick': start}
            else:
                report['active_phase'] = {'phase': phase, 'inputs': inputs, 'expected_state': oracle.state.copy(),
                                          'expected': expected, 'start_game_tick': start}
                for _ in range(2000):
                    if ticks.current['values'] == expected: break
                    sample = ticks.step(); events.extend({'tick': sample['game_tick'] - start, **event} for event in observe(phase, sample))
            if ticks.current['values'] != expected:
                report['failure_snapshot'] = ticks.current
                raise CircuitMismatch('settlement cap reached at phase ' + str(phase))
            matched_at = ticks.current['game_tick'] - start
            if phase != 'power_on':
                for _ in range(16):
                    sample = ticks.step(); events.extend({'tick': sample['game_tick'] - start, **event} for event in observe(phase, sample))
            if ticks.current['values'] != expected:
                wrong = {name: {'expected': value, 'actual': ticks.current['values'][name]} for name, value in expected.items()
                         if ticks.current['values'][name] != value}
                raise CircuitMismatch('final state mismatch at phase ' + str(phase) + ': ' + json.dumps(wrong))
            record = {'phase': phase, 'inputs': inputs, 'expected_state': oracle.state.copy(), 'matched_at': matched_at,
                      'ticks': ticks.current['game_tick'] - start, 'final': ticks.current['values'], 'events': events}
            if phase == 'power_on': report['power_on'] = record
            else: report['phases'].append(record)
            (directory / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps({'phase': phase, 'ticks': record['ticks'], 'events': len(events)}), flush=True)
        report.pop('active_phase', None)
        report['status'] = 'pass'
    except Exception as error:
        report['status'] = 'fail'; report['error_type'] = type(error).__name__; report['error'] = str(error)
        raise
    finally:
        cleaning_up = True
        def cleanup_command(command, prefixes):
            response = client.command(command)
            if not cleanup_response_matches(command, response, prefixes):
                raise RegionNotReady('cleanup command was not acknowledged: ' + command + ': ' + response)
        if frozen:
            try: cleanup_command('tick unfreeze', ('The game is running normally',))
            except Exception as error: report['unfreeze_error'] = str(error)
        if loaded:
            try:
                clear_region(client, origin, dump.size)
            except Exception as error: report['cleanup_error'] = str(error)
            try: forceload_release(client, origin, dump.size)
            except Exception as error: report['forceload_release_error'] = str(error)
        if pack_owned:
            if pack_loaded and client is not None:
                try:
                    cleanup_command('datapack disable ' + json.dumps('file/' + args.pack.name), ('Disabling data pack ',))
                except Exception as error: report['datapack_unload_error'] = str(error)
                try:
                    cleanup_command('data remove storage ' + STORAGE + ' ' + SNAPSHOT_PATH, ('Removed ',))
                except Exception as error: report['storage_remove_error'] = str(error)
            try: shutil.rmtree(args.pack)
            except Exception as error: report['datapack_remove_error'] = str(error)
            if pack_loaded and client is not None:
                try: cleanup_command('reload', ('Reloading!',))
                except Exception as error: report['datapack_reload_error'] = str(error)
        if client is not None: client.close()
        if raw is not None: raw.close()
        if log is not None: log.close()
        if out_owned: (directory / 'report.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dump', type=Path, required=True)
    parser.add_argument('--fixture', required=True, choices=sorted(FIXTURES))
    parser.add_argument('--properties', type=Path, required=True)
    parser.add_argument('--jar', type=Path, required=True)
    parser.add_argument('--pack', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--origin', default='240000,151,100000')
    parser.add_argument('--host', default='127.0.0.1')
    run(parser.parse_args())

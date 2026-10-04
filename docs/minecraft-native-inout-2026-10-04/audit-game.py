"""Run only after terminal game completion; retain failures and missing coverage."""
from pathlib import Path
import argparse
import gzip
import os
import collections
import hashlib
import importlib.util
import json
import re
import sys

HERE = Path(__file__).resolve().parent
HP = HERE / 'protocol/circuit_conformance.py'
sys.path.insert(0, str(HP.parent))
spec = importlib.util.spec_from_file_location('raw_conformance', HP)
h = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = h
spec.loader.exec_module(h)
d = h.parse_dump(HERE / 'candidate.dump')
t = json.loads((HERE / 'expected-trace.json').read_text())
p = argparse.ArgumentParser()
p.add_argument('--report', type=Path, default=HERE/'final-result.json')
p.add_argument('--raw', type=Path, default=HERE/'final-rcon.jsonl.gz')
p.add_argument('--out', type=Path, default=HERE/'final-independent-audit.json')
p.add_argument('--cleanup', type=Path, default=HERE/'final-cleanup-audit.json')
a = p.parse_args()
def open_text(path):
    return gzip.open(path, 'rt') if path.suffix == '.gz' else path.open()
def content_hash(path, decompress=False):
    digest = hashlib.sha256()
    opener = gzip.open if decompress and path.suffix == '.gz' else open
    with opener(path, 'rb') as stream:
        for chunk in iter(lambda: stream.read(1024*1024), b''):
            digest.update(chunk)
    return digest.hexdigest()
report = json.loads(a.report.read_text())
origin = tuple(report['origin'])
assert origin == (230000, 151, 180000)
assert tuple(report['size']) == d.size
assert len(report['vectors']) == 128
absolute = lambda p: tuple(a+b for a, b in zip(origin, p))
controls = {absolute(p): n for n, p in d.inputs.items()}
sites = {}
def add(p, key):
    sites.setdefault(absolute(p), []).append(key)
for n, p in d.gate_outputs.items(): add(p, ('gate', n))
for n, p in d.outputs.items(): add(p, ('lamp', n))
aliases = {item['actual_signal']: item for item in t['output_aliases']}
injections = {item['signal']: item for item in t['internal_injections']}
inventory = {item['signal']: item for item in t['physical_output_inventory']}
assert len(inventory) == len(t['physical_output_inventory']) == 14
assert len(aliases) == 12 and len(injections) == 2
assert set(aliases).isdisjoint(injections)
assert set(inventory) == set(aliases) | set(injections)
for signal,item in inventory.items():
    if signal in aliases:
        assert item['kind'] == 'Lamp'
        assert item['position'] == aliases[signal]['position']
        assert d.outputs[aliases[signal]['alias']] == tuple(item['position'])
    else:
        assert item['kind'] == 'RedstoneWire'
        assert item['position'] == injections[signal]['position']
        assert signal in d.gate_outputs
        add(item['position'], ('injection', signal))
bus = t['pinout']['inouts'][0]
for i, p in enumerate(bus['channel']): add(p, ('channel', str(i)))
add(bus['readback'], ('read', 'bus'))
add(bus['external_pad'], ('read', 'pad'))
lever_values, zero, observed = {}, {}, {}
entries = 0
releases = []
with open_text(a.raw) as f:
    for line in f:
        entry = json.loads(line)
        entries += 1
        cmd, response = entry['command'], entry['response']
        if cmd.startswith('forceload remove '): releases.append(entry)
        m = re.fullmatch(r'setblock (-?\d+) (-?\d+) (-?\d+) minecraft:lever\[([^]]+)\] replace', cmd)
        if m:
            p = tuple(map(int, m.group(1,2,3)))
            if p in controls:
                power = re.search(r'(?:^|,)powered=(true|false)(?:,|$)', m.group(4))
                assert power
                lever_values[controls[p]] = power.group(1) == 'true'
                zero.clear()
            continue
        if len(lever_values) != 7: continue
        m = re.fullmatch(r'execute if block (-?\d+) (-?\d+) (-?\d+) (minecraft:[a-z_]+)(?:\[([^]]+)\])?', cmd)
        if not m: continue
        p = tuple(map(int,m.group(1,2,3)))
        if p not in sites: continue
        block, properties = m.group(4,5)
        passed = response.strip().startswith('Test passed')
        value = None
        if block == 'minecraft:redstone_wire' and properties == 'power=0':
            zero[p] = passed
            if passed: value = False
        elif block == 'minecraft:redstone_wire' and properties is None:
            if passed and zero.get(p) is False: value = True
        elif passed and properties in ('lit=true', 'powered=true'): value = True
        elif passed and properties in ('lit=false', 'powered=false'): value = False
        if value is not None:
            raw = sum(int(lever_values[n]) << bit for bit,n in enumerate(t['control_order']))
            for key in sites[p]: observed.setdefault(raw, {})[key] = value
assert set(observed) == set(range(128))
expected_keys = {key for keys in sites.values() for key in keys}
counts = collections.Counter()
failures = []
missing_reads = []
missing_injection_reads = []
status_aliases = {a['label'].split(':')[-1]: a['alias'] for a in t['output_aliases'] if not a['label'].startswith('interface:')}
for expected in t['vectors']:
    raw = expected['raw_control_state']
    actual = observed[raw]
    mandatory = {('gate', n) for n in d.gate_outputs} | {('lamp', n) for n in d.outputs}
    assert mandatory <= set(actual), (raw, 'missing gate/lamp observations', mandatory-set(actual))
    for name,value in expected['electrical_observation_expected'].items():
        category = 'gate' if name in d.gate_outputs else 'lamp'
        if actual[(category,name)] != value:
            failures.append({'raw_state':raw,'category':category,'signal':name,'expected':value,'actual':actual[(category,name)]})
    for signal in injections:
        key = ('injection',signal)
        if key not in actual:
            missing_injection_reads.append({'raw_state':raw,'signal':signal,'position':injections[signal]['position']})
        elif actual[key] != expected['electrical_observation_expected'][signal]:
            failures.append({'raw_state':raw,'category':'injection','signal':signal,
                             'expected':expected['electrical_observation_expected'][signal],'actual':actual[key]})
    if not expected_keys <= set(actual):
        missing_bus = {key for key in expected_keys-set(actual) if key[0] != 'injection'}
        if missing_bus: missing_reads.append({'raw_state':raw,'missing':sorted(missing_bus)})
        continue
    read = actual[('read','bus')]
    pad = actual[('read','pad')]
    for role,value,wanted in [('bus_read',read,expected['electrical_read_expected']['bus_readback_and_all_channel_cells']),
                              ('external_pad',pad,expected['electrical_read_expected']['external_pad'])]:
        if value != wanted: failures.append({'raw_state':raw,'category':'read','signal':role,'expected':wanted,'actual':value})
    lamp = lambda role: actual[('lamp', status_aliases[role])]
    coherent = all(actual[('channel',str(i))] == read for i in range(len(bus['channel'])))
    coherent = coherent and lamp('expected_value') == read
    coherent = coherent and pad == lamp('external_expected') == lamp('external_observed')
    checked = 'ProtocolViolation' if not coherent else 'Contention' if lamp('conflict') else 'Floating' if lamp('floating') else 'Value'
    if checked != expected['checked_kind']:
        failures.append({'raw_state':raw,'category':'checked','expected':expected['checked_kind'],'actual':checked})
    if checked == 'Value' and expected['checked_kind'] == 'Value' and read != expected['boolean_source_assertion']:
        failures.append({'raw_state':raw,'category':'source_boolean','expected':expected['boolean_source_assertion'],'actual':read})
    if expected['checked_kind'] != 'Value': assert expected['boolean_source_assertion'] is None
    counts[checked] += 1
# Independently bind report rows to raw lever identities (harness vector order is sorted).
for row in report['vectors']:
    controls = dict(zip(sorted(d.inputs), map(int,row['vector'])))
    raw = sum(controls[n] << bit for bit,n in enumerate(t['control_order']))
    assert set(row['actual_gates']) == set(d.gate_outputs)
    for n,v in row['actual_gates'].items(): assert observed[raw][('gate',n)] == bool(v)
    for n in d.outputs: assert observed[raw][('lamp',n)] == bool(row['actual_outputs'][d.output_display_name(n)])
# Save negative/partial evidence before cleanup assertions; never call it success.
partial = {'accepted':False,'raw_states':128,'gate_lamp_raw_report_reconstruction':True,
           'electrical_failures':failures,'missing_checked_bus_observations':missing_reads,
           'missing_actual_injection_observations':missing_injection_reads,
           'physical_output_partition':{'lamps':12,'injection_pads':2},
           'checked_counts_from_complete_raw_reads':dict(counts),'raw_rcon_pairs':entries}
a.out.write_text(json.dumps(partial,indent=2)+'\n')
if failures or missing_reads or missing_injection_reads:
    print(json.dumps({'accepted':False,'electrical_failures':len(failures),'states_missing_checked_bus_reads':len(missing_reads),
                      'missing_actual_injection_reads':len(missing_injection_reads)}))
    sys.exit(0)
if not a.cleanup.exists():
    partial['cleanup_pending'] = True
    a.out.write_text(json.dumps(partial,indent=2)+'\n')
    print(json.dumps({'accepted':False,'electrical_failures':0,'checked_counts':dict(counts),'cleanup_pending':True}))
    sys.exit(0)
# Cleanup is an independent RCON record from the root, after all test reads.
cleanup = json.loads(a.cleanup.read_text())
assert cleanup['accepted'] and cleanup['all_original_occupied_sites_air']
assert cleanup['all_test_region_chunks_unforced_after_audit']
positions = {absolute((b.x,b.y,b.z)) for b in d.blocks}
air = set()
queries = []
for entry in cleanup['responses']:
    cmd = entry['command']
    if cmd.startswith('execute if block ') and cmd.endswith(' minecraft:air'):
        assert entry['response'].strip() == 'Test passed'
        air.add(tuple(map(int,cmd.split()[3:6])))
    if cmd.startswith('forceload query '): queries.append(entry)
assert positions <= air
assert len(air) == cleanup['air_sites_checked']
assert queries and all('not marked' in e['response'] for e in queries)
# Derive every region chunk from the exact initial forcing command.
with open_text(a.raw) as f: initial = json.loads(next(f))
parts = initial['command'].split()
assert parts[:2] == ['forceload','add']
x0,z0,x1,z1 = map(int,parts[2:])
required_chunks = {(x,z) for x in range(x0//16,x1//16+1) for z in range(z0//16,z1//16+1)}
queried_chunks = {tuple(int(n)//16 for n in e['command'].split()[2:]) for e in queries}
assert queried_chunks == required_chunks
assert releases and all(e['response'].startswith('Unmarked ') for e in releases)
result = {'accepted': True, 'scope': 'Real-game settled-state raw observation and checked-bus audit; no transient claim',
          'raw_states':128,'actual_gates':37,'output_lamps':len(d.outputs),'checked_counts':dict(counts),
          'actual_injection_pad_reads':128*len(injections),'physical_output_partition':{'lamps':12,'injection_pads':2},
          'raw_rcon_pairs':entries,'observations_per_state':len(expected_keys),
          'cleanup_original_positions':len(positions),'cleanup_air_sites':len(air),'cleanup_chunks':len(required_chunks),
          'raw_decompressed_sha256':content_hash(a.raw, decompress=True),
          'sha256':{os.path.relpath(path,HERE):content_hash(path) for path in [HERE/'candidate.dump',HERE/'expected-trace.json',a.report,a.raw,a.cleanup,HP,Path(__file__)]}}
a.out.write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result))

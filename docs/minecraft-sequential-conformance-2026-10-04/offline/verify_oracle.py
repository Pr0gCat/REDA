"""Offline parity against frozen simulator evidence; never connects to Minecraft."""
import gzip
import hashlib
import json
import sys
from pathlib import Path
sys.path.insert(0, str(Path('conformance').resolve()))
from circuit_conformance import parse_dump
from sequential_conformance import MixedClockOracle, protocol, make_sites, capture_commands
root = Path('docs/minecraft-sequential-conformance-2026-10-04/offline')
dump_path = Path('/tmp/reda-mixed-clock-minecraft-20261004.dump')
reference = Path('docs/mixed-clock-reset-feedback-2026-10-04/repeat1.log.gz')
trace = next(json.loads(line.split('MIXED_CLOCK_RESET_TRACE ',1)[1])
             for line in gzip.open(reference,'rt') if line.startswith('MIXED_CLOCK_RESET_TRACE '))
dump = parse_dump(dump_path)
oracle = MixedClockOracle(dump)
phases = list(protocol())
assert len(phases) == len(trace['phases']) == 264
for index,(inputs,phase) in enumerate(zip(phases,trace['phases'])):
    assert inputs == phase['inputs'], index
    expected = oracle.transition(inputs)
    assert oracle.state == phase['state'], index
    for gate in dump.gates:
        assert expected['gate:'+gate.name] == phase['internal'][gate.name], (index,gate.name)
    for label,cell in phase['cells'].items():
        assert expected['cell:'+label] == cell['value'], (index,label)
        if label.rsplit(':',1)[1] != 'InvC':
            assert expected['cell:'+label+':locked'] == cell['locked'], (index,label)
(root/'capture.mcfunction').write_text('\n'.join(capture_commands(make_sites(dump,(230000,151,100000))))+'\n')
(root/'qualified.dump').write_bytes(dump_path.read_bytes())
files = [dump_path,reference,Path(__file__),Path('src/bin/mc_dump.rs'),
         Path('conformance/circuit_conformance.py'),Path('conformance/sequential_conformance.py'),
         Path('conformance/test_sequential_conformance.py'),
         Path('/tmp/reda-mc-dump-qualified-sequential-final-20261004'),
         Path('/tmp/reda-sequential-head-target-20261004/debug/deps/libreda-b866e79be48eeab8.rlib')]
report = {'status':'offline_pass','phases':264,'gates':len(dump.gates),
          'dff_cells':len(dump.sequential_cells),'real_minecraft_executed':False,
          'sha256':{str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in files}}
(root/'oracle-parity.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps({k:v for k,v in report.items() if k != 'sha256'}))

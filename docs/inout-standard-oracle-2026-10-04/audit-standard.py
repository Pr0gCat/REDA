"""Independently verify frozen standard four-state HDL output and coverage."""
import csv,hashlib,json
from pathlib import Path
root=Path(__file__).resolve().parent
manifest=json.loads((root/'sha256.json').read_text())
for name,digest in manifest.items():assert hashlib.sha256((root/name).read_bytes()).hexdigest()==digest,name
lines=(root/'standard-oracle.csv').read_text().splitlines()
assert len(lines)==8193 and lines[-1]=='oracle_tb.sv:33: $finish called at 8192 (1s)'
rows=list(csv.reader(lines[:-1]))
python_rows=list(csv.DictReader((root/'transitions.csv').open()))
assert len(rows)==len(python_rows)==8192
for index,(row,reported) in enumerate(zip(rows,python_rows)):
 before=index//128;after=(index//2)%64;phase=index%2
 state=before if phase==0 else after
 active=[bool(state&(1<<(2*driver+1))) for driver in range(3) if state&(1<<(2*driver))]
 expected='Z' if not active else 'X' if any(value!=active[0] for value in active) else str(int(active[0]))
 assert row==[str(before),str(after),str(phase),expected]
 assert [reported[key] for key in ['from','to','phase','expected']]==row
 if expected in ('X','Z'):assert reported['checked_diagnosis']==('ContentionBus' if expected=='X' else 'FloatingBus')
 else:assert int(reported['value'])==int(expected) and reported['checked_diagnosis']=='valid'
proof=json.loads((root/'root-standard-oracle-audit.json').read_text())
for name in ['fixture.sv','oracle_tb.sv','standard-oracle.vvp','standard-oracle.csv']:
 expected=next(digest for path,digest in proof['hashes'].items() if Path(path).name==name)
 assert hashlib.sha256((root/name).read_bytes()).hexdigest()==expected
print(json.dumps({'standard_hdl_endpoint_rows_verified':8192,'ordered_transitions':4096,'classification_mismatches':0,'physical_evidence':False,'reda_inout_support_claim':False},indent=2))

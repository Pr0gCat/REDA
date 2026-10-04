import json, os, shutil, subprocess, tempfile, sys
from pathlib import Path
source=Path(sys.argv[1])
auditor=Path(__file__).resolve().with_name('audit.py')
results=[]
for corruption in ['unknown_internal_event','wrong_literal_word']:
 with tempfile.TemporaryDirectory() as temp:
  root=Path(temp)
  (root/'tests/fixtures').mkdir(parents=True)
  shutil.copy(source/'tests/fixtures/rom64_matrix.sv',root/'tests/fixtures/rom64_matrix.sv')
  for directory in ['wave-control-bound-output','wave-matrix-output']:
   (root/directory).mkdir()
   shutil.copy(source/directory/'result.jsonl',root/directory/'result.jsonl')
  if corruption=='unknown_internal_event':
   path=root/'wave-matrix-output/result.jsonl';row=json.loads(path.read_text())
   row['result']['Ok']['phases'][0]['internal_events'].append({'signal':'unobserved_injected_signal','tick':0,'value':True})
   path.write_text(json.dumps(row))
  else:
   import re
   path=root/'tests/fixtures/rom64_matrix.sv'
   path.write_text(re.sub(r"(6'd0\s*:\s*q\s*=\s*16'h)([0-9a-fA-F]+)",lambda m:m[1]+format(int(m[2],16)^1,'04x'),path.read_text(),count=1))
  process=subprocess.run(['python3',str(auditor),str(root)],capture_output=True,env={**os.environ,'PYTHONPATH':str(Path(__file__).resolve().parents[2] / 'tools')})
  assert process.returncode !=0 and 'AssertionError' in process.stderr.decode()
  results.append({'corruption':corruption,'rejected':True,'exit_code':process.returncode})
print(json.dumps(results,indent=2))

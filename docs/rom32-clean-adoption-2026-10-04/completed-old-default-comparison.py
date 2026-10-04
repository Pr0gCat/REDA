from pathlib import Path
import subprocess,json,gzip,hashlib,itertools
root=Path('/home/seith/REDA');out=root/'docs/rom32-clean-adoption-2026-10-04';results=[]
for repeat in range(1,4):
    old=Path(f'/tmp/reda-rom32-clean-20261004/clean-default-wave{repeat}.log')
    adopted=root/f'docs/matrix-first-selection-2026-10-04/default-wave{repeat}.log.gz'
    terminal=json.loads((out/f'default-repeat{repeat}-terminal.json').read_text())
    assert terminal['exit_code']==0 and hashlib.sha256(old.read_bytes()).hexdigest()==terminal['log_sha256']
    report=out/f'clean-default-wave{repeat}-full-quality-audit.json'
    with report.open('w') as dest:
        subprocess.run(['python3',str(root/'tools/audit_balanced_matrix_wave.py'),str(old),str(root/'docs/address-matrix-experiment-2026-10-03/rom32-control1.jsonl.gz')],stdout=dest,check=True,cwd=root)
    phases=0;records=0
    with old.open() as before,gzip.open(adopted,'rt') as after:
        for left,right in itertools.zip_longest(before,after):
            assert left is not None and right is not None,'record count mismatch'
            prefix,a=left.split(' ',1);other,b=right.split(' ',1)
            assert prefix==other
            a=json.loads(a);b=json.loads(b)
            if prefix=='MATRIX_SOURCE':a.pop('artifact');b.pop('artifact')
            assert a==b,(repeat,records,prefix)
            phases+=prefix=='MATRIX_PHASE';records+=1
    assert phases==2048
    results.append({'repeat':repeat,'original_terminal':terminal,'phases':phases,'records':records,'all_headers_except_artifact_and_all_phase_result_records_identical':True,'audit_sha256':hashlib.sha256(report.read_bytes()).hexdigest(),'source_bound_world_claim':'No new independently captured old world is claimed; original driver did not retain worlds immediately.'})
(out/'completed-old-default-comparison.json').write_text(json.dumps({'old_driver_session':97159,'exit_code':0,'comparisons':results,'production_change':False},indent=2)+'\n')

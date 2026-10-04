from pathlib import Path
import subprocess,json,hashlib,gzip,shutil,time
root=Path(__file__).resolve().parent
binary=Path('/tmp/reda-six-clean-target/release/examples/six_default_wave')
for repeat in range(1,4):
    output=root/f'default-repeat{repeat}'
    output.mkdir(exist_ok=False)
    start=time.monotonic()
    with (output/'wave.log').open('wb') as log,(output/'stderr.log').open('wb') as errors:
        code=subprocess.run([str(binary),str(root/'tests/fixtures/rom64_matrix.sv'),'rom64'],cwd=output,stdout=log,stderr=errors).returncode
    manifest={'repeat':repeat,'exit_code':code,'seconds':time.monotonic()-start,'source_sha256':hashlib.sha256((root/'tests/fixtures/rom64_matrix.sv').read_bytes()).hexdigest(),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
    if code==0:
        with (output/'wave.log').open() as stream:
            first=stream.readline()
            assert first.startswith('MATRIX_SOURCE ')
            header=json.loads(first[len('MATRIX_SOURCE '):])
            phases=0;result=None
            for line in stream:
                if line.startswith('MATRIX_PHASE '):phases+=1
                if line.startswith('MATRIX_RESULT '):result=json.loads(line[len('MATRIX_RESULT '):])
        assert phases==8192 and result['phases']==8192
        artifact=Path(header['artifact'])
        assert artifact.parent==output
        shutil.copyfile(artifact,output/'retained.litematic')
        fingerprint=subprocess.check_output(['/tmp/reda-wave-world-fingerprint',str(output/'retained.litematic')],text=True).strip()
        assert fingerprint==header['world_fingerprint']=='f3af4444f9776335f3c51a768a8a7ae3d5e387e66974807feb5009919b459b5a'
        manifest.update(phases=phases,world_fingerprint=fingerprint,world_sha256=hashlib.sha256((output/'retained.litematic').read_bytes()).hexdigest(),result=result,independent_audit_complete=False)
    (output/'terminal.json').write_text(json.dumps(manifest,indent=2)+'\n')
    with (output/'wave.log').open('rb') as source,gzip.open(output/'wave.log.gz','wb') as dest:shutil.copyfileobj(source,dest)
    if code!=0:raise SystemExit(code)

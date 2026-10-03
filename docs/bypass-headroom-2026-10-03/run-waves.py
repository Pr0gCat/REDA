import pathlib,subprocess,json,gzip,sys,hashlib,time
r=pathlib.Path(__file__).parent
sys.path.insert(0,'/home/seith/REDA/tools')
from audit_balanced_matrix_wave import audit_certificate_waves,read
manifest=json.loads(pathlib.Path('docs/balanced-matrix-default-2026-10-03.json').read_text())
results={}
for case,file,top in [('seven','tests/fixtures/seven_segment.v','bcd_seven_segment'),('rom','tests/fixtures/rom16x7.sv','rom16x7')]:
 reference=pathlib.Path(f'docs/balanced-matrix-default-2026-10-03-{case}-repeat1.log.gz')
 control=json.loads(read(manifest['cases'][case]['reference']).splitlines()[0])
 results[case]=[]
 for repeat in range(1,4):
  prefix=r/f'{case}-repeat{repeat}'
  started=time.monotonic()
  if True:
   with prefix.with_suffix('.log').open('w') as out,prefix.with_suffix('.stderr').open('w') as err:
    p=subprocess.run([str(r/'wave_default'),file,top,'prune-empty','short-output','separate-input-floor'],stdout=out,stderr=err)
   assert p.returncode==0,(case,repeat,p.returncode)
  data=prefix.with_suffix('.log').read_bytes()
  current=data.splitlines(); frozen=read(reference).splitlines()
  a=json.loads(current[0].split(b' ',1)[1]); b=json.loads(frozen[0].split(b' ',1)[1])
  artifact=a.pop('artifact'); b.pop('artifact')
  assert a==b and current[1:]==frozen[1:],(case,repeat,'differs beyond process-specific artifact path')
  pathlib.Path(prefix.with_suffix('.litematic')).write_bytes(pathlib.Path(artifact).read_bytes())
  audit=audit_certificate_waves(prefix.with_suffix('.log'),control)
  prefix.with_name(prefix.name+'-audit.json').write_text(json.dumps(audit,indent=2))
  prefix.with_suffix('.log.gz').write_bytes(gzip.compress(data,mtime=0))
  results[case].append({'repeat':repeat,'seconds':time.monotonic()-started,'wave_sha256':hashlib.sha256(data).hexdigest(),'audit':audit,'identical_to_frozen_actual_default_except_process_artifact_path':True})
  (r/'results.json').write_text(json.dumps(results,indent=2))
  print(case,repeat,'all waves equal, audit passes',flush=True)

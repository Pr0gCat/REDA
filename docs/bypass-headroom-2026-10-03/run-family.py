import pathlib,subprocess,json,gzip,hashlib,sys
r=pathlib.Path(__file__).parent
sys.path.insert(0,'/home/seith/REDA/tools')
fixed=pathlib.Path('docs/generator-default-quality-selection-2026-10-02-family.jsonl.gz')
control=gzip.decompress(fixed.read_bytes())
results=[]
for repeat in [1,2,3]:
 prefix=r/f'family-repeat{repeat}'
 with prefix.with_suffix('.jsonl').open('w') as out,prefix.with_suffix('.stderr').open('w') as err:
  p=subprocess.run([str(r/'family_probe'),str(fixed)],stdout=out,stderr=err)
 assert p.returncode==0,p.returncode
 data=prefix.with_suffix('.jsonl').read_bytes()
 assert data==control,repeat
 assert len(data.splitlines())==4096
 prefix.with_suffix('.jsonl.gz').write_bytes(gzip.compress(data,mtime=0))
 results.append({'repeat':repeat,'exact_immutable_records':True,'functions':4096,'sha256':hashlib.sha256(data).hexdigest(),'zero_regressions':True})
 (r/'family-results.json').write_text(json.dumps(results,indent=2))
 print(repeat,'4096 exact frozen records; no regressions',flush=True)

import pathlib,json,time,subprocess,gzip,shutil
p=pathlib.Path(__file__).parent;baseline=json.loads(gzip.decompress((p/'control.jsonl.gz').read_bytes()).splitlines()[0])
for repeat in [2,3]:
 name=f'control-repeat{repeat}';args=json.loads((p/'control-arguments.json').read_text());start=time.monotonic()
 with (p/(name+'.log')).open('w') as out,(p/(name+'.stderr')).open('w') as err:r=subprocess.run(args,stdout=out,stderr=err)
 (p/(name+'-terminal.json')).write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start})+'\n');assert r.returncode==0
 record=json.loads((p/(name+'.log')).read_text().strip());assert record==baseline
 (p/(name+'.jsonl.gz')).write_bytes(gzip.compress((json.dumps(record)+'\n').encode(),mtime=0))
 artifact=next(line.split(' ',1)[1] for line in (p/(name+'.stderr')).read_text().splitlines() if line.startswith('CONTROL_ARTIFACT '));shutil.copy2(artifact,p/(name+'.litematic'))
 print(name,'original-method raw equality passed',flush=True)

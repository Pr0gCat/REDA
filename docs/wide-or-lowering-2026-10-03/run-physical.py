import pathlib,json,subprocess,time,gzip
p=pathlib.Path(__file__).parent
build=json.loads((p/'build-arguments.json').read_text());extern=[]
for i,a in enumerate(build):
 if a=='--extern':extern+=build[i:i+2]
args=['rustc','--edition=2021','-C','opt-level=1','-C','debug-assertions=yes',str(p/'physical.rs'),'-L','dependency=/home/seith/REDA/target/release/deps','--extern','reda='+str(p/'libreda_wide_or.rlib')]+extern+['-o',str(p/'physical-probe')]
(p/'physical-build-arguments.json').write_text(json.dumps(args,indent=2));start=time.monotonic()
with (p/'physical-build.log').open('w') as out:r=subprocess.run(args,stdout=out,stderr=subprocess.STDOUT)
(p/'physical-build-terminal.json').write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start}))
if r.returncode:raise SystemExit(r.returncode)
results=[]
for count in [4,5,7]:
 for repeat in [1,2,3]:
  name=f'physical-{count}-r{repeat}';args=[str(p/'physical-probe'),str(count),str(repeat),str(p)];(p/(name+'-arguments.json')).write_text(json.dumps(args,indent=2));start=time.monotonic()
  with (p/(name+'.jsonl')).open('w') as out,(p/(name+'.stderr')).open('w') as err:r=subprocess.run(args,stdout=out,stderr=err)
  entry={'count':count,'repeat':repeat,'exit_code':r.returncode,'seconds':time.monotonic()-start};results.append(entry);(p/'physical-results.json').write_text(json.dumps(results,indent=2))
  data=(p/(name+'.jsonl')).read_bytes();(p/(name+'.jsonl.gz')).write_bytes(gzip.compress(data,mtime=0))
  print(json.dumps(entry),flush=True)
  if r.returncode:raise SystemExit(r.returncode)

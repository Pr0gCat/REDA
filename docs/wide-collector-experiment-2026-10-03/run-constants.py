import pathlib,json,subprocess,time,sys,shutil
p=pathlib.Path(__file__).parent
args=['cargo','build','--release','--manifest-path',str(p/'Cargo.toml'),'--example','wide_constant_wave'];start=time.monotonic()
(p/'constant-build-arguments.json').write_text(json.dumps(args,indent=2)+'\n')
with (p/'constant-build.log').open('w') as out,(p/'constant-build.stderr').open('w') as err:r=subprocess.run(args,stdout=out,stderr=err)
(p/'constant-build-terminal.json').write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start})+'\n')
if r.returncode:sys.exit(r.returncode)
for top in ['wide_all_low','wide_all_high','wide_mixed']:
 records=[]
 for repeat in range(1,4):
  name=f'{top}-repeat{repeat}';args=['/home/seith/REDA/target/release/examples/wide_constant_wave',str(p/'tests/fixtures'/f'{top}.sv'),top,'prune-empty','short-output'];start=time.monotonic()
  (p/(name+'-arguments.json')).write_text(json.dumps(args,indent=2)+'\n')
  with (p/(name+'.log')).open('w') as out,(p/(name+'.stderr')).open('w') as err:r=subprocess.run(args,stdout=out,stderr=err)
  (p/(name+'-terminal.json')).write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start})+'\n')
  if r.returncode:sys.exit(r.returncode)
  lines=(p/(name+'.log')).read_text().splitlines();h=json.loads(lines[0].split(' ',1)[1]);assert len(h['outputs'])==16
  wanted=[0 if top=='wide_all_low' else 65535 if top=='wide_all_high' else sum(int(v)<<(i+8*k) for k in range(2) for i,v in enumerate([False,True,bool(a&1),bool(a&8),bool(a&1)!=bool(a&2),False,True,bool(a&4)])) for a in range(16)]
  assert h['words']==wanted
  records.append(lines[1:]);assert records[-1]==records[0];shutil.copy2(h['artifact'],p/(name+'.litematic'))
  auditargs=['python3','/home/seith/REDA/tools/audit_balanced_matrix_wave.py',str(p/(name+'.log'))]
  with (p/(name+'-audit.json')).open('w') as out,(p/(name+'-audit.stderr')).open('w') as err:r=subprocess.run(auditargs,stdout=out,stderr=err)
  assert r.returncode==0;print(name,'literal truth and complete implementation replay passed',flush=True)

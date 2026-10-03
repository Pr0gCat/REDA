import pathlib,subprocess,json,time,gzip,hashlib,sys
p=pathlib.Path(__file__).parent
stages=[('existing-worlds',['cargo','test','--release','--manifest-path',str(p/'Cargo.toml'),'--test','balanced_torch_matrix','full_source_boundaries_are_certified_without_name_selection','--','--exact']),('wave-build',['cargo','build','--release','--manifest-path',str(p/'Cargo.toml'),'--example','wide_wave'])]
stages=[] # Existing-world checks and observer build already terminal/pass.
for name,args in stages:
 (p/(name+'-arguments.json')).write_text(json.dumps(args,indent=2)+'\n');start=time.monotonic()
 with (p/(name+'.log')).open('w') as out,(p/(name+'.stderr')).open('w') as err:r=subprocess.run(args,stdout=out,stderr=err)
 (p/(name+'-terminal.json')).write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start})+'\n')
 if r.returncode:sys.exit(r.returncode)
records=[]
for repeat in range(1,4):
 name=f'wide-binary-repeat{repeat}';args=['/home/seith/REDA/target/release/examples/wide_wave',str(p/'tests/fixtures/wide_rom_binary.sv'),'wide_rom','prune-empty','short-output'];start=time.monotonic()
 (p/(name+'-arguments.json')).write_text(json.dumps(args,indent=2)+'\n')
 with (p/(name+'.log')).open('w') as out,(p/(name+'.stderr')).open('w') as err:r=subprocess.run(args,stdout=out,stderr=err)
 (p/(name+'-terminal.json')).write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start})+'\n')
 if r.returncode:sys.exit(r.returncode)
 lines=(p/(name+'.log')).read_text().splitlines();header=json.loads(lines[0].split(' ',1)[1]);assert header['words']==json.loads((p/'literal-words.json').read_text())
 assert len(header['outputs'])==16
 records.append(lines[1:]);assert records[-1]==records[0]
 artifact=pathlib.Path(header['artifact']);assert artifact.exists();(p/(name+'.litematic')).write_bytes(artifact.read_bytes())
 auditargs=['python3','/home/seith/REDA/tools/audit_balanced_matrix_wave.py',str(p/(name+'.log'))]
 with (p/(name+'-audit.json')).open('w') as out,(p/(name+'-audit.stderr')).open('w') as err:r=subprocess.run(auditargs,stdout=out,stderr=err)
 assert r.returncode==0
 print(name,'complete replay passed',flush=True)
print('Three separately retained worlds, complete raw phase equality; no historical quality control claim',flush=True)

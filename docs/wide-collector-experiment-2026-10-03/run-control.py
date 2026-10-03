import pathlib,json,subprocess,time,gzip,sys
p=pathlib.Path(__file__).parent
stages=[('control-build',['cargo','build','--release','--manifest-path',str(p/'Cargo.toml'),'--example','wide_control']),('control',['/home/seith/REDA/target/release/examples/wide_control',str(p/'tests/fixtures/wide_rom_binary.sv'),'wide_rom','0','--control','--trace'])]
for name,args in stages:
 (p/(name+'-arguments.json')).write_text(json.dumps(args,indent=2)+'\n');start=time.monotonic()
 with (p/(name+'.log')).open('w') as out,(p/(name+'.stderr')).open('w') as err:r=subprocess.run(args,stdout=out,stderr=err)
 (p/(name+'-terminal.json')).write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start})+'\n')
 if r.returncode:sys.exit(r.returncode)
records=[json.loads(x) for x in (p/'control.log').read_text().splitlines() if x.startswith('{')]
assert len(records)==1
(p/'control.jsonl.gz').write_bytes(gzip.compress((''.join(json.dumps(x)+'\n' for x in records)).encode(),mtime=0))
result=records[0]['result'];print('Original-method semantic result: '+('world generated' if 'Ok' in result else 'typed refusal'),flush=True)

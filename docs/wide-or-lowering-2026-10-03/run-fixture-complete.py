import pathlib,json,subprocess,time,os
p=pathlib.Path(__file__).parent
library_args=json.loads((p/'build-arguments.json').read_text())
extern=[]
i=0
while i<len(library_args):
 if library_args[i]=='--extern':extern+=library_args[i:i+2];i+=2
 else:i+=1
common=['rustc','--edition=2021','--test','-C','opt-level=1','-C','debug-assertions=yes',str(p/'check.rs'),'-L','dependency=/home/seith/REDA/target/release/deps']+extern
stages=[('control-build',common+['--extern','reda=/tmp/reda-final-sweep-production-20261003/libreda_candidate.rlib','-o',str(p/'control-tests')]),('control', [str(p/'control-tests'),'--nocapture']),('build',library_args),('check-build',common+['--extern','reda='+str(p/'libreda_wide_or.rlib'),'-o',str(p/'candidate-tests')]),('check',[str(p/'candidate-tests'),'--nocapture'])]
for label,args in stages[2:]:
 (p/(label+'-arguments.json')).write_text(json.dumps(args,indent=2));start=time.monotonic()
 with (p/(label+'.log')).open('w') as out:r=subprocess.run(args,stdout=out,stderr=subprocess.STDOUT)
 (p/(label+'-terminal.json')).write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start,'scope':'source truth/provenance/all lowering APIs and raw physical rejection; no emitted-world acceptance'}))
 if r.returncode and label!='control':raise SystemExit(r.returncode)
 if label=='control':assert r.returncode==101,'Original defect must reproduce'

import pathlib,json,subprocess,time
p=pathlib.Path(__file__).parent
stages=[('production-integration-build',json.loads((p/'production-integration-build-arguments.json').read_text())),('production-integration',[str(p/'production-integration-tests'),'--nocapture']),('unit-build',json.loads((p/'unit-build-arguments.json').read_text()))]
for label,args in stages:
 start=time.monotonic()
 with (p/(label+'.log')).open('w') as out:r=subprocess.run(args,stdout=out,stderr=subprocess.STDOUT)
 (p/(label+'-terminal.json')).write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start}))
 if r.returncode:raise SystemExit(r.returncode)
for scope in ['compile::topology::','compile::lowering::','compile::polarity::']:
 label='unit-'+scope.split('::')[1];args=[str(p/'unit-tests'),scope,'--nocapture'];(p/(label+'-arguments.json')).write_text(json.dumps(args,indent=2));start=time.monotonic()
 with (p/(label+'.log')).open('w') as out:r=subprocess.run(args,stdout=out,stderr=subprocess.STDOUT)
 (p/(label+'-terminal.json')).write_text(json.dumps({'exit_code':r.returncode,'seconds':time.monotonic()-start}))
 if r.returncode:raise SystemExit(r.returncode)

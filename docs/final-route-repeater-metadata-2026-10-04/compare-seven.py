from pathlib import Path
import gzip,json,sys
sys.path.insert(0,'/home/seith/REDA/tools')
from compare_feedback_probe import audit,compare_quality
root=Path('/home/seith/REDA');out=Path('/tmp/reda-repeater-metadata-20261004')
def read(path):
 data=gzip.decompress(path.read_bytes()) if path.suffix=='.gz' else path.read_bytes()
 records=[]
 for line in data.decode().splitlines():
  if line.startswith('FEEDBACK_TRACE '):records.append(json.loads(line[len('FEEDBACK_TRACE '):]))
  elif line.startswith('{'):
   try:r=json.loads(line)
   except json.JSONDecodeError:continue
   if isinstance(r,dict) and 'case' in r and 'phases' in r:records.append(r)
 return records
cases={'native-feedback_counter':'counter','native-feedback_fsm':'fsm','native-feedback_state_enable':'state_enable','native-reset_counter':'counter','native-hierarchy_reset_counter':'counter','native-reset_fsm':'fsm','native-conditional_state_enable':'state_enable'}
candidate=read(out/'seven-candidate.log')
original=read(root/'docs/native-sequential-trajectory-gap-2026-10-04/seven-current.log.gz')
baseline=read(root/'docs/native-statement-reset-2026-10-02-positive_run_2.log.gz')+read(root/'docs/restored-non-strict-policy-validation-2026-10-03-feedback.jsonl.gz')
baseline=[r for r in baseline if r['case'] in cases]
result={'candidate_audit':audit(candidate,cases),'versus_pre_fallback_current':compare_quality(candidate,original,cases),'versus_retained_original':compare_quality(candidate,baseline,cases),'exact_trace_matches':{r['case']:r==next(old for old in original if old['case']==r['case']) for r in candidate}}
(out/'seven-quality.json').write_text(json.dumps(result,indent=2)+'\n')
for label in ['versus_pre_fallback_current','versus_retained_original']:print(label,'qualified=',result[label]['qualified'],'regressions=',len(result[label]['regressions']))
print('exact trace matches',result['exact_trace_matches'])

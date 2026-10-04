from pathlib import Path
import json,hashlib,itertools,sys
root=Path(sys.argv[1])
terminals=[json.loads((root/f'default-repeat{i}/terminal.json').read_text()) for i in range(1,4)]
for key in ['source_sha256','binary_sha256','phases','world_fingerprint','world_sha256','result']:
    assert all(t[key]==terminals[0][key] for t in terminals),key
streams=[(root/f'default-repeat{i}/wave.log').open() for i in range(1,4)]
try:
    headers=[json.loads(next(s).split(' ',1)[1]) for s in streams]
    for h in headers:
        h.pop('artifact')
    assert headers[0]==headers[1]==headers[2]
    count=0
    digest=hashlib.sha256()
    for lines in itertools.zip_longest(*streams):
        assert all(line is not None for line in lines)
        records=[json.loads(line.split(' ',1)[1]) for line in lines]
        assert records[0]==records[1]==records[2],count
        if lines[0].startswith('MATRIX_PHASE '):
            count+=1
            digest.update(json.dumps(records[0],sort_keys=True,separators=(',',':')).encode())
        else:
            assert all(line.startswith('MATRIX_RESULT ') for line in lines)
    assert count==8192
finally:
    for s in streams:s.close()
result={'independent_processes':3,'phases_per_process':count,'all_phase_records_identical':True,'header_difference_only':'artifact path','source_binary_world_and_quality_identical':True,'canonical_phase_records_sha256':digest.hexdigest()}
Path(__file__).with_name('three-repeat-comparison.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result))

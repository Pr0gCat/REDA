"""Lightweight retained trace audit; exit1 means the candidate is rejected."""
import gzip
import json
from pathlib import Path
import compare_feedback_probe as checker

HERE = Path(__file__).resolve().parent
def load(name):
    with gzip.open(HERE/(name+'-trace.jsonl.gz'),'rt') as f:
        return [json.loads(line) for line in f]
result = checker.compare_quality(load('candidate'),load('baseline'))
frozen = json.loads((HERE/'independent-negative-quality.json').read_text())
assert result['qualified'] == frozen['qualified']
assert result['regressions'] == frozen['regressions']
print(json.dumps({'qualified':result['qualified'],'regressions':len(result['regressions']),'retained_replay_matches':True}))
raise SystemExit(0 if result['qualified'] else 1)

"""Verify source-bound negative game result and raw cleanup evidence."""
from pathlib import Path
import hashlib,json
root=Path(__file__).resolve().parent
for line in (root/'FROZEN-SHA256SUMS').read_text().splitlines():
 digest,name=line.split('  ',1)
 assert hashlib.sha256((root/name).read_bytes()).hexdigest()==digest,name
assert json.loads((root/'game-terminal.json').read_text())['exit_code']==2
trace=[json.loads(line) for line in (root/'game-rcon.jsonl').read_text().splitlines()]
rejected=[row for row in trace if row['command'].startswith('setblock ') and 'minecraft:repeater[' in row['command']]
assert len(rejected)==4
for row in rejected:
 assert 'delay=0' in row['command']
 assert "does not accept '0' for delay property" in row['response']
assert not (root/'game-report.json').exists()
cleanup=json.loads((root/'cleanup-audit.json').read_text())
assert cleanup['accepted'] and cleanup['all_original_occupied_sites_air'] and cleanup['all_test_region_chunks_unforced_after_audit']
air=[row for row in cleanup['responses'] if row['command'].startswith('execute if block ')]
assert len(air)==99 and all(row['response'].strip().startswith('Test passed') for row in air)
chunks=[row for row in cleanup['responses'] if row['command'].startswith('forceload query ')]
assert len(chunks)==80 and all('not marked' in row['response'] for row in chunks)
print(json.dumps({'negative_game_verified':True,'invalid_delay_rejections':4,'truth_vectors_executed':0,'air_sites_checked':99,'chunks_unforced_before_after':40,'production_adoption':False},indent=2))

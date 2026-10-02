"""Replay statement-reset circuits with the independent literal state oracle."""
import argparse
import gzip
import json
from pathlib import Path
from compare_feedback_probe import audit as audit_feedback
from compare_negedge_probe import audit as audit_negedge


def audit(records):
    cases = {f'{frontend}-{top}': case for frontend in ['native', 'yosys']
             for top, case in [('reset_counter', 'counter'), ('hierarchy_reset_counter', 'counter'),
                               ('reset_fsm', 'fsm'), ('conditional_state_enable', 'state_enable')]}
    negative = {f'{frontend}-negedge_{top}': 'negedge_counter'
                for frontend in ['native', 'yosys']
                for top in ['reset_counter', 'hierarchy_reset_counter']}
    assert len(records) == 12 and {r['case'] for r in records} == set(cases) | set(negative)
    positive_result = audit_feedback([r for r in records if r['case'] in cases], cases)
    negative_result = audit_negedge([r for r in records if r['case'] in negative], negative)
    return {'cases': 12, 'retained_phases_per_case': 256,
            'positive': positive_result, 'negative': negative_result}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace')
    path = Path(parser.parse_args().trace)
    data = gzip.decompress(path.read_bytes()) if path.suffix == '.gz' else path.read_bytes()
    print(json.dumps(audit([json.loads(line) for line in data.splitlines()]), indent=2))

"""Negative tampering controls over immutable completed game evidence."""
import copy
import gzip
import json
import tempfile
import unittest
from pathlib import Path

from audit_sequential_conformance import audit, load_lines


class SequentialAuditNegativeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root=Path(__file__).resolve().parents[1]
        cls.evidence=cls.root/'docs/minecraft-sequential-conformance-2026-10-04/game-retry3'
        cls.reference=cls.root/'docs/mixed-clock-reset-feedback-2026-10-04/repeat1.log.gz'
        cls.manifest=cls.evidence/'protocol-sources/runtime-source-manifest.json'
        cls.report=json.loads((cls.evidence/'report.json').read_text())

    def fixture(self,directory):
        directory=Path(directory)
        for name in ['ticks.jsonl.gz','commands.jsonl.gz','snapshot-pack']:
            (directory/name).symlink_to(self.evidence/name)
        report=copy.deepcopy(self.report)
        (directory/'report.json').write_text(json.dumps(report))
        return directory,report

    def test_other_cleanup_error_is_never_adjudicated(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory,report=self.fixture(temporary)
            report['cleanup_error']='deliberate negative: region cleanup failed'
            (directory/'report.json').write_text(json.dumps(report))
            with self.assertRaisesRegex(ValueError,'unadjudicated cleanup error'):
                audit(directory,self.reference,source_manifest=self.manifest)

    def test_actual_storage_failure_is_never_accepted_as_false_positive(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory,_=self.fixture(temporary)
            commands=load_lines(self.evidence/'commands.jsonl.gz')
            record=next(record for record in commands if record['command']=='data remove storage reda_sequential_20261004:trace snapshot')
            record['response']='Unknown or incomplete command'
            (directory/'commands.jsonl.gz').unlink()
            with gzip.open(directory/'commands.jsonl.gz','wt') as stream:
                for record in commands:stream.write(json.dumps(record)+'\n')
            with self.assertRaisesRegex(ValueError,'cleanup command rejected or unrecognised'):
                audit(directory,self.reference,source_manifest=self.manifest)

    def test_missing_tick_frame_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory,_=self.fixture(temporary)
            ticks=load_lines(self.evidence/'ticks.jsonl.gz')
            index=next(index for index in range(2,len(ticks)-1)
                       if ticks[index]['phase']==ticks[index-1]['phase']==ticks[index+1]['phase']
                       and ticks[index]['game_tick']==ticks[index-1]['game_tick']+1
                       and ticks[index+1]['game_tick']==ticks[index]['game_tick']+1
                       and ticks[index]['values']==ticks[index-1]['values'])
            del ticks[index]
            (directory/'ticks.jsonl.gz').unlink()
            with gzip.open(directory/'ticks.jsonl.gz','wt') as stream:
                for record in ticks:stream.write(json.dumps(record)+'\n')
            with self.assertRaisesRegex(ValueError,'skipped/reversed game tick'):
                audit(directory,self.reference,source_manifest=self.manifest)


if __name__=='__main__':unittest.main()

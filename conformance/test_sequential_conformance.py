"""Boundary and state-oracle tests; these are not Minecraft evidence."""
import tempfile
import unittest
import json
import zipfile
from types import SimpleNamespace
from unittest.mock import patch
from pathlib import Path
from circuit_conformance import parse_dump,evaluate_expected,expected_block_at,RegionNotReady,CircuitMismatch
from sequential_conformance import Site,capture_commands,parse_snapshot,protocol,prepare_pack,run,TickCapture,MixedClockOracle


class SequentialDumpTest(unittest.TestCase):
    def test_literal_oracle_rejects_changed_logic_with_identical_interface(self):
        import copy
        path=Path(__file__).resolve().parents[1]/'docs/minecraft-sequential-conformance-2026-10-04/offline/qualified.dump'
        dump=parse_dump(path)
        MixedClockOracle(dump)
        changed=copy.deepcopy(dump)
        next(gate for gate in changed.gates if gate.name=='g0').inputs[0]='en'
        self.assertEqual(changed.inputs,dump.inputs)
        self.assertEqual(changed.outputs,dump.outputs)
        with self.assertRaisesRegex(ValueError,'qualified lowered graph'):MixedClockOracle(changed)
        changed=copy.deepcopy(dump)
        changed.output_labels={'g0':'q_b','g6':'q_a'}
        with self.assertRaisesRegex(ValueError,'qualified lowered graph'):MixedClockOracle(changed)

    def test_tick_step_waits_for_exact_boundary_and_rejects_skipped_tick(self):
        from unittest.mock import Mock
        capture=TickCapture.__new__(TickCapture)
        capture.client=Mock()
        capture.client.command.return_value='Stepping 1 tick(s)'
        capture.current={'game_tick':7,'values':{}}
        capture.capture=Mock(side_effect=[{'game_tick':7,'values':{}},{'game_tick':8,'values':{}}])
        with patch('sequential_conformance.time.sleep'):
            self.assertEqual(capture.step()['game_tick'],8)
        capture.capture=Mock(return_value={'game_tick':10,'values':{}})
        with self.assertRaisesRegex(RegionNotReady,'exclusive tick control'):capture.step()
        capture.client.command.return_value='Unknown command'
        with self.assertRaisesRegex(RegionNotReady,'not acknowledged'):capture.step()

    def parse(self,text):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'dump.txt';path.write_text(text)
            return parse_dump(path)

    def test_dff_is_a_stateful_repeater_and_comb_evaluation_refuses_it(self):
        dump=self.parse('SIZE 1 1 1\nGATEOUT q 0 0 0\nGATE q d,clk dff_p\n')
        self.assertEqual(expected_block_at(dump.gates[0]),'minecraft:repeater')
        with self.assertRaisesRegex(ValueError,'stateful'):
            evaluate_expected(dump.gates,{'d':1,'clk':1})

    def test_invalid_dff_arity_component_and_metadata_are_rejected(self):
        for tail in ['GATE q d dff_p\n','GATEOUT q 0 0 0 Torch\nGATE q d,clk dff_p\n',
                     'DFFCELL\n','GATE q d,clk dff_p\nDFFCELL q SData 0 0 0 Repeater\n']:
            with self.subTest(tail=tail),self.assertRaises(ValueError):
                self.parse('SIZE 1 1 1\n'+tail)

    def test_native_capture_queries_actual_components_and_locking(self):
        sites=[Site('q',(1,2,3),'minecraft:repeater'),Site('wire',(4,5,6),'minecraft:redstone_wire')]
        commands=capture_commands(sites)
        self.assertTrue(any('minecraft:repeater[powered=true]' in command for command in commands))
        self.assertTrue(any('minecraft:repeater[locked=true]' in command for command in commands))
        self.assertTrue(any('minecraft:redstone_wire[power=0]' in command for command in commands))
        sample=parse_snapshot('{t0: 7, t1: 7, s0p: 1, s0v: 0, s0l: 1, s1p: 1, s1v: 0}',sites)
        self.assertEqual(sample,{'game_tick':7,'values':{'q':False,'q:locked':True,'wire':True}})
        for text in ['{t0:7,t1:8,s0p:1,s0v:0,s0l:1,s1p:1,s1v:0}',
                     '{t0:7,t1:7,s0p:1,s0v:0,s0l:1,s1p:1}']:
            with self.assertRaises(RegionNotReady):parse_snapshot(text,sites)
        with self.assertRaises(CircuitMismatch):
            parse_snapshot('{t0:7,t1:7,s0p:0,s0v:0,s0l:1,s1p:1,s1v:0}',sites)

    def test_protocol_has_exact_264_phases_and_changes_controls_without_edges(self):
        phases=list(protocol());self.assertEqual(len(phases),264)
        for start in range(0,264,3):
            group=phases[start:start+3]
            self.assertEqual(group[0],group[2])
            self.assertEqual(group[0]['clk_a'],group[1]['clk_a'])
            self.assertEqual(group[0]['clk_b'],group[1]['clk_b'])
            for name in ('reset_a','reset_b','en'):self.assertNotEqual(group[0][name],group[1][name])

    def test_existing_datapack_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);jar=root/'server.jar'
            with zipfile.ZipFile(jar,'w') as archive:
                archive.writestr('version.json',json.dumps({'pack_version':{'data_major':107,'data_minor':1}}))
            pack=root/'existing';pack.mkdir();sentinel=pack/'owner';sentinel.write_text('preserve')
            with self.assertRaises(FileExistsError): prepare_pack(pack,jar,[])
            self.assertEqual(sentinel.read_text(),'preserve')

    def test_partial_pack_write_cleans_only_new_owned_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);jar=root/'server.jar'
            with zipfile.ZipFile(jar,'w') as archive:
                archive.writestr('version.json',json.dumps({'pack_version':{'data_major':107,'data_minor':1}}))
            pack=root/'owned'
            with patch.object(Path,'write_text',side_effect=OSError('synthetic write failure')):
                with self.assertRaises(OSError): prepare_pack(pack,jar,[])
            self.assertFalse(pack.exists())

    def test_properties_setup_failure_preserves_report_and_removes_owned_pack(self):
        from circuit_conformance import Dump,Gate
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);jar=root/'server.jar';source=root/'dump';source.write_text('fixture')
            with zipfile.ZipFile(jar,'w') as archive:
                archive.writestr('version.json',json.dumps({'pack_version':{'data_major':107,'data_minor':1}}))
            positions={'g0':(0,0,0),'g6':(10,0,0)}
            cells={(name,role):((index*10+offset,0,0),'Torch' if role=='InvC' else 'Repeater')
                   for index,name in enumerate(positions) for offset,role in enumerate(['SData','MData','MLock','SLock','InvC'])}
            dump=Dump((20,4,4),[],{name:(0,0,0) for name in ['clk_a','clk_b','reset_a','reset_b','en']},
                      positions,{'g0':'q_a','g6':'q_b'},positions,
                      [Gate('g0',['en','clk_a'],'dff_p'),Gate('g6',['g0','clk_b'],'dff_p')],{},cells)
            args=SimpleNamespace(dump=source,origin='0,0,0',out=root/'evidence',pack=root/'new-pack',
                                 properties=root/'missing-properties',jar=jar,host='127.0.0.1')
            with patch('sequential_conformance.parse_dump',return_value=dump),patch('sequential_conformance.MixedClockOracle'),patch('sequential_conformance.RconClient') as client:
                with self.assertRaises(FileNotFoundError): run(args)
                client.assert_not_called()
            self.assertFalse(args.pack.exists())
            report=json.loads((args.out/'report.json').read_text())
            self.assertEqual(report['status'],'fail')
            self.assertEqual(report['error_type'],'FileNotFoundError')
            self.assertTrue((args.out/'snapshot-pack'/'pack.mcmeta').exists())


if __name__=='__main__':unittest.main()

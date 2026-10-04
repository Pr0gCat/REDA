"""Boundary and state-oracle tests; these are not Minecraft evidence."""
import tempfile
import unittest
import json
import zipfile
from types import SimpleNamespace
from unittest.mock import patch
from pathlib import Path
from circuit_conformance import parse_dump,evaluate_expected,expected_block_at,RegionNotReady,CircuitMismatch
from sequential_conformance import Site,capture_commands,parse_snapshot,protocol,prepare_pack,run,TickCapture,MixedClockOracle,STORAGE,SNAPSHOT_PATH,cleanup_response_matches


class SequentialDumpTest(unittest.TestCase):
    def test_actual_storage_cleanup_ack_is_exact_and_errors_remain_rejected(self):
        command=f'data remove storage {STORAGE} {SNAPSHOT_PATH}'
        self.assertTrue(cleanup_response_matches(command,f'Modified storage {STORAGE}',('Removed ',)))
        for response in ['Unknown or incomplete command','Modified storage another:trace',f'Modified storage {STORAGE} trailing error']:
            self.assertFalse(cleanup_response_matches(command,response,('Removed ',)))
        self.assertFalse(cleanup_response_matches('reload',f'Modified storage {STORAGE}',('Reloading!',)))

    def test_module_and_direct_script_entrypoints_import_without_pythonpath(self):
        import os,subprocess,sys
        root=Path(__file__).resolve().parents[1]
        environment=dict(os.environ);environment.pop('PYTHONPATH',None)
        for arguments in [('-m','conformance.sequential_conformance'),('conformance/sequential_conformance.py',)]:
            result=subprocess.run([sys.executable,*arguments,'--help'],cwd=root,env=environment,capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)

    def test_failed_cleanup_responses_are_reported_and_release_still_runs(self):
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);jar=root/'server.jar'
            with zipfile.ZipFile(jar,'w') as archive:
                archive.writestr('version.json',json.dumps({'pack_version':{'data_major':107,'data_minor':1}}))
            source=Path(__file__).resolve().parents[1]/'docs/minecraft-sequential-conformance-2026-10-04/offline/qualified.dump'
            args=SimpleNamespace(dump=source,origin='0,0,0',out=root/'evidence',pack=root/'new-pack',properties=root/'properties',jar=jar,host='127.0.0.1')
            client=Mock();client.command.return_value='Unknown or incomplete command'
            with patch('sequential_conformance.read_properties',return_value={'rcon.password':'test'}),patch('sequential_conformance.RconClient',return_value=client),patch('sequential_conformance.forceload'),patch('sequential_conformance.wait_for_region_loaded'),patch('sequential_conformance.verify_region_is_air'),patch('sequential_conformance.verify_positions_placed',return_value=[]),patch('sequential_conformance.clear_region') as clear:
                # Placement succeeds in this offline failure injection; cleanup
                # still uses the actual command reader and rejects raw errors.
                clear.side_effect=[None,RegionNotReady('synthetic cleanup failure')]
                with self.assertRaisesRegex(RegionNotReady,'function was not executed'):run(args)
            report=json.loads((args.out/'report.json').read_text())
            for key in ['unfreeze_error','cleanup_error','forceload_release_error','datapack_unload_error','storage_remove_error','datapack_reload_error']:
                self.assertIn(key,report)
            self.assertIn('Unknown or incomplete command',report['storage_remove_error'])
            self.assertFalse(args.pack.exists())

    def test_native_snapshot_uses_required_owned_nbt_path_and_refuses_missing_function(self):
        from unittest.mock import Mock
        commands=capture_commands([Site('q',(1,2,3),'minecraft:repeater')])
        self.assertEqual(commands[0],f'data modify storage {STORAGE} snapshot set value {{}}')
        for command in commands[1:]:
            self.assertIn(f'storage {STORAGE} snapshot.',command)
        client=Mock()
        client.command.return_value='Unknown function reda_sequential_20261004:capture'
        with self.assertRaisesRegex(RegionNotReady,'function was not executed'):TickCapture(client,[])
        self.assertEqual(client.command.call_count,1)
        client=Mock()
        client.command.side_effect=['Executed 3 command(s) from function',
                                    'Storage has the following contents: {t0: 7, t1: 7}']
        capture=TickCapture(client,[])
        self.assertEqual(capture.current,{'game_tick':7,'values':{}})
        client.command.assert_called_with(f'data get storage {STORAGE} {SNAPSHOT_PATH}')
        client=Mock()
        client.command.side_effect=['Running function reda_sequential_20261004:capture',
                                    'Storage has the following contents: {t0: 7, t1: 7}']
        self.assertEqual(TickCapture(client,[]).current,{'game_tick':7,'values':{}})

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

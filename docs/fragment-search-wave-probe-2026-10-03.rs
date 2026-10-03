//! Independent continuous-history record producer for recursive search experiments.
use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisBudget, SynthesisInput};
use reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
use reda::compile::{Gate, Netlist};
use reda::compile::planner::{Anchor, PortPlacements};
use reda::redstone::simulator::{position::Position, Simulator};
use reda::redstone::world::{block::{BlockKind, Facing}, storage::World};
use serde_json::json;
use std::collections::BTreeMap;

fn signal(world: &World, at: (i32, i32, i32)) -> bool {
    let block = world.get(at.0, at.1, at.2);
    assert!(matches!(block.kind, BlockKind::RedstoneWire | BlockKind::Torch |
        BlockKind::WallTorch | BlockKind::Repeater | BlockKind::Lamp | BlockKind::Lever));
    if block.kind == BlockKind::RedstoneWire { block.power > 0 } else { block.lit }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let case = &args[1];
    let budget: u64 = args[2].parse().unwrap();
    let directory = std::path::Path::new(&args[3]);
    let netlist = match case.as_str() {
        "chain" | "pinned-chain" => Netlist {
            inputs: vec!["a".into()], outputs: vec!["y".into()],
            gates: vec![Gate::nor("m", &["a"]), Gate::nor("y", &["m"])],
        },
        "mux" => Netlist {
            inputs: vec!["a".into(), "b".into(), "s".into()], outputs: vec!["o".into()],
            gates: vec![Gate::nor("ns", &["s"]), Gate::nor("na", &["a"]),
                Gate::nor("nb", &["b"]), Gate::nor("x", &["na", "s"]),
                Gate::nor("y", &["nb", "ns"]), Gate::nor("z", &["x", "y"]),
                Gate::nor("o", &["z"])],
        },
        "and4" => reda::circuits::and4::build_and4_netlist().0,
        "native-seven" | "rom" => {
            use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
            let (file, top) = if case == "rom" {
                ("tests/fixtures/rom16x7.sv", "rom16x7")
            } else { ("tests/fixtures/seven_segment.v", "bcd_seven_segment") };
            let text = std::fs::read_to_string(file).unwrap();
            compile_systemverilog(&[SourceInput { name: file, text: &text }],
                &CompileOptions::new(top)).unwrap().netlist
        },
        _ => panic!("unknown case"),
    };
    let mut pins = PortPlacements::default();
    if case == "pinned-chain" {
        pins.pin("a", Anchor { x: 1, y: 1, z: 4 }, Facing::South);
        pins.pin("y", Anchor { x: 4, y: 1, z: 4 }, Facing::North);
    }
    let caller = (case == "pinned-chain").then_some(&pins);
    let result = match compile_fragment_synth(SynthesisInput {
        lowered: &netlist, source_provenance: None, pins: caller,
    }, SynthesisBudget::Evaluations(budget)) {
        Ok(result) => result,
        Err(error) => {
            println!("{}", json!({"kind":"refusal", "case":case, "budget":budget,
                "error":error.to_string()}));
            return;
        },
    };
    let compiled = &result.compiled;
    let fingerprint = canonical_world_fingerprint(&compiled.world);
    let artifact = directory.join(format!("{case}-b{budget}-{}.litematic", fingerprint.as_str()));
    reda::formats::litematic::save(&artifact, &compiled.world, "recursive search experiment").unwrap();
    let reloaded = reda::formats::litematic::load(&artifact).unwrap();
    assert_eq!(canonical_world_fingerprint(&reloaded), fingerprint);
    let mut definitions = BTreeMap::new();
    for gate in &netlist.gates {
        definitions.insert(format!("gate:{}", gate.output),
            (*compiled.gate_output_positions.get(&gate.output).unwrap(), gate.output.clone()));
    }
    for (index, output) in netlist.outputs.iter().enumerate() {
        definitions.insert(format!("driver:{index}"),
            (*compiled.gate_output_positions.get(output).unwrap(), output.clone()));
        definitions.insert(format!("lamp:{index}"),
            (*compiled.output_positions.get(output).unwrap(), output.clone()));
    }
    let mut measured = reloaded;
    if caller.is_some() {
        for output in &netlist.outputs {
            reda::compile::probe_caller_cell(&mut measured, compiled.output_positions[output]);
        }
    }
    let mut sim = Simulator::new(measured);
    sim.run_until_stable(2000).unwrap();
    let snapshot = |world: &World| definitions.iter().map(|(label, (at, _))|
        (label.clone(), signal(world, *at))).collect::<BTreeMap<_, _>>();
    sim.attach_observer(definitions.iter().map(|(label, (at, _))|
        (Position::new(at.0, at.1, at.2), label.clone())));
    println!("{}", json!({"kind":"source", "case":case, "budget":budget,
        "inputs":netlist.inputs, "outputs":netlist.outputs,
        "gates":netlist.gates.iter().map(|g| json!({"output":g.output,
            "inputs":g.inputs, "kind":format!("{:?}",g.kind)})).collect::<Vec<_>>(),
        "definitions":definitions, "power_on":snapshot(sim.world()),
        "world_fingerprint":fingerprint, "case_fingerprint":result.case_fingerprint,
        "trace":result.trace, "evaluations_used":result.evaluations_used,
        "stop_reason":result.stop_reason, "metrics":result.metrics, "tick_cap":2000}));
    let states = 1usize << netlist.inputs.len();
    for from in 0..states {
        for to in 0..states {
            for (phase, address) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                let start = sim.current_tick();
                for (bit, name) in netlist.inputs.iter().enumerate() {
                    let at = compiled.input_positions[name];
                    let on = address & (1 << bit) != 0;
                    if caller.is_some() {
                        reda::compile::drive_caller_cell(sim.world_mut(), at, on);
                    } else {
                        let mut lever = sim.world().get(at.0, at.1, at.2).clone();
                        assert_eq!(lever.kind, BlockKind::Lever);
                        lever.lit = on;
                        sim.world_mut().set(at.0, at.1, at.2, lever);
                    }
                }
                let ticks = sim.run_until_stable(2000).unwrap();
                println!("{}",json!({"kind":"phase", "from":from, "to":to,
                    "phase":phase, "address":address, "start":start, "ticks":ticks,
                    "signals":snapshot(sim.world()), "events":sim.observations().iter()
                        .map(|e| json!({"label":e.label,"tick":e.tick,"value":e.value}))
                        .collect::<Vec<_>>()}));
            }
        }
    }
    assert_eq!(canonical_world_fingerprint(&compiled.world), fingerprint);
    println!("{}",json!({"kind":"result", "phases":2*states*states}));
}

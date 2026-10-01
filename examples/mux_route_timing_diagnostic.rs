//! Retained-history Mux diagnosis; optional and4-yosys traces the AND control.
//! Output includes all gates and routed relays.
//! This records evidence; it does not choose or accept a generator candidate.
use reda::compile::{compile, lowering::lower_optimised, planner};
use reda::frontend::{compile_systemverilog, synthesize_verilog, CompileOptions, SourceInput};
use reda::redstone::{
    simulator::{position::Position, Simulator},
    world::block::BlockKind,
};
use serde_json::json;
use std::collections::HashMap;
fn main() {
    let and = std::env::args().nth(1).as_deref() == Some("and4-yosys");
    let names: &[&str] = if and {
        &["a", "b", "c", "d"]
    } else {
        &["a", "b", "s"]
    };
    let states = 1usize << names.len();
    let (source, ports) = if and {
        synthesize_verilog(include_str!("../tests/fixtures/and4.sv"), "and4").unwrap()
    } else {
        let artifact = compile_systemverilog(
            &[SourceInput {
                name: "probe.sv",
                text: "module mux2(input logic a,b,s, output logic y); assign y=s?b:a; endmodule",
            }],
            &CompileOptions::new("mux2"),
        )
        .unwrap();
        let ports = artifact.output_map();
        (artifact.netlist, ports)
    };
    let netlist = lower_optimised(&source).unwrap();
    let circuit = compile(&netlist).unwrap();
    let attempted = planner::plan_from_netlist(&netlist, &planner::PortPlacements::default());
    let refusal = attempted.as_ref().err().map(|e| format!("{e:?}"));
    let plan = attempted.ok();
    let fingerprint =
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&circuit.world);
    if let Some(plan) = &plan {
        let emitted = planner::emit_candidate(plan, &netlist, circuit.world.size()).unwrap();
        assert_eq!(
            fingerprint,
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&emitted.world)
        );
    }
    assert_eq!(
        fingerprint,
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
            &compile(&netlist).unwrap().world
        )
    );
    let mut sites = Vec::new();
    let mut routes = Vec::new();
    for gate in &netlist.gates {
        let (x, y, z) = circuit.gate_output_positions[&gate.output];
        sites.push((Position::new(x, y, z), format!("gate:{}", gate.output)));
    }
    let (x, y, z) = circuit.output_positions[&ports["y"]];
    sites.push((Position::new(x, y, z), "lamp".into()));
    for route in plan.as_ref().into_iter().flat_map(|p| p.routes()) {
        let mut relays = Vec::new();
        for (at, state) in route.anchors().iter().zip(route.realisation()) {
            if state.kind == BlockKind::Repeater {
                let label = format!(
                    "relay:{}:{},{},{}",
                    route.owner().unwrap(),
                    at.x,
                    at.y,
                    at.z
                );
                sites.push((Position::new(at.x, at.y, at.z), label.clone()));
                relays.push(json!({"label":label,"at":at,"state":state}));
            }
        }
        routes.push(json!({"owner":route.owner(),"terminals":format!("{:?}",route.terminals()),"paths":route.branch_paths(),"relays":relays}));
    }
    let mut unattributed = Vec::new();
    if plan.is_none() {
        for index in circuit.world.positions_of(BlockKind::Repeater) {
            let (x, y, z) = circuit.world.decode(index);
            let label = format!("unattributed-relay:{x},{y},{z}");
            sites.push((Position::new(x, y, z), label.clone()));
            unattributed.push(json!({"label":label,"at":[x,y,z],"state":circuit.world.get(x,y,z)}));
        }
    }
    let mut sim = Simulator::new(circuit.world.clone());
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(sites);
    let order = netlist.combinational_order().unwrap();
    let mut phases = Vec::new();
    for from in 0..states {
        for to in 0..states {
            for (phase, value) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                for (bit, name) in names.iter().enumerate() {
                    let (x, y, z) = circuit.input_positions[*name];
                    let mut state = sim.world().get(x, y, z).clone();
                    state.lit = value & (1 << bit) != 0;
                    sim.world_mut().set(x, y, z, state);
                }
                let start = sim.current_tick();
                let ticks = sim.run_until_stable(2000).unwrap();
                let mut values: HashMap<String, bool> = names
                    .iter()
                    .enumerate()
                    .map(|(bit, name)| (name.to_string(), value & (1 << bit) != 0))
                    .collect();
                for &index in &order {
                    let gate = &netlist.gates[index];
                    let expected = gate
                        .kind
                        .evaluate(&gate.inputs.iter().map(|n| values[n]).collect::<Vec<_>>());
                    values.insert(gate.output.clone(), expected);
                    let (x, y, z) = circuit.gate_output_positions[&gate.output];
                    let state = sim.world().get(x, y, z);
                    let actual = if state.kind == BlockKind::RedstoneWire {
                        state.power > 0
                    } else {
                        state.lit
                    };
                    assert_eq!(
                        actual, expected,
                        "gate {} {from}->{to} phase={phase}",
                        gate.output
                    );
                }
                let expected = if and {
                    value == 15
                } else if value & 4 != 0 {
                    value & 2 != 0
                } else {
                    value & 1 != 0
                };
                assert_eq!(sim.world().get(x, y, z).lit, expected);
                let observed =
                    reda::timing::observations_to_result(sim.observations(), start, ticks);
                let changes: std::collections::BTreeMap<_, _> = observed
                    .nets
                    .iter()
                    .map(|(n, v)| (n.clone(), json!(v.changes())))
                    .collect();
                phases.push(json!({"from":from,"to":to,"phase":phase,"value":value,"ticks":ticks,"changes":changes}));
            }
        }
    }
    println!("{}",serde_json::to_string_pretty(&json!({"input_bit_order":names,"planner":format!("{:?}",circuit.planner_kind()),"plan_refusal":refusal,"unattributed_relays":unattributed,"world_fingerprint":fingerprint,"netlist":format!("{netlist:?}"),"routes":routes,"phases":phases})).unwrap());
}

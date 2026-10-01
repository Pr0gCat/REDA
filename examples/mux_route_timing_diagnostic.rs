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
    let argument = std::env::args().nth(1);
    let and = argument.as_deref() == Some("and4-yosys");
    let xor = argument.as_deref() == Some("xor2");
    let names: &[&str] = if and {
        &["a", "b", "c", "d"]
    } else if xor {
        &["a", "b"]
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
                text: if xor {
                    "module xor2(input logic a,b, output logic y); assign y=a^b; endmodule"
                } else {
                    "module mux2(input logic a,b,s, output logic y); assign y=s?b:a; endmodule"
                },
            }],
            &CompileOptions::new(if xor { "xor2" } else { "mux2" }),
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
        for terminal in route.terminals() {
            let at = terminal.sink.anchor;
            sites.push((
                Position::new(at.x, at.y, at.z),
                format!(
                    "landing:{}->{}:{}",
                    route.owner().unwrap(),
                    terminal.sink.gate,
                    terminal.sink.input_index
                ),
            ));
        }
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
        let internal_relay_steps: Vec<_> = route.branch_paths().iter().flat_map(|path|path.windows(3)).filter_map(|w| {
            let cell=circuit.world.get(w[1].x,w[1].y,w[1].z);
            (cell.kind==BlockKind::Repeater).then(||json!({"previous":w[0],"at":w[1],"next":w[2],"strict_flat_path_legal":reda::compile::routing::route_step_is_legal(w[0],w[1],w[2],cell)}))
        }).collect();
        routes.push(json!({"owner":route.owner(),"terminals":format!("{:?}",route.terminals()),"paths":route.branch_paths(),"relays":relays,"internal_relay_steps":internal_relay_steps}));
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
    let balance = std::env::args().any(|a| a == "--balance-arrivals");
    let mut world = circuit.world.clone();
    let mut arrival: HashMap<String, u64> = netlist.inputs.iter().map(|n| (n.clone(), 0)).collect();
    let mut estimates = Vec::new();
    let mut probe_relays = Vec::new();
    if let Some(plan) = &plan {
        let mut usage = HashMap::new();
        for route in plan.routes() {
            for path in route.branch_paths() {
                for at in path {
                    *usage.entry(*at).or_insert(0usize) += 1;
                }
            }
        }
        for index in netlist.combinational_order().unwrap() {
            let gate = &netlist.gates[index];
            let mut inputs = Vec::new();
            for (port, name) in gate.inputs.iter().enumerate() {
                let path = plan
                    .routes()
                    .iter()
                    .find_map(|route| {
                        route.terminals().iter().zip(route.branch_paths()).find_map(
                            |(terminal, path)| {
                                (terminal.sink.gate == gate.name
                                    && terminal.sink.input_index as usize == port)
                                    .then_some(path)
                            },
                        )
                    })
                    .expect("fixture input must have a verified route");
                let delay: u64 = path
                    .iter()
                    .map(|at| {
                        let cell = world.get(at.x, at.y, at.z);
                        if cell.kind == BlockKind::Repeater {
                            2 * u64::from(cell.delay)
                        } else {
                            0
                        }
                    })
                    .sum();
                inputs.push((name, path, arrival[name] + delay));
            }
            let latest = inputs.iter().map(|(_, _, t)| *t).max().unwrap_or(0);
            let earliest = inputs.iter().map(|(_, _, t)| *t).min().unwrap_or(0);
            estimates.push(json!({"gate":gate.name,"inputs":inputs.iter().map(|(n,_,t)|json!({"signal":n,"nominal_arrival":t})).collect::<Vec<_>>(),"skew":latest-earliest}));
            for (name, path, time) in &inputs {
                if !balance {
                    continue;
                }
                let mut missing = latest - time;
                for window in path.windows(3).rev() {
                    if missing == 0 {
                        break;
                    }
                    let (prev, at, next) = (window[0], window[1], window[2]);
                    if prev.y != at.y
                        || at.y != next.y
                        || (at.x - prev.x, at.z - prev.z) != (next.x - at.x, next.z - at.z)
                    {
                        continue;
                    }
                    if usage[&at] != 1
                        || world.get(at.x, at.y, at.z).kind != BlockKind::RedstoneWire
                        || world.get(at.x, at.y - 1, at.z).kind != BlockKind::Solid
                    {
                        continue;
                    }
                    let toward = match (next.x - at.x, next.z - at.z) {
                        (1, 0) => reda::redstone::world::block::Facing::East,
                        (-1, 0) => reda::redstone::world::block::Facing::West,
                        (0, 1) => reda::redstone::world::block::Facing::South,
                        (0, -1) => reda::redstone::world::block::Facing::North,
                        _ => continue,
                    };
                    let mut relay = reda::redstone::world::block::BlockState::air();
                    relay.kind = BlockKind::Repeater;
                    relay.name = "minecraft:repeater".into();
                    relay.delay = 1;
                    relay.facing = Some(toward.opposite());
                    world.set(at.x, at.y, at.z, relay);
                    let label = format!("probe-relay:{name}:{},{},{}", at.x, at.y, at.z);
                    sites.push((Position::new(at.x, at.y, at.z), label.clone()));
                    probe_relays
                        .push(json!({"signal":name,"receiver":gate.name,"at":at,"label":label}));
                    missing = missing
                        .checked_sub(2)
                        .expect("nominal stages are whole repeater delays");
                }
                assert_eq!(
                    missing, 0,
                    "no private straight solid-backed cell for probe {name} -> {}",
                    gate.name
                );
            }
            arrival.insert(
                gate.output.clone(),
                latest + if gate.is_merge() { 0 } else { 2 },
            );
        }
    } else {
        assert!(!balance, "cannot balance an unattributed fallback world");
    }
    let cut = std::env::args().find_map(|a| a.strip_prefix("--cut-relay=").map(str::to_owned));
    if let Some(spec) = &cut {
        let coords: Vec<i32> = spec
            .split(',')
            .map(|s| s.parse().expect("cut relay coordinates"))
            .collect();
        assert_eq!(coords.len(), 3);
        assert_eq!(
            world.get(coords[0], coords[1], coords[2]).kind,
            BlockKind::Repeater
        );
        world.set(
            coords[0],
            coords[1],
            coords[2],
            reda::redstone::world::block::BlockState::air(),
        );
    }
    let final_fingerprint =
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world);
    let mut sim = Simulator::new(world);
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
                } else if xor {
                    (value & 1 != 0) != (value & 2 != 0)
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
    println!("{}",serde_json::to_string_pretty(&json!({"diagnostic_mutation":balance || cut.is_some(),"diagnostic_cut":cut,"arrival_estimates":estimates,"probe_relays":probe_relays,"original_world_fingerprint":fingerprint,"verified_ledger_matches_final_world":plan.is_some() && fingerprint == final_fingerprint,"input_bit_order":names,"planner":format!("{:?}",circuit.planner_kind()),"plan_refusal":refusal,"unattributed_relays":unattributed,"world_fingerprint":final_fingerprint,"netlist":format!("{netlist:?}"),"routes":routes,"phases":phases})).unwrap());
}

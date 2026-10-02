//! Diagnostic search over shared truth-table covers; no shipping policy changes.
use reda::compile::{
    compile_without_rewrite,
    fragment_synth::{legacy_adapter::LegacyCandidateAdapter, rewrite::realise_physical_rewrite},
    topology::Library,
    Gate, Netlist,
};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::{
    simulator::{position::Position, Simulator},
    world::{block::BlockKind, storage::World},
};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
#[path = "../src/frontend/cover.rs"]
mod cover;
fn evaluate(n: &Netlist, address: usize) -> HashMap<String, bool> {
    let mut v: HashMap<_, _> = n
        .inputs
        .iter()
        .enumerate()
        .map(|(i, s)| (s.clone(), address & (1 << i) != 0))
        .collect();
    for i in n.combinational_order().unwrap() {
        let g = &n.gates[i];
        let a: Vec<_> = g.inputs.iter().map(|s| v[s]).collect();
        v.insert(g.output.clone(), g.kind.evaluate(&a));
    }
    v
}
fn gate(n: &mut Netlist, mut operands: Vec<String>, nor: bool) -> String {
    while operands.len() > 3 {
        operands = operands
            .chunks(3)
            .map(|chunk| gate(n, chunk.to_vec(), false))
            .collect();
    }
    let mut name = format!("$cover_gate_{}", n.gates.len());
    while n.inputs.contains(&name) || n.gates.iter().any(|g| g.output == name) {
        name.push('_');
    }
    let refs: Vec<_> = operands.iter().map(String::as_str).collect();
    n.gates.push(if nor {
        Gate::nor(&name, &refs)
    } else {
        Gate::merge(&name, &refs)
    });
    name
}
fn candidate(source: &Netlist, polarity: usize) -> Netlist {
    let mut n = Netlist {
        inputs: source.inputs.clone(),
        outputs: vec![],
        gates: vec![],
    };
    let negatives: Vec<_> = source
        .inputs
        .iter()
        .map(|s| gate(&mut n, vec![s.clone()], true))
        .collect();
    let low = gate(
        &mut n,
        vec![source.inputs[0].clone(), negatives[0].clone()],
        true,
    );
    let high = gate(&mut n, vec![low.clone()], true);
    let mut shared = BTreeMap::new();
    let states = 1 << source.inputs.len();
    let values: Vec<_> = (0..states).map(|a| evaluate(source, a)).collect();
    for (i, out) in source.outputs.iter().enumerate() {
        let inverted = polarity & (1 << i) != 0;
        let table: Vec<_> = values.iter().map(|v| v[out]).collect();
        let mut terms = vec![];
        for cube in cover::cover(&table, !inverted) {
            let term = shared
                .entry((cube.bits, cube.mask))
                .or_insert_with(|| {
                    if cube.mask == 0 {
                        return high.clone();
                    }
                    let mismatches: Vec<_> = source
                        .inputs
                        .iter()
                        .enumerate()
                        .filter(|(b, _)| cube.mask & (1 << b) != 0)
                        .map(|(b, s)| {
                            if cube.bits & (1 << b) != 0 {
                                negatives[b].clone()
                            } else {
                                s.clone()
                            }
                        })
                        .collect();
                    if mismatches.len() <= 3 {
                        gate(&mut n, mismatches, true)
                    } else {
                        let joined = gate(&mut n, mismatches, false);
                        gate(&mut n, vec![joined], true)
                    }
                })
                .clone();
            terms.push(term);
        }
        let result = if terms.is_empty() {
            if inverted {
                high.clone()
            } else {
                low.clone()
            }
        } else if inverted && terms.len() <= 3 {
            gate(&mut n, terms, true)
        } else {
            let joined = gate(&mut n, terms, false);
            if inverted {
                gate(&mut n, vec![joined], true)
            } else {
                joined
            }
        };
        n.outputs.push(result);
    }
    n
}
fn signal(w: &World, p: (i32, i32, i32)) -> bool {
    let b = w.get(p.0, p.1, p.2);
    if b.kind == BlockKind::RedstoneWire {
        b.power > 0
    } else {
        b.lit
    }
}
fn measure(source: &Netlist, c: &reda::compile::CompiledCircuit) -> serde_json::Value {
    let implementation = c.implementation_netlist(source);
    let states = 1usize << source.inputs.len();
    let drivers = c.physical_rewrite().map_or_else(
        || {
            source
                .outputs
                .iter()
                .map(|s| c.gate_output_positions[s])
                .collect::<Vec<_>>()
        },
        |r| r.source_output_driver_positions().to_vec(),
    );
    let lamps = c.physical_rewrite().map_or_else(
        || {
            source
                .outputs
                .iter()
                .map(|s| c.output_positions[s])
                .collect::<Vec<_>>()
        },
        |r| r.source_output_positions().to_vec(),
    );
    let mut sim = Simulator::new(c.world.clone());
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(
        drivers
            .iter()
            .chain(&lamps)
            .enumerate()
            .map(|(i, &p)| (Position::new(p.0, p.1, p.2), i.to_string())),
    );
    let mut phases = vec![];
    for from in 0..states {
        for to in 0..states {
            for (phase, address) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                let start = sim.current_tick();
                for (i, s) in source.inputs.iter().enumerate() {
                    let p = c.input_positions[s];
                    let mut lever = sim.world().get(p.0, p.1, p.2).clone();
                    lever.lit = address & (1 << i) != 0;
                    sim.world_mut().set(p.0, p.1, p.2, lever);
                }
                sim.run_until_stable(2000).unwrap();
                let expected = evaluate(source, address);
                let internal = evaluate(implementation, address);
                for g in &implementation.gates {
                    assert_eq!(
                        signal(sim.world(), c.gate_output_positions[&g.output]),
                        internal[&g.output],
                        "internal {} at {}",
                        g.output,
                        address
                    );
                }
                for (i, out) in source.outputs.iter().enumerate() {
                    assert_eq!(signal(sim.world(), drivers[i]), expected[out]);
                    assert_eq!(signal(sim.world(), lamps[i]), expected[out]);
                }
                let measured_internal: BTreeMap<_, _> = implementation
                    .gates
                    .iter()
                    .map(|g| {
                        (
                            g.output.clone(),
                            signal(sim.world(), c.gate_output_positions[&g.output]),
                        )
                    })
                    .collect();
                phases.push(json!({"internal":measured_internal,"driver":drivers.iter().map(|&p|signal(sim.world(),p)).collect::<Vec<_>>(),"lamp":lamps.iter().map(|&p|signal(sim.world(),p)).collect::<Vec<_>>(),"from":from,"to":to,"phase":phase,"address":address,"ticks":sim.current_tick()-start,"expected":source.outputs.iter().map(|s|expected[s]).collect::<Vec<_>>(),"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>() }));
            }
        }
    }
    let exact_cells: Vec<_> = c
        .world
        .cells()
        .iter()
        .enumerate()
        .filter_map(|(flat, &index)| {
            let block = c.world.palette().get(index).unwrap();
            (block.kind != BlockKind::Air).then(|| (c.world.decode(flat), block))
        })
        .collect();
    let exact = reda::compile::metrics::canonical_fingerprint(
        &serde_json::to_vec(&(c.world.size(), exact_cells)).unwrap(),
    );
    let gates = |n: &Netlist| {
        n.gates
            .iter()
            .map(|g| json!({"output":g.output,"inputs":g.inputs,"kind":g.kind.wire_name()}))
            .collect::<Vec<_>>()
    };
    json!({"source_gates":gates(source),"implemented_inputs":implementation.inputs,"implemented_gates":gates(implementation),"strict_certified":c.physical_rewrite().is_some(),"exact_world_fingerprint":exact,"phases":phases,"fingerprint":reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&c.world)})
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let text = std::fs::read_to_string(&args[1]).unwrap();
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: &args[1],
            text: &text,
        }],
        &CompileOptions::new(&args[2]),
    )
    .unwrap();
    let input = if args.iter().any(|a| a == "--yosys") {
        reda::frontend::synthesize_verilog(&text, &args[2])
            .unwrap()
            .0
    } else {
        artifact.netlist
    };
    let source = reda::compile::lowering::lower_optimised(&input).unwrap();
    assert!((1..=6).contains(&source.inputs.len()));
    assert!(source.outputs.len() <= 16);
    assert!(
        source.gates.iter().all(|g| !g.kind.is_sequential()),
        "this probe qualifies combinational covers"
    );
    let count = args.get(3).map_or(2, |s| s.parse::<usize>().unwrap());
    assert!(count <= 1 << source.outputs.len());
    for index in 0..=count {
        let polarity = if index == 0 { None } else { Some(index - 1) };
        let implementation = polarity.map_or_else(|| source.clone(), |p| candidate(&source, p));
        let result = (|| -> Result<_, String> {
            let raw =
                compile_without_rewrite(&implementation).map_err(|e| format!("compile: {e:?}"))?;
            if index == 0 && args.iter().any(|a| a == "--control") {
                return Ok(measure(&source, &raw));
            }
            if index == 0 && args.iter().any(|a| a == "--flat-refresh") {
                let c = reda::compile::selection::compile_flat_refresh_candidate(&source)
                    .map_err(|e| format!("strict flat draft: {e:?}"))?;
                return Ok(measure(&source, &c));
            }
            if index == 0 && args.iter().any(|a| a == "--default") {
                let c = reda::compile::compile(&source)
                    .map_err(|e| format!("default compile: {e:?}"))?;
                return Ok(measure(&source, &c));
            }
            let mut adapted = LegacyCandidateAdapter::adapt(&implementation, &raw)
                .map_err(|e| format!("manifest: {e:?}"))?;
            let moved = if args.iter().any(|a| a == "--flat-refresh") {
                reda::compile::fragment_synth::refresh::flatten_refreshes(&mut adapted.candidate)
            } else {
                0
            };
            eprintln!("polarity {polarity:?}: moved {moved} refreshes");
            let rewrite = realise_physical_rewrite(
                &source,
                &implementation,
                &adapted.candidate,
                &Library::default_library(),
                &implementation.inputs,
                &implementation.outputs,
                1_000_000,
            )
            .map_err(|e| {
                let mut bad = Vec::new();
                let mut illegal=Vec::new();
                for route in adapted.candidate.routes.values() {
                    for branch in &route.branches {
                        for t in branch.path.windows(3) {
                            let b=raw.world.get(t[1].x,t[1].y,t[1].z);
                            if !reda::compile::routing::route_step_is_legal(t[0],t[1],t[2],b) {illegal.push(json!({"route":route.id,"sink":branch.sink,"previous":t[0],"at":t[1],"next":t[2],"state":b}));}
                        }
                        for pair in branch.path.windows(2) {
                            let a=pair[0];let b=pair[1];
                            if raw.world.get(a.x,a.y,a.z).kind!=BlockKind::RedstoneWire || raw.world.get(b.x,b.y,b.z).kind!=BlockKind::RedstoneWire {continue;}
                            let connected=reda::redstone::simulator::position::HORIZONTAL.into_iter().any(|direction|reda::redstone::simulator::connectivity::dust_connections(&raw.world,Position::new(a.x,a.y,a.z),direction).iter().any(|p|p==Position::new(b.x,b.y,b.z)));
                            if !connected {bad.push(json!({"route":route.id,"sink":branch.sink,"from":a,"to":b,"headroom":raw.world.get(a.x,a.y+1,a.z),"step_floor":raw.world.get(b.x,a.y,b.z)}));}
                        }
                    }
                }
                format!("strict proof: {e:?}; disconnected dust diagnostics: {}",json!({"dust":bad,"illegal_steps":illegal}))
            })?;
            Ok(measure(
                &source,
                &reda::compile::CompiledCircuit::from_certified_rewrite(rewrite),
            ))
        })();
        println!(
            "{}",
            json!({"polarity":polarity,"source_inputs":source.inputs,"source_outputs":source.outputs,"result":result})
        );
    }
}

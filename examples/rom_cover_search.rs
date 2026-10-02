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
use std::collections::{BTreeMap, HashMap, HashSet};
#[path = "../src/frontend/cover.rs"]
mod cover;
fn evaluate(n: &Netlist, address: usize, input_order: &[String]) -> HashMap<String, bool> {
    let mut v: HashMap<_, _> = input_order
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
fn gate(n: &mut Netlist, mut operands: Vec<String>, nor: bool, fuse: bool) -> String {
    while operands.len() > 3 {
        operands = operands
            .chunks(3)
            .map(|chunk| {
                if fuse && chunk.len() == 1 {
                    chunk[0].clone()
                } else {
                    gate(n, chunk.to_vec(), false, fuse)
                }
            })
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
fn candidate(source: &Netlist, polarity: usize, fuse: bool) -> Netlist {
    let mut n = Netlist {
        inputs: source.inputs.clone(),
        outputs: vec![],
        gates: vec![],
    };
    let negatives: Vec<_> = source
        .inputs
        .iter()
        .map(|s| gate(&mut n, vec![s.clone()], true, fuse))
        .collect();
    let low = gate(
        &mut n,
        vec![source.inputs[0].clone(), negatives[0].clone()],
        true,
        fuse,
    );
    let high = gate(&mut n, vec![low.clone()], true, fuse);
    let mut shared = BTreeMap::new();
    let states = 1 << source.inputs.len();
    let values: Vec<_> = (0..states)
        .map(|a| evaluate(source, a, &source.inputs))
        .collect();
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
                    if fuse || mismatches.len() <= 3 {
                        gate(&mut n, mismatches, true, fuse)
                    } else {
                        let joined = gate(&mut n, mismatches, false, fuse);
                        gate(&mut n, vec![joined], true, fuse)
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
        } else if inverted && (fuse || terms.len() <= 3) {
            gate(&mut n, terms, true, fuse)
        } else {
            let joined = gate(&mut n, terms, false, fuse);
            if inverted {
                gate(&mut n, vec![joined], true, fuse)
            } else {
                joined
            }
        };
        n.outputs.push(result);
    }
    n
}
// Experimental route mutation. Whole-candidate proof and retained quality remain mandatory.
fn balance_primary(
    n: &Netlist,
    candidate: &mut reda::compile::fragment_synth::candidate::ExpandedPhysicalCandidate,
    terminal_mode: bool,
) -> usize {
    use reda::compile::fragment_synth::identity::ConnectionId;
    use reda::compile::routing::{DelayedOwner, RouteTarget, RouteTerminalKind};
    use reda::redstone::world::block::Facing;
    // This experiment starts from unit-delay routes; counts are delay units only there.
    if candidate
        .routes
        .values()
        .flat_map(|r| &r.cells)
        .any(|c| c.state.kind == BlockKind::Repeater && c.state.delay != 1)
    {
        return 0;
    }
    let mut proposals = vec![];
    for instance in &candidate.instances.instances {
        let g = &n.gates[instance.logical_gate.0 as usize];
        if g.kind.wire_name() != "nor"
            || g.inputs.len() < 2
            || !g.inputs.iter().all(|s| n.inputs.contains(s))
        {
            continue;
        }
        let branches: Vec<_> = candidate
            .routes
            .values()
            .flat_map(|route| {
                route
                    .branches
                    .iter()
                    .filter_map(move |branch| match branch.target {
                        RouteTarget::Connection(ConnectionId::External {
                            instance: receiver,
                            ..
                        }) if receiver == instance.id => Some((route, branch)),
                        _ => None,
                    })
            })
            .collect();
        if branches.len() != g.inputs.len() {
            continue;
        }
        let maximum = branches
            .iter()
            .map(|(_, b)| b.terminal.repeaters)
            .max()
            .unwrap();
        for (route, branch) in branches {
            let delta = maximum - branch.terminal.repeaters;
            if delta == 0 || delta > 4 || branch.path.len() < 2 {
                continue;
            }
            if !terminal_mode {
                if let Some(cell) = branch
                    .path
                    .iter()
                    .rev()
                    .filter_map(|at| route.cells.iter().find(|c| c.at == *at))
                    .find(|cell| {
                        cell.at != branch.terminal.at
                            && cell.state.kind == BlockKind::Repeater
                            && u64::from(cell.state.delay) + delta <= 4
                            && route
                                .branches
                                .iter()
                                .filter(|b| b.path.contains(&cell.at))
                                .count()
                                == 1
                            && !candidate
                                .observations
                                .values()
                                .any(|o| o.site.at == cell.at)
                    })
                {
                    let mut state = cell.state.clone();
                    state.delay += delta as u8;
                    proposals.push((route.id, branch.sink, cell.at, state));
                }
                continue;
            }
            let at = branch.terminal.at;
            if route
                .branches
                .iter()
                .filter(|b| b.path.contains(&at))
                .count()
                != 1
                || candidate.observations.values().any(|o| o.site.at == at)
            {
                continue;
            }
            let previous = branch.path[branch.path.len() - 2];
            let Some(cell) = route.cells.iter().find(|c| c.at == at) else {
                continue;
            };
            let mut state = cell.state.clone();
            if state.kind == BlockKind::RedstoneWire {
                if previous.y != at.y {
                    continue;
                }
                let facing = match (at.x - previous.x, at.z - previous.z) {
                    (1, 0) => Facing::West,
                    (-1, 0) => Facing::East,
                    (0, 1) => Facing::North,
                    (0, -1) => Facing::South,
                    _ => continue,
                };
                state.kind = BlockKind::Repeater;
                state.name = "minecraft:repeater".into();
                state.facing = Some(facing);
                state.power = 0;
                state.delay = delta as u8;
                state.lit = true;
            } else if state.kind == BlockKind::Repeater && u64::from(state.delay) + delta <= 4 {
                state.delay += delta as u8;
            } else {
                continue;
            }
            proposals.push((route.id, branch.sink, at, state));
        }
    }
    for (route_id, sink, at, state) in &proposals {
        let route = candidate.routes.get_mut(route_id).unwrap();
        let branch = route.branches.iter_mut().find(|b| b.sink == *sink).unwrap();
        let cell = route.cells.iter_mut().find(|c| c.at == *at).unwrap();
        if cell.state.kind == BlockKind::RedstoneWire {
            branch.terminal.repeaters += 1;
        }
        cell.state = state.clone();
        if *at != branch.terminal.at {
            continue;
        }
        branch.terminal.state = state.clone();
        branch.terminal.kind = RouteTerminalKind::RepeaterIntoSupport;
        branch.terminal.delayed_owner = Some(DelayedOwner::Route(*route_id));
    }
    proposals.len()
}
fn signal(w: &World, p: (i32, i32, i32)) -> bool {
    let b = w.get(p.0, p.1, p.2);
    if b.kind == BlockKind::RedstoneWire {
        b.power > 0
    } else {
        b.lit
    }
}
fn measure(
    source: &Netlist,
    c: &reda::compile::CompiledCircuit,
    trace: bool,
    trace_landings: bool,
) -> Result<serde_json::Value, String> {
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
    let mut watched: Vec<_> = drivers
        .iter()
        .chain(&lamps)
        .enumerate()
        .map(|(i, &p)| (Position::new(p.0, p.1, p.2), i.to_string()))
        .collect();
    if trace {
        watched.extend(implementation.gates.iter().map(|g| {
            let p = c.gate_output_positions[&g.output];
            (
                Position::new(p.0, p.1, p.2),
                format!("internal/{}", g.output),
            )
        }));
    }
    let mut landings = BTreeMap::new();
    if trace_landings {
        use reda::compile::fragment_synth::identity::ConnectionId;
        use reda::compile::routing::RouteTarget;
        let adapted;
        let manifest = if let Some(rewrite) = c.physical_rewrite() {
            rewrite.implemented_candidate()
        } else {
            adapted = LegacyCandidateAdapter::adapt(implementation, c).unwrap();
            &adapted.candidate
        };
        for route in manifest.routes.values() {
            for branch in &route.branches {
                let RouteTarget::Connection(ConnectionId::External {
                    instance,
                    input_index,
                }) = branch.target
                else {
                    continue;
                };
                let owner = manifest
                    .instances
                    .instances
                    .iter()
                    .find(|i| i.id == instance)
                    .unwrap();
                let g = &implementation.gates[owner.logical_gate.0 as usize];
                let terminal = branch.terminal.at;
                let mut sites = vec![("terminal", terminal)];
                if let Some(at) = branch.path.iter().rev().find(|at| {
                    c.world.get(at.x, at.y, at.z).kind == BlockKind::Repeater && **at != terminal
                }) {
                    sites.push(("last_repeater", *at));
                }
                for (role, at) in sites {
                    let state = c.world.get(at.x, at.y, at.z);
                    assert!(
                        matches!(state.kind, BlockKind::RedstoneWire | BlockKind::Repeater),
                        "unsupported landing observation: {state:?}"
                    );
                    let name = format!("{}/{}/{role}", route.id.0, branch.sink.ordinal);
                    watched.push((Position::new(at.x, at.y, at.z), format!("landing/{name}")));
                    landings.insert(name, json!({"role":role,"at":at,"state":state,"receiver":g.output,"receiver_at":c.gate_output_positions[&g.output],"input_index":input_index,"source":g.inputs[input_index as usize],"receiver_kind":g.kind.wire_name(),"terminal":branch.terminal,"path":branch.path}));
                }
            }
        }
    }
    sim.attach_observer(watched);
    let landing_values = |sim: &Simulator| {
        landings
            .iter()
            .map(|(name, site)| {
                let at = &site["at"];
                (
                    name.clone(),
                    signal(
                        sim.world(),
                        (
                            at["x"].as_i64().unwrap() as i32,
                            at["y"].as_i64().unwrap() as i32,
                            at["z"].as_i64().unwrap() as i32,
                        ),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    let initial_landings = landing_values(&sim);
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
                let expected = evaluate(source, address, &source.inputs);
                let internal = evaluate(implementation, address, &source.inputs);
                for g in &implementation.gates {
                    let actual = signal(sim.world(), c.gate_output_positions[&g.output]);
                    if actual != internal[&g.output] {
                        return Err(format!("qualification: internal {} at from {from} to {to} phase {phase} address {address}: expected {}, actual {actual}",g.output,internal[&g.output]));
                    }
                }
                for (i, out) in source.outputs.iter().enumerate() {
                    if signal(sim.world(), drivers[i]) != expected[out]
                        || signal(sim.world(), lamps[i]) != expected[out]
                    {
                        return Err(format!("qualification: public output {i} at from {from} to {to} phase {phase} address {address}"));
                    }
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
                phases.push(json!({"internal":measured_internal,"driver":drivers.iter().map(|&p|signal(sim.world(),p)).collect::<Vec<_>>(),"lamp":lamps.iter().map(|&p|signal(sim.world(),p)).collect::<Vec<_>>(),"from":from,"to":to,"phase":phase,"address":address,"ticks":sim.current_tick()-start,"expected":source.outputs.iter().map(|s|expected[s]).collect::<Vec<_>>(),"events":sim.observations().iter().filter(|e|e.label.parse::<usize>().is_ok()).map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>() }));
                if trace_landings {
                    let values = landing_values(&sim);
                    phases.last_mut().unwrap()["landings"] = json!(values);
                    phases.last_mut().unwrap()["landing_events"] = json!(sim
                        .observations()
                        .iter()
                        .filter_map(|e| e.label.strip_prefix("landing/").map(
                            |name| json!({"landing":name,"tick":e.tick-start,"value":e.value})
                        ))
                        .collect::<Vec<_>>());
                }
                if trace {
                    phases.last_mut().unwrap()["internal_events"] = json!(sim
                        .observations()
                        .iter()
                        .filter_map(|e| e
                            .label
                            .strip_prefix("internal/")
                            .map(|name| json!({"signal":name,"tick":e.tick-start,"value":e.value})))
                        .collect::<Vec<_>>());
                }
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
    let mut result = json!({"source_gates":gates(source),"implemented_inputs":c.physical_rewrite().map_or(&implementation.inputs[..], |r| r.rewrite_certificate().rewritten_inputs_in_source_order()),"implemented_gates":gates(implementation),"strict_certified":c.physical_rewrite().is_some(),"exact_world_fingerprint":exact,"phases":phases,"fingerprint":reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&c.world)});
    if trace_landings {
        result["landing_sites"] = json!(landings);
        result["initial_landings"] = json!(initial_landings);
    }
    if trace {
        result["gate_positions"] = json!(c.gate_output_positions);
        result["implemented_outputs"] = json!(implementation.outputs);
    }
    Ok(result)
}

fn search_refreshes(source: &Netlist, budget: usize, trace: bool, trace_landings: bool) {
    use reda::compile::fragment_synth::refresh::{removable_flat_refreshes, remove_flat_refresh};
    let baseline = reda::compile::selection::compile_flat_refresh_candidate(source).unwrap();
    let manifest = baseline.physical_rewrite().unwrap().implemented_candidate();
    let sites = removable_flat_refreshes(manifest);
    let baseline_quality = reda::compile::selection::qualify_boundary(source, &baseline).unwrap();
    println!(
        "{}",
        json!({"polarity":null,"search":"control","eligible":sites.len(),"budget":budget,"source_inputs":source.inputs,"source_outputs":source.outputs,"result":measure(source,&baseline,trace,trace_landings)})
    );
    for (ordinal, &(route, at)) in sites.iter().take(budget).enumerate() {
        let mut draft = manifest.clone();
        assert!(remove_flat_refresh(&mut draft, route, at));
        let result = (|| -> Result<_, String> {
            let rewrite = realise_physical_rewrite(
                source,
                source,
                &draft,
                &Library::default_library(),
                &source.inputs,
                &source.outputs,
                1_000_000,
            )
            .map_err(|e| format!("strict refresh removal: {e:?}"))?;
            let c = reda::compile::CompiledCircuit::from_certified_rewrite(rewrite);
            let measured = measure(source, &c, trace, trace_landings)?;
            let q = reda::compile::selection::qualify_boundary(source, &c)
                .ok_or("retained qualification failed")?;
            eprintln!(
                "refresh {ordinal}: no regression={}, strict improvement={}",
                q.dominates(&baseline_quality),
                q != baseline_quality
            );
            Ok(measured)
        })();
        println!(
            "{}",
            json!({"polarity":null,"search":"single_refresh_removal","ordinal":ordinal,"route":route,"at":at,"source_inputs":source.inputs,"source_outputs":source.outputs,"result":result})
        );
    }
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
    let terminal_mode = args.iter().any(|a| a == "--balance-terminal");
    let balance = terminal_mode || args.iter().any(|a| a == "--balance-primary");
    let trace_landings = args.iter().any(|a| a == "--trace-landings");
    let trace = trace_landings || args.iter().any(|a| a == "--trace");
    if let Some(value) = args
        .iter()
        .find_map(|a| a.strip_prefix("--refresh-search="))
    {
        search_refreshes(&source, value.parse().unwrap(), trace, trace_landings);
        return;
    }
    for index in 0..=count {
        let polarity = if index == 0 { None } else { Some(index - 1) };
        let mut implementation = polarity.map_or_else(
            || source.clone(),
            |p| candidate(&source, p, args.iter().any(|a| a == "--fused-reductions")),
        );
        if polarity.is_some() && args.iter().any(|a| a == "--reverse-inputs") {
            implementation.inputs.reverse();
        }
        if polarity.is_some() && args.iter().any(|a| a == "--prune-unused") {
            let mut live: HashSet<_> = implementation.outputs.iter().cloned().collect();
            for i in implementation
                .combinational_order()
                .unwrap()
                .into_iter()
                .rev()
            {
                let g = &implementation.gates[i];
                if live.contains(&g.output) {
                    live.extend(g.inputs.iter().cloned());
                }
            }
            implementation.gates.retain(|g| live.contains(&g.output));
        }
        let result = (|| -> Result<_, String> {
            let raw =
                compile_without_rewrite(&implementation).map_err(|e| format!("compile: {e:?}"))?;
            if index == 0 && args.iter().any(|a| a == "--control") {
                return measure(&source, &raw, trace, trace_landings);
            }
            if index == 0 && !balance && args.iter().any(|a| a == "--flat-refresh") {
                let c = reda::compile::selection::compile_flat_refresh_candidate(&source)
                    .map_err(|e| format!("strict flat draft: {e:?}"))?;
                return measure(&source, &c, trace, trace_landings);
            }
            if index == 0 {
                if let Some(value) = args
                    .iter()
                    .find_map(|a| a.strip_prefix("--selected-budget="))
                {
                    let c = reda::compile::selection::compile_selected_with_refresh_budget(
                        &source,
                        value.parse().unwrap(),
                    )
                    .map_err(|e| format!("selected compile: {e:?}"))?;
                    return measure(&source, &c, trace, trace_landings);
                }
            }
            if index == 0 && args.iter().any(|a| a == "--default") {
                let c = reda::compile::compile(&source)
                    .map_err(|e| format!("default compile: {e:?}"))?;
                return measure(&source, &c, trace, trace_landings);
            }
            let mut adapted = LegacyCandidateAdapter::adapt(&implementation, &raw)
                .map_err(|e| format!("manifest: {e:?}"))?;
            let moved = if args.iter().any(|a| a == "--flat-refresh") {
                reda::compile::fragment_synth::refresh::flatten_refreshes(&mut adapted.candidate)
            } else {
                0
            };
            if balance {
                let changed =
                    balance_primary(&implementation, &mut adapted.candidate, terminal_mode);
                eprintln!("polarity {polarity:?}: balanced {changed} primary landings");
            }
            eprintln!("polarity {polarity:?}: moved {moved} refreshes");
            let rewrite = realise_physical_rewrite(
                &source,
                &implementation,
                &adapted.candidate,
                &Library::default_library(),
                &source.inputs,
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
            measure(
                &source,
                &reda::compile::CompiledCircuit::from_certified_rewrite(rewrite),
                trace,
                trace_landings,
            )
        })();
        println!(
            "{}",
            json!({"polarity":polarity,"source_inputs":source.inputs,"source_outputs":source.outputs,"result":result})
        );
    }
}

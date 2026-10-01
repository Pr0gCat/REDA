//! Manual ROM truth/history observer with an optional uncertified relay probe.
use reda::compile::{compile, lowering::lower_optimised};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};
use serde_json::{json, Value};
use std::collections::HashMap;

// Diagnostic mutation only: the original candidate ledger is not retimed.
fn relocate_entries(
    source: &reda::compile::Netlist,
    world: &mut reda::redstone::world::storage::World,
) -> Value {
    use reda::compile::planner::{emit_candidate, plan_from_netlist, PortPlacements};
    use reda::compile::routing::{route_step_is_legal, RouteTerminalKind};
    let plan = match plan_from_netlist(source, &PortPlacements::default()) {
        Ok(p) => p,
        Err(e) => return json!({"status":"refused","reason":format!("{e:?}")}),
    };
    let emitted = match emit_candidate(&plan, source, world.size()) {
        Ok(world) => world,
        Err(e) => return json!({"status":"refused","reason":format!("{e:?}")}),
    };
    let fp = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
    if fp(&emitted.world) != fp(world) {
        return json!({"status":"refused","reason":"independent plan differs from actual world"});
    }
    let receivers: std::collections::HashSet<_> = plan
        .routes()
        .iter()
        .filter(|r| {
            r.owner()
                .is_some_and(|n| source.outputs.iter().any(|o| o == n))
        })
        .flat_map(|r| {
            r.terminals()
                .iter()
                .filter(|t| {
                    t.kind == RouteTerminalKind::RepeaterIntoSupport
                        && source
                            .gates
                            .iter()
                            .any(|g| g.name == t.sink.gate && g.inputs.len() > 1)
                })
                .map(|t| t.sink.gate.clone())
        })
        .collect();
    let mut usage = HashMap::new();
    for route in plan.routes() {
        for path in route.branch_paths() {
            for at in path {
                *usage.entry(*at).or_insert(0usize) += 1;
            }
        }
    }
    let mut candidate = world.clone();
    let mut changes = Vec::new();
    for route in plan.routes() {
        for (terminal, path) in route.terminals().iter().zip(route.branch_paths()) {
            if !receivers.contains(&terminal.sink.gate)
                || terminal.kind != RouteTerminalKind::RepeaterIntoSupport
            {
                continue;
            }
            if path.len() < 3 {
                return json!({"status":"refused","reason":"short private approach"});
            }
            let n = path.len();
            let (prev, at, next) = (path[n - 3], path[n - 2], path[n - 1]);
            let relay = world.get(next.x, next.y, next.z).clone();
            if usage.get(&at) != Some(&1)
                || candidate.get(at.x, at.y, at.z).kind != BlockKind::RedstoneWire
                || candidate.get(at.x, at.y - 1, at.z).kind != BlockKind::Solid
                || !route_step_is_legal(prev, at, next, &relay)
            {
                return json!({"status":"refused","reason":"no private straight solid-backed predecessor","gate":terminal.sink.gate});
            }
            let mut dust = reda::redstone::world::block::BlockState::air();
            dust.kind = BlockKind::RedstoneWire;
            dust.name = "minecraft:redstone_wire".into();
            candidate.set(at.x, at.y, at.z, relay);
            candidate.set(next.x, next.y, next.z, dust);
            changes.push(json!({"owner":route.owner(),"receiver":terminal.sink.gate,"relay_from":next,"relay_to":at}));
        }
    }
    *world = candidate;
    json!({"status":"mutated","moves":changes})
}

fn probe(encoding: usize, relocate: bool) -> Value {
    let words: [u8; 4] = std::array::from_fn(|i| ((encoding >> (3 * i)) & 7) as u8);
    let mut text = String::from("module probe(input logic a,b,output logic y0,y1,y2);");
    for bit in 0..3 {
        let bits = words.map(|word| (word >> bit) & 1);
        text.push_str(&format!(
            "assign y{bit}=b ? (a ? 1'b{} : 1'b{}) : (a ? 1'b{} : 1'b{});",
            bits[3], bits[2], bits[1], bits[0]
        ));
    }
    text.push_str("endmodule");
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "probe.sv",
            text: &text,
        }],
        &CompileOptions::new("probe"),
    )
    .unwrap();
    let ports = artifact.output_map();
    let mut source = lower_optimised(&artifact.netlist).unwrap();
    source.outputs = (0..3).map(|i| ports[&format!("y{i}")].clone()).collect();
    let mut seen = std::collections::BTreeSet::new();
    source.outputs.retain(|n| seen.insert(n.clone()));
    let circuit = match compile(&source) {
        Ok(c) => c,
        Err(e) => {
            return json!({"encoding":encoding,"words":words,"status":"compile_refusal","reason":format!("{e:?}")})
        }
    };
    let fingerprint =
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&circuit.world);
    let repeated = compile(&source).unwrap();
    assert_eq!(
        fingerprint,
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&repeated.world)
    );
    let mut world = circuit.world.clone();
    let diagnostic = if relocate {
        relocate_entries(&source, &mut world)
    } else {
        Value::Null
    };
    let final_fingerprint =
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world);
    let measure = || -> Value {
        let mut sim = Simulator::new(world);
        if let Err(e) = sim.run_until_stable(2000) {
            return json!({"encoding":encoding,"words":words,"status":"initial_settling_failure","reason":format!("{e:?}")});
        }
        let order = source.combinational_order().unwrap();
        let mut phases = Vec::new();
        for from in 0..4usize {
            for to in 0..4usize {
                for (phase, address) in [from, to].into_iter().enumerate() {
                    let mut values: HashMap<String, bool> = HashMap::from([
                        ("a".into(), address & 1 != 0),
                        ("b".into(), address & 2 != 0),
                    ]);
                    for name in ["a", "b"] {
                        let (x, y, z) = circuit.input_positions[name];
                        let mut state = sim.world().get(x, y, z).clone();
                        state.lit = values[name];
                        sim.world_mut().set(x, y, z, state);
                    }
                    let ticks = match sim.run_until_stable(2000) {
                        Ok(t) => t,
                        Err(e) => {
                            return json!({"encoding":encoding,"words":words,"status":"settling_failure","from":from,"to":to,"phase":phase,"reason":format!("{e:?}"),"completed_phases":phases})
                        }
                    };
                    let mut failures = Vec::new();
                    for &index in &order {
                        let gate = &source.gates[index];
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
                        if actual != expected {
                            failures.push(json!({"gate":gate.output,"at":[x,y,z],"expected":expected,"actual":actual,"state":state}));
                        }
                    }
                    for bit in 0..3 {
                        let (x, y, z) = circuit.output_positions[&ports[&format!("y{bit}")]];
                        let expected = words[address] & (1 << bit) != 0;
                        let actual = sim.world().get(x, y, z).lit;
                        if actual != expected {
                            failures.push(json!({"public_bit":bit,"at":[x,y,z],"expected":expected,"actual":actual}));
                        }
                    }
                    phases.push(
                        json!({"from":from,"to":to,"phase":phase,"address":address,"ticks":ticks}),
                    );
                    if !failures.is_empty() {
                        return json!({"encoding":encoding,"words":words,"status":"incorrect_physics","planner":format!("{:?}",circuit.planner_kind()),"fingerprint":fingerprint,"netlist":format!("{source:?}"),"failures":failures,"completed_phases":phases});
                    }
                }
            }
        }
        json!({"encoding":encoding,"words":words,"status":"passed","planner":format!("{:?}",circuit.planner_kind()),"fingerprint":fingerprint,"phases":phases})
    };
    let mut report = measure();
    if relocate {
        report["diagnostic"] = diagnostic;
        report["final_world_fingerprint"] = json!(final_fingerprint);
        report["retimed_candidate_certified"] = json!(false);
        report["world_mutated"] = json!(final_fingerprint != fingerprint);
    }
    report
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let start: usize = args.get(1).map(|a| a.parse().unwrap()).unwrap_or(0);
    let end: usize = args.get(2).map(|a| a.parse().unwrap()).unwrap_or(4096);
    assert!(start < end && end <= 4096);
    let relocate = args.iter().any(|a| a == "--weak-public-entry");
    let mut failed = false;
    for encoding in start..end {
        let result = probe(encoding, relocate);
        failed |= result["status"] != "passed";
        println!("{result}");
        if (encoding + 1) % 64 == 0 {
            eprintln!("completed encoding {}", encoding);
        }
    }
    if failed {
        std::process::exit(1);
    }
}

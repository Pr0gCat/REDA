//! A single-input transition must preserve the public output's false value.
use reda::compile::{compile, lowering::lower_optimised};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use serde_json::json;

#[test]
fn trace_yosys_single_input_transition() {
    let (netlist, outputs) = reda::frontend::synthesize_verilog(
        include_str!("fixtures/seven_segment.v"),
        "bcd_seven_segment",
    )
    .unwrap();
    let lowered = lower_optimised(&netlist).unwrap();
    let compiled = compile(&lowered).unwrap();
    let mut watched = reda::timing::watch_all_nets(&compiled);
    let output = compiled.output_positions[&outputs["a"]];
    watched.push((
        reda::redstone::simulator::position::Position::new(output.0, output.1, output.2),
        "public_a".to_string(),
    ));
    let positions: Vec<_> = ["d0", "d1", "d2", "d3"]
        .iter()
        .map(|name| compiled.input_positions[*name])
        .collect();
    let mut sim = Simulator::new(compiled.world);
    for &(x, y, z) in &positions {
        let mut block = sim.world().get(x, y, z).clone();
        block.lit = false;
        sim.world_mut().set(x, y, z, block);
    }
    sim.run_until_stable(2000).unwrap();
    assert!(sim.world().get(output.0, output.1, output.2).lit);
    sim.attach_observer(watched);
    let timing = reda::timing::measure_transition(&mut sim, 2000, |sim| {
        let (x, y, z) = positions[1];
        let mut block = sim.world().get(x, y, z).clone();
        block.lit = true;
        sim.world_mut().set(x, y, z, block);
    })
    .unwrap();
    assert!(sim.world().get(output.0, output.1, output.2).lit);
    let report = json!({"from":0,"to":2,"output_map":outputs,
        "gates":lowered.gates.iter().map(|gate| json!({"kind":format!("{:?}",gate.kind),"inputs":gate.inputs,"output":gate.output})).collect::<Vec<_>>(),
        "changes":timing.nets.iter().map(|(name,net)| (name.clone(),json!(net.changes()))).collect::<std::collections::BTreeMap<_,_>>()});
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    std::fs::write(
        target.join("generator_yosys_hazard_trace.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn trace_slowest_decoder_transition() {
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "fixture.sv",
            text: include_str!("fixtures/seven_segment.sv"),
        }],
        &CompileOptions::new("seven_segment"),
    )
    .unwrap();
    let outputs = artifact.output_map();
    let lowered = lower_optimised(&artifact.netlist).unwrap();
    let compiled = compile(&lowered).unwrap();
    let route_report = reda::compile::routing_stats::analyze(&lowered, &compiled).unwrap();
    let routes: Vec<_> = route_report
        .edges
        .iter()
        .map(|edge| {
            json!({
                "source":edge.source, "sink":edge.sink, "hops":edge.hops,
                "repeaters":edge.total().repeaters,
                "parts":edge.parts.iter().map(|(part, cost)|
                    (format!("{part:?}"), json!({"length":cost.length,"repeaters":cost.repeaters})))
                    .collect::<std::collections::BTreeMap<_,_>>()
            })
        })
        .collect();
    let mut repeater_delays = std::collections::BTreeMap::new();
    for index in compiled
        .world
        .positions_of(reda::redstone::world::block::BlockKind::Repeater)
    {
        *repeater_delays
            .entry(
                compiled
                    .world
                    .palette()
                    .get(compiled.world.cells()[index])
                    .unwrap()
                    .delay,
            )
            .or_insert(0usize) += 1;
    }
    let mut watched = reda::timing::watch_all_nets(&compiled);
    for (name, signal) in &outputs {
        let (x, y, z) = compiled.output_positions[signal];
        watched.push((
            reda::redstone::simulator::position::Position::new(x, y, z),
            format!("public_{name}"),
        ));
    }
    let positions: Vec<_> = ["d0", "d1", "d2", "d3"]
        .iter()
        .map(|name| compiled.input_positions[*name])
        .collect();
    let mut sim = Simulator::new(compiled.world);
    sim.run_until_stable(2000).unwrap();
    for &(x, y, z) in &positions {
        let mut block = sim.world().get(x, y, z).clone();
        block.lit = false;
        sim.world_mut().set(x, y, z, block);
    }
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(watched);
    let timing = reda::timing::measure_transition(&mut sim, 2000, |sim| {
        for (i, &(x, y, z)) in positions.iter().enumerate() {
            let mut block = sim.world().get(x, y, z).clone();
            block.lit = 7 & (1 << i) != 0;
            sim.world_mut().set(x, y, z, block);
        }
    })
    .unwrap();
    for (index, name) in ["a", "b", "c", "d", "e", "f", "g"].iter().enumerate() {
        let (x, y, z) = compiled.output_positions[&outputs[*name]];
        assert_eq!(
            sim.world().get(x, y, z).lit,
            reda::circuits::seven_segment::TRUTH_TABLE[7][index] == 1
        );
    }
    let report = json!({"from":0,"to":7,"output_map":outputs,"routes":routes,"repeater_delays":repeater_delays,
        "settle_ticks":timing.settle_game_ticks,
        "gates":lowered.gates.iter().map(|gate| json!({"kind":format!("{:?}",gate.kind),"inputs":gate.inputs,"output":gate.output,"position":compiled.gate_output_positions[&gate.output]})).collect::<Vec<_>>(),
        "changes":timing.nets.iter().map(|(name,net)| (name.clone(),json!(net.changes()))).collect::<std::collections::BTreeMap<_,_>>()});
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    std::fs::write(
        target.join("generator_slow_path_trace.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn trace_single_input_transition() {
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "fixture.sv",
            text: include_str!("fixtures/seven_segment.sv"),
        }],
        &CompileOptions::new("seven_segment"),
    )
    .unwrap();
    let outputs = artifact.output_map();
    let lowered = lower_optimised(&artifact.netlist).unwrap();
    let compiled = compile(&lowered).unwrap();
    let mut watched = reda::timing::watch_all_nets(&compiled);
    let positions: Vec<_> = ["d0", "d1", "d2", "d3"]
        .iter()
        .map(|name| compiled.input_positions[*name])
        .collect();
    let output = compiled.output_positions[&outputs["f"]];
    watched.push((
        reda::redstone::simulator::position::Position::new(output.0, output.1, output.2),
        "public_f".to_string(),
    ));
    let mut sim = Simulator::new(compiled.world);
    sim.run_until_stable(2000).unwrap();
    for (i, &(x, y, z)) in positions.iter().enumerate() {
        let mut block = sim.world().get(x, y, z).clone();
        block.lit = 14 & (1 << i) != 0;
        sim.world_mut().set(x, y, z, block);
    }
    sim.run_until_stable(2000).unwrap();
    assert!(!sim.world().get(output.0, output.1, output.2).lit);
    sim.attach_observer(watched);
    let start = sim.current_tick();
    let (x, y, z) = positions[2];
    let mut block = sim.world().get(x, y, z).clone();
    block.lit = false;
    sim.world_mut().set(x, y, z, block);
    let ticks = sim.run_until_stable(2000).unwrap();
    assert!(!sim.world().get(output.0, output.1, output.2).lit);
    let timing = reda::timing::observations_to_result(sim.observations(), start, ticks);
    assert!(
        !timing.nets.contains_key("public_f"),
        "f must remain false throughout a single-input blank-to-blank transition"
    );
    let report = json!({"output_map":outputs,
        "netlist_hash":artifact.debug.netlist_fingerprint.as_str(),
        "debug_hash":reda::compile::metrics::canonical_fingerprint(artifact.debug.to_json().as_bytes()).as_str(),
        "gates":lowered.gates.iter().map(|gate| json!({"kind":format!("{:?}",gate.kind),"inputs":gate.inputs,"output":gate.output})).collect::<Vec<_>>(),
        "changes":timing.nets.iter().map(|(name,net)| (name.clone(),json!(net.changes()))).collect::<std::collections::BTreeMap<_,_>>()});
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    std::fs::write(
        target.join("generator_hazard_trace.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
    println!("{report}");
}

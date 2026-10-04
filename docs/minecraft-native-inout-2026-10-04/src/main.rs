//! Fixture qualification exporter; no compiler changes and no game execution.
//! Usage (root must serialize under an actual memory cap): exporter DUMP TRACE.json
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use reda::compile::{Gate, Netlist};
use reda::compile::topology::GateKind;
use reda::frontend::{compile_systemverilog_physical, CompileOptions, SourceInput};
use reda::frontend::evaluate::Evaluator;
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use serde_json::{json, Value};

type Point = (i32, i32, i32);
fn point(p: [i32; 3]) -> Point { (p[0], p[1], p[2]) }
fn signal_at(positions: &BTreeMap<String, Point>, at: [i32; 3]) -> String {
    let matches: Vec<_> = positions.iter().filter(|(_, p)| **p == point(at)).collect();
    assert_eq!(matches.len(), 1, "coordinate must identify exactly one logical signal: {at:?}");
    matches[0].0.clone()
}
fn reference_gate(output: &str, inputs: Vec<String>, kind: GateKind) -> Gate {
    Gate { name: output.into(), output: output.into(), inputs, kind }
}
fn token(s: &str) {
    assert!(!s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c == ','), "invalid dump token {s:?}");
}
fn ieee_value(e0: bool, d0: bool, e1: bool, d1: bool, present: bool, value: bool) -> &'static str {
    let active: Vec<_> = [(e0, d0), (e1, d1), (present, value)].into_iter()
        .filter_map(|(en, value)| en.then_some(value)).collect();
    if active.is_empty() { "z" }
    else if active.iter().any(|v| *v != active[0]) { "x" }
    else if active[0] { "1" } else { "0" }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 { return Err("usage: exporter DUMP TRACE.json".into()); }
    let fixture = include_str!("../../../../tests/fixtures/resolved-inout.sv");
    let mut circuit = compile_systemverilog_physical(
        &[SourceInput { name: "tests/fixtures/resolved-inout.sv", text: fixture }],
        &CompileOptions::new("top"),
    )?;
    // This producer is deliberately fixture-qualified. Fail if the public
    // materialization changes instead of silently dropping any observation.
    assert_eq!(circuit.pinout.inouts.len(), 1);
    let bus = circuit.pinout.inouts[0].clone();
    let actual = circuit.physical.implementation_netlist(circuit.implementation_netlist()).clone();
    assert!(circuit.physical.physical_rewrite().is_none(), "fixture needs a fresh boundary proof if rewritten");
    assert_eq!(actual.gates.len(), 37);
    let actual_names: BTreeSet<_> = actual.gates.iter().map(|g| g.output.clone()).collect();
    assert_eq!(actual_names.len(), 37);
    assert_eq!(actual_names, circuit.physical.gate_output_positions.keys().cloned().collect());
    assert!(actual.gates.iter().all(|g| !g.kind.is_sequential()));
    assert_eq!(actual.inputs.iter().cloned().collect::<BTreeSet<_>>(),
        circuit.physical.input_positions.keys().cloned().collect());

    // Public coordinate joins, never private read/injection naming conventions.
    let bus_read = signal_at(&circuit.physical.input_positions, bus.readback);
    let pad_read = signal_at(&circuit.physical.input_positions, bus.external_pad);
    let present_signal = signal_at(&circuit.physical.input_positions, bus.external_present);
    let value_signal = signal_at(&circuit.physical.input_positions, bus.external_value);
    assert_ne!(bus_read, pad_read);
    let source_order = ["e0", "d0", "e1", "d1"]; // The public fixture port contract / Icarus bit order.
    assert_eq!(circuit.pinout.inputs.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        source_order.into_iter().collect());
    let source_signals: Vec<_> = source_order.iter().map(|name|
        signal_at(&circuit.physical.input_positions, circuit.pinout.inputs[*name])).collect();
    let ordinary_and_protocol: BTreeSet<_> = source_signals.iter().cloned()
        .chain([present_signal.clone(), value_signal.clone()]).collect();
    assert_eq!(ordinary_and_protocol.len(), 6);
    let expected_inputs: BTreeSet<_> = ordinary_and_protocol.iter().cloned()
        .chain([bus_read.clone(), pad_read.clone()]).collect();
    assert_eq!(expected_inputs, actual.inputs.iter().cloned().collect());
    for name in &ordinary_and_protocol {
        let p = circuit.physical.input_positions[name];
        assert_eq!(circuit.physical.world.get(p.0, p.1, p.2).kind, BlockKind::Lever);
    }
    for p in [bus.readback, bus.external_pad] {
        assert_eq!(circuit.physical.world.get(p[0], p[1], p[2]).kind, BlockKind::RedstoneWire);
    }

    let public_status = [
        ("conflict", bus.conflict), ("floating", bus.floating),
        ("expected_value", bus.expected_value), ("external_observed", bus.external_observed),
        ("external_expected", bus.external_expected),
    ];
    let mut output_bindings = Vec::<(String, String, [i32; 3])>::new();
    for (label, p) in &circuit.pinout.outputs {
        output_bindings.push((label.clone(), signal_at(&circuit.physical.output_positions, *p), *p));
    }
    assert_eq!(output_bindings.len(), 1);
    for (role, p) in public_status {
        output_bindings.push((format!("{}:{role}", bus.source.name),
            signal_at(&circuit.physical.output_positions, p), p));
    }
    assert!(output_bindings.iter().all(|(_, _, p)|
        circuit.physical.world.get(p[0], p[1], p[2]).kind == BlockKind::Lamp));
    let public_lamp_signals: BTreeSet<_> = output_bindings.iter().map(|(_, signal, _)| signal.clone()).collect();
    let mut injections = Vec::new();
    let mut physical_output_inventory = Vec::new();
    for (name, p) in &circuit.physical.output_positions {
        let kind = circuit.physical.world.get(p.0, p.1, p.2).kind;
        eprintln!("PHYSICAL_OUTPUT {name} {} {} {} {kind:?}", p.0, p.1, p.2);
        physical_output_inventory.push(json!({"signal":name,"position":[p.0,p.1,p.2],"kind":format!("{kind:?}")}));
        assert!(actual.outputs.contains(name), "physical output is not an implementation output: {name}");
        if public_lamp_signals.contains(name) { continue; }
        if kind == BlockKind::Lamp {
            output_bindings.push((format!("interface:{name}"), name.clone(), [p.0,p.1,p.2]));
        } else {
            assert_eq!(kind, BlockKind::RedstoneWire,
                "unclassified physical output: {name} at {p:?}");
            assert_ne!(*p, point(bus.external_pad));
            assert_ne!(*p, point(bus.readback));
            assert!(bus.channel.iter().any(|q| q[1] == p.1 && q[2] == p.2
                && q[0].abs_diff(p.0) == 2), "pad is detached from this public shared channel");
            injections.push(name.clone());
        }
    }
    assert_eq!(injections.len(), 2);
    assert!(injections.iter().all(|name| actual.outputs.contains(name)));
    let covered_outputs: BTreeSet<_> = output_bindings.iter().map(|(_, name, _)| name.clone())
        .chain(injections.iter().cloned()).collect();
    assert_eq!(covered_outputs, circuit.physical.output_positions.keys().cloned().collect());
    assert_eq!(covered_outputs, actual.outputs.iter().cloned().collect());

    let mut prefix = "__qualification_".to_owned();
    let known: BTreeSet<_> = actual_names.iter().cloned().chain(actual.inputs.iter().cloned()).collect();
    while known.iter().any(|name| name.starts_with(&prefix)) { prefix.push('_'); }
    let caller = format!("{prefix}caller");
    let mut reference: Netlist = actual.clone();
    reference.inputs = ordinary_and_protocol.iter().cloned().chain([caller.clone()]).collect();
    let mut bus_drivers = injections.clone();
    bus_drivers.push(caller.clone());
    reference.gates.push(reference_gate(&bus_read, bus_drivers, GateKind::Or(3)));
    reference.gates.push(reference_gate(&pad_read, vec![caller.clone()], GateKind::Buf));
    let mut outputs = Vec::new();
    for (ordinal, (label, signal, p)) in output_bindings.iter().enumerate() {
        let alias = format!("{prefix}output_{ordinal}");
        reference.gates.push(reference_gate(&alias, vec![signal.clone()], GateKind::Buf));
        outputs.push((alias, label.clone(), signal.clone(), *p));
    }
    reference.outputs = outputs.iter().map(|(alias, _, _, _)| alias.clone()).collect();
    let order = reference.combinational_order().ok_or("reference closure contains a cycle")?;
    let evaluator = Evaluator::new(&reference)?;
    assert_eq!(reference.inputs.len(), 7);
    let virtual_names: BTreeSet<_> = reference.gates.iter().map(|g| g.output.clone())
        .filter(|n| !actual_names.contains(n)).collect();
    assert_eq!(virtual_names.len(), 2 + output_bindings.len()); // Two read closures plus every lamp alias.
    assert!(virtual_names.is_disjoint(&actual_names));

    // Install only the independent caller fixture. Ordinary/protocol hardware
    // and every compiled gate remain exactly the public API's emitted world.
    let p = bus.external_entry;
    assert!(p[1] > 0);
    assert_eq!(circuit.physical.world.get(p[0], p[1], p[2]).kind, BlockKind::Air);
    assert_eq!(circuit.physical.world.get(p[0], p[1]-1, p[2]).kind, BlockKind::Air);
    let mut floor = BlockState::air();
    floor.kind = BlockKind::Solid;
    floor.name = "minecraft:stone".into();
    circuit.physical.world.set(p[0], p[1]-1, p[2], floor);
    let mut lever = BlockState::air();
    lever.kind = BlockKind::Lever;
    lever.name = "minecraft:lever".into();
    lever.face = Some(Face::Floor);
    lever.facing = Some(Facing::North);
    circuit.physical.world.set(p[0], p[1], p[2], lever);

    let controls: Vec<_> = source_signals.iter().cloned()
        .chain([present_signal.clone(), value_signal.clone(), caller.clone()]).collect();
    let mut rows: Vec<Value> = Vec::with_capacity(128);
    let mut classifications = BTreeMap::<String, usize>::new();
    let mut injection_truth = vec![Vec::new(), Vec::new()];
    let mut writer_truth = vec![Vec::new(), Vec::new()];
    let status_signals: BTreeMap<_, _> = public_status.iter().map(|(role, p)|
        (*role, signal_at(&circuit.physical.output_positions, *p))).collect();
    for raw in 0u8..128 {
        let drive: BTreeMap<_, _> = controls.iter().enumerate()
            .map(|(bit, name)| (name.clone(), raw & (1 << bit) != 0)).collect();
        let values = evaluator.settle(&drive)?;
        let [e0, d0, e1, d1, present, declared, actual_caller] =
            std::array::from_fn::<_, 7, _>(|bit| raw & (1 << bit) != 0);
        let internal = [e0 && d0, e1 && d1];
        for i in 0..2 { injection_truth[i].push(values[&injections[i]]); writer_truth[i].push(internal[i]); }
        let electrical_bus = internal.into_iter().any(|v| v) || actual_caller;
        let ieee = ieee_value(e0, d0, e1, d1, present, declared);
        let matched = actual_caller == (present && declared);
        assert_eq!(values[&bus_read], electrical_bus);
        assert_eq!(values[&pad_read], actual_caller);
        assert_eq!(values[&status_signals["conflict"]], ieee == "x");
        assert_eq!(values[&status_signals["floating"]], ieee == "z");
        assert_eq!(values[&status_signals["expected_value"]], internal.into_iter().any(|v| v) || present && declared);
        assert_eq!(values[&status_signals["external_observed"]], actual_caller);
        assert_eq!(values[&status_signals["external_expected"]], present && declared);
        assert_eq!(values[&output_bindings[0].1], electrical_bus);
        let (kind, boolean_source_assertion) = if !matched { ("ProtocolViolation", None) }
            else if ieee == "x" { ("Contention", None) }
            else if ieee == "z" { ("Floating", None) }
            else { ("Value", Some(ieee == "1")) };
        *classifications.entry(kind.into()).or_default() += 1;
        let observed_expected: BTreeMap<_, _> = actual_names.iter()
            .chain(reference.outputs.iter()).map(|name| (name.clone(), values[name])).collect();
        rows.push(json!({
            "raw_control_state": raw, "icarus_declared_state": raw & 63,
            "controls": drive, "protocol_matched": matched, "source_ieee_expected": ieee,
            "checked_kind": kind, "boolean_source_assertion": boolean_source_assertion,
            "electrical_observation_expected": observed_expected,
            "electrical_read_expected": {"bus_readback_and_all_channel_cells": electrical_bus,
                "external_pad": actual_caller},
        }));
    }
    assert_eq!(classifications, BTreeMap::from([
        ("Value".into(), 38), ("Contention".into(), 18),
        ("Floating".into(), 8), ("ProtocolViolation".into(), 64),
    ]));
    let injection_roles: Vec<_> = injection_truth.iter().map(|truth| {
        let matches: Vec<_> = writer_truth.iter().enumerate().filter(|(_, expected)| *expected == truth).map(|(i, _)| i).collect();
        assert_eq!(matches.len(), 1, "actual injection must equal exactly one source writer for all128 controls");
        matches[0]
    }).collect();
    assert_eq!(injection_roles.iter().copied().collect::<BTreeSet<_>>(), BTreeSet::from([0, 1]));
    assert_eq!(rows.iter().filter(|r| r["protocol_matched"].as_bool() == Some(true)).count(), 64);

    let mut dump = BufWriter::new(File::create(&args[0])?);
    let world = &circuit.physical.world;
    let (sx, sy, sz) = world.size();
    writeln!(dump, "SIZE {sx} {sy} {sz}")?;
    for (flat, &index) in world.cells().iter().enumerate() {
        let state = world.palette().get(index).ok_or("world palette index is missing")?;
        if state.kind == BlockKind::Air { continue; }
        let (x, y, z) = world.decode(flat);
        let facing = state.facing.map(|f| format!("{f:?}").to_ascii_lowercase()).unwrap_or_else(|| "-".into());
        let face = state.face.map(|f| format!("{f:?}").to_ascii_lowercase()).unwrap_or_else(|| "-".into());
        writeln!(dump, "BLOCK {x} {y} {z} {:?} {facing} {face} {} {} {}", state.kind, state.lit, state.delay, state.name)?;
        for (key, value) in &state.extra_properties {
            token(key); token(value);
            writeln!(dump, "BLOCKPROP {x} {y} {z} {key} {value}")?;
        }
    }
    for name in &ordinary_and_protocol {
        token(name);
        let (x, y, z) = circuit.physical.input_positions[name];
        writeln!(dump, "INPUT {name} {x} {y} {z}")?;
    }
    writeln!(dump, "INPUT {caller} {} {} {}", p[0], p[1], p[2])?;
    for (alias, label, _, p) in &outputs {
        token(alias); token(label);
        writeln!(dump, "OUTPUT {alias} {} {} {}", p[0], p[1], p[2])?;
        writeln!(dump, "OUTLABEL {label} {alias}")?;
    }
    for gate in &actual.gates {
        token(&gate.output);
        let (x, y, z) = circuit.physical.gate_output_positions[&gate.output];
        let kind = world.get(x, y, z).kind;
        assert!(matches!(kind, BlockKind::Torch | BlockKind::WallTorch | BlockKind::RedstoneWire | BlockKind::Repeater | BlockKind::Comparator));
        writeln!(dump, "GATEOUT {} {x} {y} {z} {kind:?}", gate.output)?;
    }
    for index in order {
        let gate = &reference.gates[index];
        token(&gate.output);
        for input in &gate.inputs { token(input); }
        let inputs = if gate.inputs.is_empty() { "-".into() } else { gate.inputs.join(",") };
        let kind = gate.kind.wire_name();
        assert!(matches!(kind, "nor" | "merge" | "buf" | "andnot"));
        writeln!(dump, "GATE {} {inputs} {kind}", gate.output)?;
    }
    dump.flush()?;
    let virtual_definitions: Vec<_> = reference.gates.iter().filter(|g| virtual_names.contains(&g.output))
        .map(|g| json!({"output":g.output,"inputs":g.inputs,"kind":g.kind.wire_name()})).collect();
    let trace = json!({
        "scope":"fixture-qualified expected electrical graph; not a compiler algorithm or IEEE Boolean substitution",
        "public_api":"frontend::compile_systemverilog_physical",
        "fixture":"tests/fixtures/resolved-inout.sv", "top":"top",
        "pinout": circuit.pinout, "actual_gate_count":37, "actual_gate_names":actual_names,
        "physical_output_inventory":physical_output_inventory,
        "virtual_reference_definitions":virtual_definitions, "virtual_reference_observations":[],
        "control_order":controls, "source_control_order":source_order,
        "source_control_bindings": source_order.iter().zip(&source_signals).map(|(public,signal)|json!({"source":public,"signal":signal})).collect::<Vec<_>>(),
        "typed_reads":{"bus_read":bus_read,"external_pad_read":pad_read},
        "internal_injections":injections.iter().zip(injection_roles).map(|(signal, writer)|json!({"signal":signal,"source_writer_ordinal":writer,"position":circuit.physical.output_positions[signal]})).collect::<Vec<_>>(),
        "caller_fixture":{"input":caller,"entry":p,"floor":[p[0],p[1]-1,p[2]],"allowed_world_changes":2},
        "output_aliases":outputs.iter().map(|(alias,label,signal,pos)|json!({"alias":alias,"label":label,"actual_signal":signal,"position":pos})).collect::<Vec<_>>(),
        "raw_control_count":128,"matched_protocol_count":64,"mismatched_protocol_count":64,
        "checked_expected_counts":classifications,"vectors":rows,
        "qualification_requirement":"Root must independently compare matched64 declared states to Icarus and all actual electrical/channel/pad observations to checked outcomes. Generic harness raw Boolean success never certifies X/Z or ProtocolViolation.",
    });
    let mut manifest = BufWriter::new(File::create(&args[1])?);
    serde_json::to_writer_pretty(&mut manifest, &trace)?;
    writeln!(manifest)?;
    manifest.flush()?;
    Ok(())
}
fn main() {
    if let Err(error) = run() { eprintln!("{error}"); std::process::exit(1); }
}

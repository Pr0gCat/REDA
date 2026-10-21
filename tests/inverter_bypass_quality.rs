use reda::compile::{
    compile,
    lowering::{lower, lower_optimised, lower_optimised_with_provenance},
    Gate, Netlist,
};
use reda::redstone::{
    simulator::{position::Position, Simulator},
    world::block::BlockKind,
};
use serde_json::{json, Value};
use std::collections::HashMap;

fn measure(source: &Netlist, expected_inverted: &[bool]) -> Value {
    measure_function(source, |vector| {
        expected_inverted
            .iter()
            .map(|inverted| (vector != 0) ^ inverted)
            .collect()
    })
}

fn measure_function(source: &Netlist, expected_function: impl Fn(usize) -> Vec<bool>) -> Value {
    let circuit = compile(source).unwrap();
    let repeated = compile(source).unwrap();
    let fingerprint =
        reda::compile::metrics::canonical_world_fingerprint(&circuit.world);
    assert_eq!(
        fingerprint,
        reda::compile::metrics::canonical_world_fingerprint(&repeated.world)
    );
    let inputs: Vec<_> = source
        .inputs
        .iter()
        .map(|n| circuit.input_positions[n])
        .collect();
    assert_eq!(source.outputs.len(), expected_function(0).len());
    let drivers: Vec<_> = source
        .outputs
        .iter()
        .map(|o| circuit.gate_output_positions[o])
        .collect();
    let lamps: Vec<_> = source
        .outputs
        .iter()
        .map(|o| circuit.output_positions[o])
        .collect();
    let mut sim = Simulator::new(circuit.bench);
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(
        drivers
            .iter()
            .enumerate()
            .map(|(i, p)| (Position::new(p.0, p.1, p.2), format!("driver{i}")))
            .chain(
                lamps
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (Position::new(p.0, p.1, p.2), format!("lamp{i}"))),
            ),
    );
    let mut worst = 0;
    let mut total = 0;
    let mut driver_last = 0;
    let mut glitches = [0usize; 2];
    let mut single = [0usize; 2];
    let mut pulse_worst = 0;
    let mut pulse_total = 0;
    let mut traces = Vec::new();
    for from in 0..(1usize << inputs.len()) {
        for to in 0..(1usize << inputs.len()) {
            for (phase, vector) in [from, to].into_iter().enumerate() {
                for (bit, input) in inputs.iter().enumerate() {
                    let mut lever = sim.world().get(input.0, input.1, input.2).clone();
                    lever.lit = vector & (1 << bit) != 0;
                    sim.world_mut().set(input.0, input.1, input.2, lever);
                }
                sim.reset_observer();
                let start = sim.current_tick();
                let ticks = sim.run_until_stable(2000).unwrap();
                let mut values: HashMap<String, bool> = source
                    .inputs
                    .iter()
                    .enumerate()
                    .map(|(bit, n)| (n.clone(), vector & (1 << bit) != 0))
                    .collect();
                for index in source.combinational_order().unwrap() {
                    let gate = &source.gates[index];
                    let operands: Vec<_> = gate.inputs.iter().map(|n| values[n]).collect();
                    let expected = gate.kind.evaluate(&operands);
                    values.insert(gate.output.clone(), expected);
                    let (x, y, z) = circuit.gate_output_positions[&gate.output];
                    let b = sim.world().get(x, y, z);
                    assert_eq!(
                        if b.kind == BlockKind::RedstoneWire {
                            b.power > 0
                        } else {
                            b.lit
                        },
                        expected,
                        "internal driver {}",
                        gate.output
                    );
                }
                for (port, expected) in expected_function(vector).iter().enumerate() {
                    for (x, y, z) in [drivers[port], lamps[port]] {
                        assert_eq!(sim.world().get(x, y, z).lit, *expected);
                    }
                }
                if phase == 1 {
                    worst = worst.max(ticks);
                    total += ticks;
                    for (port, expected) in expected_function(vector).iter().enumerate() {
                        let expected = *expected;
                        for (i, prefix) in ["driver", "lamp"].into_iter().enumerate() {
                            let label = format!("{prefix}{port}");
                            let events: Vec<_> = sim
                                .observations()
                                .iter()
                                .filter(|e| e.label == label)
                                .collect();
                            if events.len() > 1 {
                                glitches[i] += 1;
                                if (from ^ to).count_ones() == 1 {
                                    single[i] += 1;
                                }
                            }
                            if i == 0 {
                                driver_last = driver_last
                                    .max(events.last().map(|e| e.tick - start).unwrap_or(0));
                                for pair in events.windows(2) {
                                    if pair[0].value != expected {
                                        let width = pair[1].tick - pair[0].tick;
                                        pulse_worst = pulse_worst.max(width);
                                        pulse_total += width;
                                    }
                                }
                            }
                        }
                    }
                    traces.push(json!({"from":from,"to":to,"ticks":ticks,"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
                }
            }
        }
    }
    json!({"fingerprint":fingerprint,"traces":traces,"quality":{"worst_settle_ticks":worst,"total_settle_ticks":total,"driver_glitches":glitches[0],"lamp_glitches":glitches[1],"driver_single_input_glitches":single[0],"lamp_single_input_glitches":single[1],"worst_driver_change_ticks":driver_last,"worst_driver_pulse_ticks":pulse_worst,"total_driver_pulse_ticks":pulse_total}})
}

#[test]
fn guarded_inverter_bypass_preserves_real_boundaries_and_improves_physical_chains() {
    let mut report = Vec::new();
    for length in 2..=10 {
        let source = Netlist {
            inputs: vec!["a".into()],
            outputs: vec![format!("g{}", length - 1)],
            gates: (0..length)
                .map(|i| {
                    let input = if i == 0 {
                        "a".to_owned()
                    } else {
                        format!("g{}", i - 1)
                    };
                    Gate::nor(&format!("g{i}"), &[input.as_str()])
                })
                .collect(),
        };
        // Positive lowering is the independent reference, not the new optimizer.
        let control = lower(&source).unwrap();
        assert_eq!(control, source);
        let candidate = lower_optimised(&source).unwrap();
        assert_eq!(candidate.outputs, source.outputs);
        let provenance = lower_optimised_with_provenance(&source).unwrap();
        assert_eq!(candidate, provenance.netlist);
        assert_eq!(provenance.provenance.len(), candidate.gates.len());
        assert_eq!(provenance.source_terminals.len(), source.gates.len());
        for terminals in &provenance.source_terminals {
            assert!(terminals.iter().all(|i| *i < candidate.gates.len()));
        }
        let reference = measure(&control, &[length % 2 == 1]);
        let optimized = measure(&candidate, &[length % 2 == 1]);
        report.push(json!({"length":length,"control":reference,"candidate":optimized}));
    }
    std::fs::write(
        "target/inverter_bypass_quality.json",
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn bypass_keeps_exported_inverters_and_rejects_multi_input_support_fanout() {
    let exported = Netlist {
        inputs: vec!["a".into()],
        outputs: vec!["y".into(), "middle".into(), "y".into()],
        gates: vec![
            Gate::nor("first", &["a"]),
            Gate::nor("middle", &["first"]),
            Gate::nor("last", &["middle"]),
            Gate::nor("y", &["last"]),
        ],
    };
    let unchanged = lower_optimised(&exported).unwrap();
    assert_eq!(unchanged.outputs, exported.outputs);
    for name in ["middle", "y"] {
        assert!(unchanged.gates.iter().any(|g| g.output == name));
    }
    for input in [false, true] {
        let mut values = HashMap::from([("a".to_owned(), input)]);
        for index in unchanged.combinational_order().unwrap() {
            let gate = &unchanged.gates[index];
            let operands: Vec<_> = gate.inputs.iter().map(|n| values[n]).collect();
            values.insert(gate.output.clone(), gate.kind.evaluate(&operands));
        }
        for name in &unchanged.outputs {
            assert_eq!(values[name], input);
        }
    }
    for receiver in ["a", "middle"] {
        let source = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["y".into(), "other".into()],
            gates: vec![
                Gate::nor("first", &["a"]),
                Gate::nor("middle", &["first"]),
                Gate::nor("y", &["middle"]),
                Gate::nor("other", &[receiver, "b"]),
            ],
        };
        assert_eq!(lower_optimised(&source).unwrap(), source);
    }
}

#[test]
fn inverter_only_fanout_preserves_all_real_output_lamps_and_quality() {
    let mut report = Vec::new();
    for left in 1..=4 {
        for right in 1..=4 {
            let mut gates = vec![
                Gate::nor("p0", &["a"]),
                Gate::nor("p1", &["p0"]),
                Gate::nor("p2", &["p1"]),
                Gate::nor("p3", &["p2"]),
            ];
            let mut outputs = Vec::new();
            for (prefix, length) in [("left", left), ("right", right)] {
                let mut input = "p3".to_owned();
                for i in 0..length {
                    let output = format!("{prefix}{i}");
                    gates.push(Gate::nor(&output, &[input.as_str()]));
                    input = output;
                }
                outputs.push(input);
            }
            let source = Netlist {
                inputs: vec!["a".into()],
                outputs,
                gates,
            };
            let control = lower(&source).unwrap();
            assert_eq!(control, source);
            let candidate = lower_optimised(&source).unwrap();
            assert_eq!(candidate.outputs, source.outputs);
            let expected = [left % 2 == 1, right % 2 == 1];
            let reference = measure(&control, &expected);
            let optimized = measure(&candidate, &expected);
            report
                .push(json!({"left":left,"right":right,"control":reference,"candidate":optimized}));
        }
    }
    std::fs::write(
        "target/inverter_bypass_fanout_quality.json",
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn multi_input_source_before_inverter_chain_preserves_physical_quality() {
    let mut report = Vec::new();
    for arity in [2, 3] {
        for length in 3..=8 {
            let inputs: Vec<String> = (0..arity).map(|i| format!("in{i}")).collect();
            let names: Vec<_> = inputs.iter().map(String::as_str).collect();
            let mut gates = vec![Gate::nor("seed", &names)];
            let mut previous = "seed".to_owned();
            for i in 0..length {
                let output = format!("stage{i}");
                gates.push(Gate::nor(&output, &[previous.as_str()]));
                previous = output;
            }
            let source = Netlist {
                inputs,
                outputs: vec![previous],
                gates,
            };
            let control = lower(&source).unwrap();
            assert_eq!(control, source);
            let candidate = lower_optimised(&source).unwrap();
            let expected = |vector| vec![(vector == 0) ^ (length % 2 == 1)];
            let reference = measure_function(&control, expected);
            let optimized = measure_function(&candidate, expected);
            eprintln!(
                "arity={arity},length={length},control={},candidate={}",
                reference["quality"], optimized["quality"]
            );
            report.push(
                json!({"arity":arity,"length":length,"control":reference,"candidate":optimized}),
            );
        }
    }
    std::fs::write(
        "target/inverter_bypass_parent_quality.json",
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn inverter_chain_before_multi_input_receiver_preserves_physical_quality() {
    let mut report = Vec::new();
    for arity in [2, 3] {
        for length in 3..=8 {
            let inputs: Vec<String> = (0..arity).map(|i| format!("in{i}")).collect();
            let mut gates = Vec::new();
            let mut previous = inputs[0].clone();
            for i in 0..length {
                let output = format!("stage{i}");
                gates.push(Gate::nor(&output, &[previous.as_str()]));
                previous = output;
            }
            let mut operands = vec![previous.as_str()];
            operands.extend(inputs[1..].iter().map(String::as_str));
            gates.push(Gate::nor("y", &operands));
            let source = Netlist {
                inputs,
                outputs: vec!["y".into()],
                gates,
            };
            let control = lower(&source).unwrap();
            assert_eq!(control, source);
            let candidate = lower_optimised(&source).unwrap();
            let expected = |vector: usize| {
                vec![
                    !((vector & 1 != 0) ^ (length % 2 == 1))
                        && vector & ((1usize << arity) - 2) == 0,
                ]
            };
            let reference = measure_function(&control, expected);
            let optimized = measure_function(&candidate, expected);
            assert_eq!(
                candidate, control,
                "downstream multi-input receiver must retain its timing path"
            );
            eprintln!(
                "receiver arity={arity},length={length},control={},candidate={}",
                reference["quality"], optimized["quality"]
            );
            report.push(
                json!({"arity":arity,"length":length,"control":reference,"candidate":optimized}),
            );
        }
    }
    std::fs::write(
        "target/inverter_bypass_receiver_quality.json",
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

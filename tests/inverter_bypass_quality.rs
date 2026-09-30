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
    let circuit = compile(source).unwrap();
    let repeated = compile(source).unwrap();
    let fingerprint =
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&circuit.world);
    assert_eq!(
        fingerprint,
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&repeated.world)
    );
    let input = circuit.input_positions["a"];
    assert_eq!(source.outputs.len(), expected_inverted.len());
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
    let mut sim = Simulator::new(circuit.world);
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
    for from in 0..2usize {
        for to in 0..2usize {
            for (phase, vector) in [from, to].into_iter().enumerate() {
                let mut lever = sim.world().get(input.0, input.1, input.2).clone();
                lever.lit = vector != 0;
                sim.world_mut().set(input.0, input.1, input.2, lever);
                sim.reset_observer();
                let start = sim.current_tick();
                let ticks = sim.run_until_stable(2000).unwrap();
                let mut values = HashMap::from([("a".to_owned(), vector != 0)]);
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
                for (port, inverted) in expected_inverted.iter().enumerate() {
                    for (x, y, z) in [drivers[port], lamps[port]] {
                        assert_eq!(sim.world().get(x, y, z).lit, (vector != 0) ^ inverted);
                    }
                }
                if phase == 1 {
                    worst = worst.max(ticks);
                    total += ticks;
                    for (port, inverted) in expected_inverted.iter().enumerate() {
                        let expected = (vector != 0) ^ inverted;
                        for (i, prefix) in ["driver", "lamp"].into_iter().enumerate() {
                            let label = format!("{prefix}{port}");
                            let events: Vec<_> = sim
                                .observations()
                                .iter()
                                .filter(|e| e.label == label)
                                .collect();
                            if events.len() > 1 {
                                glitches[i] += 1;
                                if from != to {
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

fn check_frozen_caps(reference: &Value, candidate: &Value, fixture: &Value) {
    for (metric, cap) in fixture["control"]["quality"].as_object().unwrap() {
        for value in [&reference["quality"][metric], &candidate["quality"][metric]] {
            assert!(
                value.as_u64().unwrap() <= cap.as_u64().unwrap(),
                "independent frozen metric regression: {metric}"
            );
        }
    }
}

#[test]
fn guarded_inverter_bypass_preserves_real_boundaries_and_improves_physical_chains() {
    let frozen: Value = serde_json::from_str(include_str!(
        "../docs/generator-inverter-bypass-2026-09-30.json"
    ))
    .unwrap();
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
        let fixture = frozen["chains"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["length"] == length)
            .unwrap();
        check_frozen_caps(&reference, &optimized, fixture);
        for (metric, value) in optimized["quality"].as_object().unwrap() {
            assert!(
                value.as_u64().unwrap() <= reference["quality"][metric].as_u64().unwrap(),
                "length={length},metric={metric}"
            );
        }
        if length >= 3 {
            assert!(
                optimized["quality"]["worst_driver_change_ticks"]
                    .as_u64()
                    .unwrap()
                    < reference["quality"]["worst_driver_change_ticks"]
                        .as_u64()
                        .unwrap()
            );
        }
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
    let frozen: Value = serde_json::from_str(include_str!(
        "../docs/generator-inverter-bypass-2026-09-30.json"
    ))
    .unwrap();
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
            let fixture = frozen["fanouts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["left"] == left && c["right"] == right)
                .unwrap();
            check_frozen_caps(&reference, &optimized, fixture);
            for (metric, value) in optimized["quality"].as_object().unwrap() {
                assert!(
                    value.as_u64().unwrap() <= reference["quality"][metric].as_u64().unwrap(),
                    "left={left},right={right},metric={metric}"
                );
            }
            assert!(
                optimized["quality"]["worst_driver_change_ticks"]
                    .as_u64()
                    .unwrap()
                    < reference["quality"]["worst_driver_change_ticks"]
                        .as_u64()
                        .unwrap()
            );
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

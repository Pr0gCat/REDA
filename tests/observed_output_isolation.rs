//! Public output fanout must not read power coming backward from a gate support.
use reda::compile::{
    compile,
    lowering::lower_optimised,
    planner::{plan_from_netlist, PortPlacements},
    routing::RouteTerminalKind,
};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};
use std::collections::HashMap;

#[test]
fn output_lamp_fanout_has_one_way_gate_landings_and_correct_history() {
    for words in [[1u8, 3, 2, 0], [5u8, 3, 2, 0]] {
        for (a, b, reverse) in [
            ("a", "b", false),
            ("left", "right", false),
            ("a", "b", true),
        ] {
            let inputs = if reverse {
                format!("{b},{a}")
            } else {
                format!("{a},{b}")
            };
            let mut text = format!("module probe(input logic {inputs},output logic y0,y1,y2);");
            for bit in 0..3 {
                let bits = words.map(|word| (word >> bit) & 1);
                text.push_str(&format!(
                    "assign y{bit}={b} ? ({a} ? 1'b{} : 1'b{}) : ({a} ? 1'b{} : 1'b{});",
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
            source.outputs = (0..3)
                .map(|bit| ports[&format!("y{bit}")].clone())
                .collect();
            let circuit = compile(&source).unwrap();
            let repeated = compile(&source).unwrap();
            assert_eq!(
                reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
                    &circuit.world
                ),
                reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
                    &repeated.world
                )
            );
            let plan = plan_from_netlist(&source, &PortPlacements::default()).unwrap();
            let mut checked = 0;
            for route in plan.routes() {
                if route
                    .owner()
                    .is_some_and(|n| source.outputs.iter().any(|o| o == n))
                {
                    for terminal in route.terminals() {
                        assert_ne!(terminal.kind, RouteTerminalKind::DirectedDustIntoSupport);
                        checked += 1;
                    }
                }
            }
            assert!(checked > 0, "the fixture must exercise output fanout");
            let order = source.combinational_order().unwrap();
            let mut sim = Simulator::new(circuit.world.clone());
            sim.run_until_stable(2000).unwrap();
            for from in 0..4usize {
                for to in 0..4usize {
                    for address in [from, to] {
                        let mut values: HashMap<String, bool> = source
                            .inputs
                            .iter()
                            .enumerate()
                            .map(|(bit, n)| (n.clone(), address & (1 << bit) != 0))
                            .collect();
                        let actual_address = usize::from(values[a]) + 2 * usize::from(values[b]);
                        let expected = words[actual_address];
                        for n in &source.inputs {
                            let (x, y, z) = circuit.input_positions[n];
                            let mut s = sim.world().get(x, y, z).clone();
                            s.lit = values[n];
                            sim.world_mut().set(x, y, z, s);
                        }
                        sim.run_until_stable(2000).unwrap();
                        for &index in &order {
                            let gate = &source.gates[index];
                            let operands: Vec<_> = gate.inputs.iter().map(|n| values[n]).collect();
                            let expected = gate.kind.evaluate(&operands);
                            values.insert(gate.output.clone(), expected);
                            let (x, y, z) = circuit.gate_output_positions[&gate.output];
                            let state = sim.world().get(x, y, z);
                            assert_eq!(
                                if state.kind == BlockKind::RedstoneWire {
                                    state.power > 0
                                } else {
                                    state.lit
                                },
                                expected,
                                "gate={},from={from},to={to},address={address}",
                                gate.output
                            );
                        }
                        for (bit, name) in source.outputs.iter().enumerate() {
                            let (x, y, z) = circuit.output_positions[name];
                            assert_eq!(
                                sim.world().get(x, y, z).lit,
                                expected & (1 << bit) != 0,
                                "lamp bit={bit},from={from},to={to},address={address}"
                            );
                        }
                    }
                }
            }
        }
    }
}

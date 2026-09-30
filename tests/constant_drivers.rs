use reda::compile::{compile, lowering::lower_optimised, Gate, Netlist};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};

#[test]
fn disconnected_torch_and_its_inverter_are_certified_constant_drivers() {
    for inputs in [vec![], vec!["a".into(), "b".into()]] {
        let source = Netlist {
            inputs,
            outputs: vec!["one".into(), "zero".into()],
            gates: vec![Gate::nor("one", &[]), Gate::nor("zero", &["one"])],
        };
        let lowered = lower_optimised(&source).unwrap();
        let circuit = compile(&lowered).unwrap();
        let repeat = compile(&lowered).unwrap();
        assert_eq!(
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&circuit.world),
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&repeat.world)
        );
        let drivers = circuit.gate_output_positions.clone();
        let lamps = circuit.output_positions.clone();
        let inputs: Vec<_> = circuit.input_positions.values().copied().collect();
        let mut sim = Simulator::new(circuit.world);
        sim.run_until_stable(2000).unwrap();
        for vector in 0..(1usize << inputs.len()) {
            for (bit, &(x, y, z)) in inputs.iter().enumerate() {
                let mut lever = sim.world().get(x, y, z).clone();
                lever.lit = vector & (1 << bit) != 0;
                sim.world_mut().set(x, y, z, lever);
            }
            sim.run_until_stable(2000).unwrap();
            for (name, expected) in [("one", true), ("zero", false)] {
                for &(x, y, z) in &[drivers[name], lamps[name]] {
                    let b = sim.world().get(x, y, z);
                    assert_eq!(
                        if b.kind == BlockKind::RedstoneWire {
                            b.power > 0
                        } else {
                            b.lit
                        },
                        expected
                    );
                }
            }
        }
    }
}

#[test]
fn native_constant_outputs_have_real_drivers_and_keep_constant_debug_bindings() {
    use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
    for source in [
        "module c(output logic one,zero); assign one=1'b1; assign zero=1'b0; endmodule",
        "module c(input logic a,b, output logic one,zero,y); assign one=a|1'b1; assign zero=a&1'b0; assign y=a^b; endmodule",
    ] {
        let artifact = compile_systemverilog(&[SourceInput { name:"constants.sv",text:source }],&CompileOptions::new("c")).unwrap();
        assert!(artifact.debug.realisations.iter().any(|r|matches!(r,reda::frontend::debug::Realisation::Const(true))));
        assert!(artifact.debug.realisations.iter().any(|r|matches!(r,reda::frontend::debug::Realisation::Const(false))));
        assert_eq!(artifact.debug.gates.len(),artifact.netlist.gates.len());
        for (reference,gate) in artifact.debug.gates.iter().zip(&artifact.netlist.gates) {
            assert_eq!(reference.output,gate.output);
        }
        let output_map = artifact.output_map();
        let lowered = lower_optimised(&artifact.netlist).unwrap();
        let circuit = compile(&lowered).unwrap();
        let outputs = circuit.output_positions.clone();
        let drivers = circuit.gate_output_positions.clone();
        let inputs: Vec<_> = circuit.input_positions.values().copied().collect();
        let mut sim = Simulator::new(circuit.world);
        sim.run_until_stable(2000).unwrap();
        for vector in 0..(1usize << inputs.len()) {
            for (bit,&(x,y,z)) in inputs.iter().enumerate() {
                let mut lever=sim.world().get(x,y,z).clone();
                lever.lit=vector&(1<<bit)!=0;
                sim.world_mut().set(x,y,z,lever);
            }
            sim.run_until_stable(2000).unwrap();
            for (name,expected) in [("one",true),("zero",false)] {
                let signal=&output_map[name];
                for &(x,y,z) in &[outputs[signal],drivers[signal]] {
                    let b=sim.world().get(x,y,z);
                    assert_eq!(if b.kind==BlockKind::RedstoneWire {b.power>0}else{b.lit},expected);
                }
            }
        }
        assert!(artifact.netlist.gates.iter().any(|g|g.kind==reda::compile::topology::GateKind::Nor(0)));
    }
}

#[test]
fn native_constant_register_data_survives_clock_edges() {
    use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
    for value in [false, true] {
        let source=format!("module c(input logic clk,output logic q); always_ff @(posedge clk) q <= 1'b{}; endmodule",u8::from(value));
        let artifact = compile_systemverilog(
            &[SourceInput {
                name: "constant_dff.sv",
                text: &source,
            }],
            &CompileOptions::new("c"),
        )
        .unwrap();
        let output = artifact.output_map()["q"].clone();
        let circuit = compile(&lower_optimised(&artifact.netlist).unwrap()).unwrap();
        let (x, y, z) = circuit.input_positions["clk"];
        let driver = circuit.gate_output_positions[&output];
        let lamp = circuit.output_positions[&output];
        let mut sim = Simulator::new(circuit.world);
        sim.run_until_stable(2000).unwrap();
        for clock in [true, false, true, false] {
            let mut lever = sim.world().get(x, y, z).clone();
            lever.lit = clock;
            sim.world_mut().set(x, y, z, lever);
            sim.run_until_stable(2000).unwrap();
            for (x, y, z) in [driver, lamp] {
                let b = sim.world().get(x, y, z);
                assert_eq!(
                    if b.kind == BlockKind::RedstoneWire {
                        b.power > 0
                    } else {
                        b.lit
                    },
                    value
                );
            }
        }
    }
}

#[test]
fn fresh_yosys_constant_output_ports_have_real_certified_drivers() {
    let source="module c(input a,b,output one,zero,y); assign one=1'b1; assign zero=1'b0; assign y=a^b; endmodule";
    let (netlist, ports) = reda::frontend::synthesize_verilog(source, "c").unwrap();
    let circuit = compile(&lower_optimised(&netlist).unwrap()).unwrap();
    let lamps = circuit.output_positions.clone();
    let drivers = circuit.gate_output_positions.clone();
    let inputs: Vec<_> = circuit.input_positions.values().copied().collect();
    let mut sim = Simulator::new(circuit.world);
    sim.run_until_stable(2000).unwrap();
    for vector in 0..4u32 {
        for (bit, &(x, y, z)) in inputs.iter().enumerate() {
            let mut b = sim.world().get(x, y, z).clone();
            b.lit = vector & (1 << bit) != 0;
            sim.world_mut().set(x, y, z, b);
        }
        sim.run_until_stable(2000).unwrap();
        for (name, expected) in [
            ("one", true),
            ("zero", false),
            ("y", vector.count_ones() == 1),
        ] {
            for &(x, y, z) in &[lamps[&ports[name]], drivers[&ports[name]]] {
                let b = sim.world().get(x, y, z);
                assert_eq!(
                    if b.kind == BlockKind::RedstoneWire {
                        b.power > 0
                    } else {
                        b.lit
                    },
                    expected
                );
            }
        }
    }
}

use reda::compile::fragment_synth::{
    support_composition::compile_disjoint_support, torch_row::compile_torch_row,
};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use std::collections::BTreeMap;

const SOURCE: &str = "module m(input a,b,c,d,e,f, output u,z,o,v,w,x,y,q,r); assign u=a|b; assign z=1'b0; assign o=1'b1; assign v=c&d; assign w=c&d; assign x=c; assign y=e^f; assign q=~(e|f); assign r=f; endmodule";
// Independent source truth tables, explicitly partitioned for the control.
const CONTROLS: [&str; 3] = [
    "module m(input a,b,output u,z,o); reg[2:0]word; always @* case({a,b}) 2'b00:word=3'b001; default:word=3'b101; endcase assign u=word[2]; assign z=word[1]; assign o=word[0]; endmodule",
    "module m(input c,d,output v,w,x); reg[2:0]word; always @* case({c,d}) 2'b00:word=3'b000; 2'b01:word=3'b000; 2'b10:word=3'b001; default:word=3'b111; endcase assign v=word[2]; assign w=word[1]; assign x=word[0]; endmodule",
    "module m(input e,f,output y,q,r); reg[2:0]word; always @* case({e,f}) 2'b00:word=3'b010; 2'b01:word=3'b101; 2'b10:word=3'b100; default:word=3'b001; endcase assign y=word[2]; assign q=word[1]; assign r=word[0]; endmodule",
];

#[test]
fn independent_leaf_controls_match_every_public_lamp_at_every_game_tick() {
    let native = |text| {
        compile_systemverilog(
            &[SourceInput {
                name: "waveform.sv",
                text,
            }],
            &CompileOptions::new("m"),
        )
        .unwrap()
    };
    let source = native(SOURCE);
    let composed = compile_disjoint_support(&source.netlist, 1_000_000).unwrap();
    let mut simulator = Simulator::new(composed.physical().world().clone());
    simulator.run_until_stable(2000).unwrap();
    let mut controls: Vec<_> = CONTROLS
        .iter()
        .map(|text| {
            let artifact = native(text);
            let physical = compile_torch_row(&artifact.netlist, 1_000_000).unwrap();
            let mut simulator = Simulator::new(physical.physical().world().clone());
            simulator.run_until_stable(2000).unwrap();
            (artifact, physical, simulator)
        })
        .collect();
    let mut frames = 0;
    // Single-bit Gray changes, then simultaneous adversarial changes. Keep
    // all histories and compare the entire transient, not only settled lamps.
    let addresses = (0..64u32).map(|i| i ^ (i >> 1)).chain((0..64u32).map(|i| {
        if i % 2 == 0 {
            i / 2
        } else {
            63 - i / 2
        }
    }));
    for address in addresses {
        let inputs: BTreeMap<_, _> = ["a", "b", "c", "d", "e", "f"]
            .into_iter()
            .enumerate()
            .map(|(bit, name)| (name.to_owned(), address & (1 << bit) != 0))
            .collect();
        let set = |simulator: &mut Simulator, names: &[String], positions: &[(i32, i32, i32)]| {
            for (name, at) in names.iter().zip(positions) {
                let mut lever = simulator.world().get(at.0, at.1, at.2).clone();
                lever.lit = inputs[name];
                simulator.world_mut().set(at.0, at.1, at.2, lever);
            }
        };
        set(
            &mut simulator,
            &source.netlist.inputs,
            composed.source_input_positions(),
        );
        for (artifact, physical, simulator) in &mut controls {
            set(
                simulator,
                &artifact.netlist.inputs,
                physical.source_input_positions(),
            );
        }
        for tick in 0..64 {
            simulator.step();
            for (_, _, simulator) in &mut controls {
                simulator.step();
            }
            let expected: BTreeMap<_, _> = controls
                .iter()
                .flat_map(|(artifact, physical, simulator)| {
                    artifact
                        .ordered_output_ports
                        .iter()
                        .zip(physical.source_output_positions())
                        .map(|(port, at)| {
                            (
                                port.name.clone(),
                                simulator.world().get(at.0, at.1, at.2).lit,
                            )
                        })
                })
                .collect();
            for (port, at) in source
                .ordered_output_ports
                .iter()
                .zip(composed.source_output_positions())
            {
                assert_eq!(
                    simulator.world().get(at.0, at.1, at.2).lit,
                    expected[&port.name],
                    "address {address} tick {tick}: {}",
                    port.name
                );
            }
            frames += 1;
        }
    }
    assert_eq!(frames, 8192);
}

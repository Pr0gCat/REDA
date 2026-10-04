//! Public source occurrences survive native selection without cloning logic.
use reda::frontend::{
    compile_systemverilog, synthesize_selected, CompileOptions, FrontendMode, SourceInput,
};

const SOURCE: &str = "module shared(input a,b, output z,x, output [1:0] pair, output tail);\n\
assign z=a&b; assign x=a&b; assign pair={a,a}; assign tail=a&b; endmodule";

#[test]
fn declared_order_bits_and_shared_drivers_survive_native_artifact_and_selection() {
    let source = SourceInput {
        name: "shared.sv",
        text: SOURCE,
    };
    let artifact =
        compile_systemverilog(&[source.clone()], &CompileOptions::new("shared")).unwrap();
    let bindings: Vec<_> = artifact
        .ordered_output_ports
        .iter()
        .map(|port| (port.name.clone(), port.signal.clone()))
        .collect();
    assert_eq!(
        bindings
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["z", "x", "pair[0]", "pair[1]", "tail"]
    );
    assert_eq!(
        artifact
            .ordered_output_ports
            .iter()
            .map(|port| port.bit)
            .collect::<Vec<_>>(),
        [0, 0, 0, 1, 0]
    );
    assert_eq!(
        artifact.netlist.outputs,
        bindings
            .iter()
            .map(|(_, signal)| signal.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        artifact
            .ports
            .iter()
            .filter(|port| port.direction == reda::frontend::PortDirection::Output)
            .map(|port| port.name.as_str())
            .collect::<Vec<_>>(),
        ["pair[0]", "pair[1]", "tail", "x", "z"]
    );
    assert_eq!(artifact.netlist.outputs[0], artifact.netlist.outputs[1]);
    assert_eq!(artifact.netlist.outputs[0], artifact.netlist.outputs[4]);
    assert_eq!(artifact.netlist.outputs[2], artifact.netlist.outputs[3]);
    // One AND and one legitimate primary-input passthrough buffer; aliases
    // add neither fake gates nor separate drivers.
    assert_eq!(artifact.netlist.gates.len(), 2);
    for mode in [FrontendMode::Native, FrontendMode::Auto] {
        let selected = synthesize_selected(source.clone(), "shared", mode).unwrap();
        assert_eq!(selected.frontend, FrontendMode::Native);
        assert_eq!(selected.output_bindings, bindings);
        assert_eq!(selected.netlist.outputs, artifact.netlist.outputs);
        assert_eq!(selected.outputs, artifact.output_map());
    }
}

#[test]
fn public_circuit_adapter_keeps_every_binding_in_source_order() {
    let (netlist, labels) = reda::circuits::verilog::synthesize_with_frontend(
        SOURCE,
        "shared.sv",
        "shared",
        FrontendMode::Native,
    )
    .unwrap();
    assert_eq!(
        labels
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>(),
        ["z", "x", "pair[0]", "pair[1]", "tail"]
    );
    assert_eq!(
        netlist.outputs,
        labels
            .iter()
            .map(|(_, signal)| signal.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn ordered_vector_bits_keep_their_distinct_source_drivers() {
    let source = SourceInput {
        name: "bits.sv",
        text: "module bits(input a,b, output [1:0] z, output same); assign z={b,a}; assign same=a; endmodule",
    };
    let artifact = compile_systemverilog(&[source], &CompileOptions::new("bits")).unwrap();
    let drivers: Vec<_> = artifact
        .ordered_output_ports
        .iter()
        .map(|port| {
            let gate = artifact
                .netlist
                .gates
                .iter()
                .find(|gate| gate.output == port.signal)
                .unwrap();
            (port.name.as_str(), gate.inputs[0].as_str())
        })
        .collect();
    assert_eq!(drivers, [("z[0]", "a"), ("z[1]", "b"), ("same", "a")]);
    assert_eq!(artifact.netlist.gates.len(), 2);
    assert_eq!(artifact.netlist.outputs.len(), 3);
}

//! Every gate knows which module instance built it, so layout can follow the
//! module tree while logic optimisation stays flat.

use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};

#[test]
fn gates_belong_to_the_instance_that_builds_them_not_the_ones_that_wire_them() {
    let text = include_str!("fixtures/hierarchy_adder16.sv");
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "hierarchy_adder16.sv",
            text,
        }],
        &CompileOptions::new("hierarchy_adder16"),
    )
    .expect("frontend");
    let debug = &artifact.debug;

    // The top, four nibbles, sixteen bits: each child names its parent.
    assert_eq!(debug.instances.len(), 21);
    assert_eq!(debug.instances[0].parent, None);
    let nibbles: Vec<usize> = (0..debug.instances.len())
        .filter(|&index| debug.instances[index].module == "adder4")
        .collect();
    assert_eq!(nibbles.len(), 4);
    for (index, info) in debug.instances.iter().enumerate() {
        if info.module == "full_adder" {
            let parent = info.parent.expect("a full adder sits in a nibble").0 as usize;
            assert!(
                nibbles.contains(&parent),
                "instance {index} hangs off {parent}"
            );
        }
    }

    // Every adder gate is built inside some full adder, though the port
    // connections that feed it belong to the nibble and the top.
    let in_full_adder = debug
        .gates
        .iter()
        .filter(|gate| {
            debug
                .instance_of_gate(gate)
                .is_some_and(|instance| debug.instances[instance.0 as usize].module == "full_adder")
        })
        .count();
    assert_eq!(
        in_full_adder,
        debug.gates.len(),
        "gates attributed to full adders"
    );
}

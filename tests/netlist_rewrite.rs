use reda::compile::{
    equivalence::{prove_netlist_rewrite, NetlistRewriteError},
    Gate, Netlist,
};

fn pair() -> (Netlist, Netlist, Vec<String>, Vec<String>) {
    let source = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs: vec!["y".into()],
        gates: vec![Gate::nor("y", &["a", "b"])],
    };
    let rewritten = Netlist {
        inputs: vec!["right".into(), "left".into()],
        outputs: vec!["out".into()],
        gates: vec![
            Gate::merge("merged", &["left", "right"]),
            Gate::nor("out", &["merged"]),
        ],
    };
    (
        source,
        rewritten,
        vec!["left".into(), "right".into()],
        vec!["out".into()],
    )
}

#[test]
fn complete_rewrite_proof_handles_renamed_ports_and_changed_topology() {
    let (source, rewritten, inputs, outputs) = pair();
    let certificate = prove_netlist_rewrite(&source, &rewritten, &inputs, &outputs, 1000).unwrap();
    assert_eq!(
        certificate,
        prove_netlist_rewrite(&source, &rewritten, &inputs, &outputs, 1000).unwrap()
    );
    assert_ne!(
        certificate.source_netlist_hash(),
        certificate.rewritten_netlist_hash()
    );
    let report = serde_json::to_value(&certificate).unwrap();
    assert_eq!(report["input_vectors_checked"], 4);
    let exact = report["work_used"].as_u64().unwrap();
    assert!(prove_netlist_rewrite(&source, &rewritten, &inputs, &outputs, exact).is_ok());
    assert!(matches!(
        prove_netlist_rewrite(&source, &rewritten, &inputs, &outputs, exact - 1),
        Err(NetlistRewriteError::ProofExhausted { .. })
    ));
}

#[test]
fn wrong_functions_and_incomplete_or_duplicate_bindings_fail_closed() {
    let (source, mut rewritten, inputs, outputs) = pair();
    rewritten.gates[1] = Gate::merge("out", &["left", "right"]);
    assert!(matches!(
        prove_netlist_rewrite(&source, &rewritten, &inputs, &outputs, 1000),
        Err(NetlistRewriteError::OutputMismatch { .. })
    ));
    let (source, rewritten, inputs, outputs) = pair();
    for bad in [
        vec![],
        vec!["left".into(), "left".into()],
        vec!["unknown".into(), "right".into()],
    ] {
        assert_eq!(
            prove_netlist_rewrite(&source, &rewritten, &bad, &outputs, 1000),
            Err(NetlistRewriteError::InvalidPortBindings)
        );
    }
    assert_eq!(
        prove_netlist_rewrite(&source, &rewritten, &inputs, &[], 1000),
        Err(NetlistRewriteError::InvalidPortBindings)
    );
}

#[test]
fn invalid_graphs_and_exponential_work_are_rejected_before_enumeration() {
    let (source, rewritten, inputs, outputs) = pair();
    let mut bad = source.clone();
    bad.gates[0] = Gate::nor("y", &["y"]);
    assert!(matches!(
        prove_netlist_rewrite(&bad, &rewritten, &inputs, &outputs, 1000),
        Err(NetlistRewriteError::InvalidNetlist { .. })
    ));
    bad = source.clone();
    bad.gates[0].inputs.push("missing".into());
    assert!(matches!(
        prove_netlist_rewrite(&bad, &rewritten, &inputs, &outputs, 1000),
        Err(NetlistRewriteError::InvalidNetlist { .. })
    ));
    bad = source.clone();
    bad.gates.push(Gate::nor("a", &["b"]));
    assert!(matches!(
        prove_netlist_rewrite(&bad, &rewritten, &inputs, &outputs, 1000),
        Err(NetlistRewriteError::InvalidNetlist { .. })
    ));
    bad = source.clone();
    bad.gates[0].kind = reda::compile::topology::GateKind::DffPosedge;
    assert!(matches!(
        prove_netlist_rewrite(&bad, &rewritten, &inputs, &outputs, 1000),
        Err(NetlistRewriteError::InvalidNetlist { .. })
    ));
    bad = source.clone();
    bad.gates[0].inputs.clear();
    assert!(matches!(
        prove_netlist_rewrite(&bad, &rewritten, &inputs, &outputs, 1000),
        Err(NetlistRewriteError::InvalidNetlist { .. })
    ));
    let wide = Netlist {
        inputs: (0..64).map(|i| format!("in{i}")).collect(),
        outputs: vec![],
        gates: vec![],
    };
    assert!(matches!(
        prove_netlist_rewrite(&wide, &wide, &wide.inputs, &[], u64::MAX),
        Err(NetlistRewriteError::ProofExhausted { .. })
    ));
    let empty = Netlist {
        inputs: vec![],
        outputs: vec![],
        gates: vec![],
    };
    assert!(matches!(
        prove_netlist_rewrite(&empty, &empty, &[], &[], 0),
        Err(NetlistRewriteError::ProofExhausted { .. })
    ));
}

#[test]
fn output_aliases_and_port_permutations_are_checked_as_complete_boundaries() {
    let (mut source, mut rewritten, inputs, outputs) = pair();
    source.outputs.push(source.outputs[0].clone());
    rewritten.outputs.push(rewritten.outputs[0].clone());
    let bindings = vec![outputs[0].clone(), outputs[0].clone()];
    assert!(prove_netlist_rewrite(&source, &rewritten, &inputs, &bindings, 1000).is_ok());
    assert_eq!(
        prove_netlist_rewrite(&source, &rewritten, &inputs, &outputs, 1000),
        Err(NetlistRewriteError::InvalidPortBindings)
    );
    source.outputs = vec!["y".into()];
    source.gates = vec![Gate::nor("y", &["a"])];
    rewritten.outputs = outputs.clone();
    rewritten.gates = vec![Gate::nor("out", &["left"])];
    assert!(prove_netlist_rewrite(&source, &rewritten, &inputs, &outputs, 1000).is_ok());
    let swapped = vec![inputs[1].clone(), inputs[0].clone()];
    assert!(matches!(
        prove_netlist_rewrite(&source, &rewritten, &swapped, &outputs, 1000),
        Err(NetlistRewriteError::OutputMismatch { .. })
    ));
}

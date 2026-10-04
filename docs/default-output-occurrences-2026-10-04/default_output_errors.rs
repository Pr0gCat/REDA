use reda::compile::{
    selection::compile_selected_with_refresh_budget, topology::GateKind, CompileError, Gate,
    Netlist,
};

#[test]
fn occurrence_owner_preserves_realisability_driver_and_cycle_errors() {
    let source = |gate| Netlist {
        inputs: (0..4).map(|bit| format!("input{bit}")).collect(),
        outputs: vec!["input0".into(), "bad".into(), "bad".into()],
        gates: vec![gate],
    };
    for budget in [0, 256] {
        let unsupported = source(Gate {
            name: "bad".into(),
            output: "bad".into(),
            kind: GateKind::And,
            inputs: vec!["input0".into(), "input1".into()],
        });
        assert!(matches!(
            compile_selected_with_refresh_budget(&unsupported, budget),
            Err(CompileError::NotRealisable { .. })
        ));
        let missing = source(Gate::nor("bad", &["missing"]));
        assert!(
            matches!(compile_selected_with_refresh_budget(&missing,budget),Err(CompileError::UndrivenSignal(name)) if name=="missing")
        );
        let cycle = source(Gate::nor("bad", &["bad"]));
        assert!(matches!(
            compile_selected_with_refresh_budget(&cycle, budget),
            Err(CompileError::CyclicNetlist)
        ));
    }
}

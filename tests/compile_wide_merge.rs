//! A Boolean merge that cannot fit a cell is a typed error, never a panic.
use reda::compile::{compile, CompileError, Gate, Netlist};
#[test]
fn wide_merges_are_typed_errors() {
    for count in [4, 5, 7] {
        let inputs: Vec<_> = (0..count).map(|i| format!("i{i}")).collect();
        let refs: Vec<_> = inputs.iter().map(String::as_str).collect();
        let merge = Gate::merge("output", &refs);
        let source = Netlist {
            inputs,
            outputs: vec!["output".into()],
            gates: vec![merge],
        };
        assert!(
            matches!(compile(&source),Err(CompileError::NotRealisable{gate,..}) if gate=="output")
        );
    }
}

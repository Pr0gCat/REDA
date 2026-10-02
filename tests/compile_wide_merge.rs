//! A Boolean merge that cannot fit a cell is a typed error, never a panic.
use reda::compile::{
    compile, compile_legacy, compile_without_rewrite, CompileError, Gate, Netlist,
};
#[test]
fn wide_merges_are_typed_errors_at_all_compiler_entries() {
    for count in [4, 5, 7] {
        let inputs: Vec<_> = (0..count).map(|i| format!("i{i}")).collect();
        let refs: Vec<_> = inputs.iter().map(String::as_str).collect();
        let merge = Gate::merge("output", &refs);
        let source = Netlist {
            inputs,
            outputs: vec!["output".into()],
            gates: vec![merge],
        };
        for entry in [compile, compile_without_rewrite, compile_legacy] {
            assert!(
                matches!(entry(&source),Err(CompileError::NotRealisable{gate,..}) if gate=="output")
            );
        }
    }
}

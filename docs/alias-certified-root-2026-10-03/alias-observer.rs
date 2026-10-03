use reda::compile::{Gate, Netlist};
use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisInput, SynthesisBudget};
fn main() {
    let mut failures = 0;
    for (name, netlist) in [
        ("pure", Netlist { inputs: vec!["a".into()], outputs: vec!["a".into()], gates: vec![] }),
        ("mixed", Netlist { inputs: vec!["a".into(), "b".into()], outputs: vec!["a".into(), "y".into(), "a".into()], gates: vec![Gate::nor("y", &["a", "b"])] }),
    ] {
        let result = std::panic::catch_unwind(|| compile_fragment_synth(SynthesisInput { lowered: &netlist,
            source_provenance: None, pins: None }, SynthesisBudget::Evaluations(0)));
        match result {
            Ok(Ok(product)) => println!("{name} accepted inputs={:?} outputs={:?} metrics={:?}", product.compiled.input_positions, product.compiled.output_positions, product.metrics),
            Ok(Err(error)) => { failures += 1; println!("{name} refused: {error}"); },
            Err(_) => { failures += 1; println!("{name} panicked before certification"); },
        }
    }
    assert_eq!(failures, 0, "every alias fixture must compile and certify");
}

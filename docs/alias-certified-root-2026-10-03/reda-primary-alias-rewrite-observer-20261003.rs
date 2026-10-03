use reda::compile::{Gate, Netlist};
use reda::compile::fragment_synth::{config::{SearchConfig, CertificationConfig},torch_row::compile_player_torch_row};
fn main() {
    let source=Netlist {inputs:vec!["a".into(),"b".into()],outputs:vec!["a".into(),"y".into(),"a".into()],gates:vec![Gate::nor("y", &["a","b"])]};
    let rewrite=compile_player_torch_row(&source,SearchConfig::checked_defaults().max_equivalence_proof_steps).expect("existing physical rewrite certifies primary aliases");
    let runtime=rewrite.certify_runtime(&CertificationConfig::from_search(&SearchConfig::checked_defaults())).expect("retained histories certify");
    assert_eq!(rewrite.source(),&source);
    assert_eq!(rewrite.source_output_positions().len(),3);
    println!("accepted outputs={:?} metrics={:?}",rewrite.source_output_positions(),runtime.metrics());
}

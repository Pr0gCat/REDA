use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
fn main() {
    for (file,top) in [("tests/fixtures/seven_segment.v","bcd_seven_segment"),("tests/fixtures/rom16x7.sv","rom16x7")] {
        let text=std::fs::read_to_string(file).unwrap();
        let source=compile_systemverilog(&[SourceInput{name:file,text:&text}],&CompileOptions::new(top)).unwrap().netlist;
        eprintln!("CASE {} gates={}",top,source.gates.len());
        let compiled=reda::compile::compile(&source).unwrap();
        eprintln!("WORLD {} {}",top,reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&compiled.world).as_str());
    }
}

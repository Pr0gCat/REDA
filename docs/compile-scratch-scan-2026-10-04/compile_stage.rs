use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let text = std::fs::read_to_string(&args[1]).unwrap();
    let started = std::time::Instant::now();
    let source = compile_systemverilog(&[SourceInput { name: &args[2], text: &text }], &CompileOptions::new(&args[2])).unwrap().netlist;
    eprintln!("SOURCE seconds={} inputs={} outputs={} gates={}", started.elapsed().as_secs_f64(), source.inputs.len(), source.outputs.len(), source.gates.len());
    let result = reda::compile::compile_without_rewrite(&source);
    eprintln!("TERMINAL seconds={} success={}", started.elapsed().as_secs_f64(), result.is_ok());
    let compiled = result.unwrap();
    eprintln!("WORLD {}", reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&compiled.world).as_str());
    reda::formats::litematic::save(std::path::Path::new("retained.litematic"), &compiled.world, "ROM64 stage diagnosis").unwrap();
}

fn main(){
 let text=std::fs::read_to_string("tests/fixtures/rom16x7.sv").unwrap();
 let source=reda::frontend::compile_systemverilog(&[reda::frontend::SourceInput{name:"rom16x7",text:&text}],&reda::frontend::CompileOptions::new("rom16x7")).unwrap().netlist;
 let certified=reda::compile::fragment_synth::torch_row::compile_balanced_torch_matrix(&source,10_000_000).unwrap();
 let world=certified.physical().world();
 let fingerprint=reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(world);
 assert_eq!(fingerprint.as_str(),"f4658639e48b15d789732aa49f27db0031fd4e45f04212d65a1bf21d5adbe7ce");
 let path=std::path::Path::new("docs/balanced-matrix-default-2026-10-03-rom.litematic");
 reda::formats::litematic::save(path,world,"balanced-matrix-certified-rom").unwrap();
 assert_eq!(fingerprint,reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&reda::formats::litematic::load(path).unwrap()));
 println!("REEXPORTED_CERTIFIED_ROM {}",fingerprint.as_str());
}

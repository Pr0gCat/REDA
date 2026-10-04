use std::path::Path;
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::compile::{compile,lowering::lower_optimised,fragment_synth::benchmark::canonical_world_fingerprint};
use reda::redstone::simulator::Simulator;
fn main() {
 let source=std::fs::read_to_string("internal_inout.sv").unwrap();
 let artifact=compile_systemverilog(&[SourceInput{name:"internal_inout.sv",text:&source}],&CompileOptions::new("m")).unwrap();
 let lowered=lower_optimised(&artifact.netlist).unwrap();let expected=compile(&lowered).unwrap();
 for mode in ["native","auto"] {
  let world=reda::formats::litematic::load(Path::new(&format!("qualified-{mode}/output/m.litematic"))).unwrap();
  assert_eq!(world.size(),expected.world.size());
  assert_eq!(canonical_world_fingerprint(&world),canonical_world_fingerprint(&expected.world));
  let mut sim=Simulator::new(world);sim.run_until_stable(2000).unwrap();let mut phases=0;
  for from in 0..16usize {for to in 0..16usize {for address in [from,to] {
   for bit in 0..4 {let &(x,y,z)=&expected.input_positions[&format!("a[{bit}]")];let mut lever=sim.world().get(x,y,z).clone();lever.lit=address&(1<<bit)!=0;sim.world_mut().set(x,y,z,lever);}
   sim.run_until_stable(2000).unwrap();
   for (port,mask) in [("y",5),("z",10)] {for bit in 0..4 {
    let label=format!("{port}[{bit}]");let signal=&artifact.output_map()[&label];let (x,y,z)=expected.output_positions[signal];
    assert_eq!(sim.world().get(x,y,z).lit,(address^mask)&(1<<bit)!=0,"{mode} {from}->{to} {label}");
   }}
   phases+=1;
  }}}
  println!("{mode}: {phases} continuous exported-world phases; eight lamps match independent XOR-5/XOR-10 oracle");
 }
}

use reda::compile::{Gate, Netlist, topology::GateKind};
use reda::compile::fragment_synth::{torch_row::compile_balanced_torch_matrix, benchmark::canonical_world_fingerprint};
use reda::formats::litematic::{save,load};
use reda::redstone::{simulator::{Simulator}, world::block::BlockKind};
fn gate(name:String,kind:GateKind,inputs:Vec<String>)->Gate { Gate{name:name.clone(),output:name,kind,inputs} }
#[test]
fn wider_rom_full_implementation_and_boundary_truth() {
 let words:[u16;16]=[0x9653,0x2ad4,0xc138,0x73e1,0x4b29,0xe592,0x187f,0xab64,0x6d83,0xf04a,0x35bc,0x82d7,0x59e8,0xd126,0x07fb,0xbe45];
 let mut source=Netlist{inputs:(0..4).map(|b|format!("a{b}")).collect(),outputs:(0..16).map(|b|format!("q{b}")).collect(),gates:vec![]};
 for bit in 0..4 { source.gates.push(gate(format!("not{bit}"),GateKind::Nor(1),vec![format!("a{bit}")])); }
 for address in 0..16 { source.gates.push(gate(format!("row{address}"),GateKind::Nor(4),(0..4).map(|bit|if address&(1<<bit)==0 {format!("a{bit}")}else{format!("not{bit}")}).collect())); }
 for bit in 0..16 {let inputs:Vec<_>=(0..16).filter(|&a| words[a]&(1<<bit)!=0).map(|a|format!("row{a}")).collect();source.gates.push(gate(format!("q{bit}"),GateKind::Or(inputs.len()),inputs));}
 let rewrite=compile_balanced_torch_matrix(&source,10_000_000).unwrap();
 let physical=rewrite.physical();
 let path=std::path::Path::new("/tmp/reda-wide-collector-20261003/wide-rom.litematic");save(path,physical.world(),"wide-rom").unwrap();
 let mut sim=Simulator::new(load(path).unwrap());sim.run_until_stable(2000).unwrap();
 let inputs=rewrite.source_input_positions();let drivers=rewrite.source_output_driver_positions();let lamps=rewrite.source_output_positions();
 assert_eq!(drivers.len(),16);assert_eq!(lamps.len(),16);
 for from in 0..16 {for to in 0..16 {for address in [from,to] {
  for (bit,&(x,y,z)) in inputs.iter().enumerate(){let mut lever=sim.world().get(x,y,z).clone();lever.lit=address&(1<<bit)!=0;sim.world_mut().set(x,y,z,lever);}
  sim.run_until_stable(2000).unwrap();
  for bit in 0..16 {let expected=words[address]&(1<<bit)!=0;for &(x,y,z) in [&drivers[bit],&lamps[bit]] {let s=sim.world().get(x,y,z);let value=if s.kind==BlockKind::RedstoneWire{s.power>0}else{s.lit};assert_eq!(value,expected,"address {address} output {bit}");}}
 }}}
 println!("WIDE_ROM fingerprint={} phases=512 outputs=16",canonical_world_fingerprint(sim.world()).as_str());
}

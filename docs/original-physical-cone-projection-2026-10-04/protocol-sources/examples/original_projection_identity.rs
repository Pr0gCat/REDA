use reda::compile::{compile, lowering::lower_optimised, Netlist, fragment_synth::{original_projection::project_original,candidate::ExpandedPhysicalCandidate,benchmark::canonical_world_fingerprint}};
use reda::frontend::{compile_systemverilog,CompileOptions,SourceInput};
use reda::redstone::world::{storage::World,block::BlockKind};
use flate2::{write::GzEncoder,Compression};
fn graph(n:&Netlist)->serde_json::Value{serde_json::json!({"inputs":n.inputs,"outputs":n.outputs,"gates":n.gates.iter().map(|g|serde_json::json!({"name":g.name,"kind":format!("{:?}",g.kind),"inputs":g.inputs,"output":g.output})).collect::<Vec<_>>()})}
fn manifest(c:&ExpandedPhysicalCandidate)->serde_json::Value{
 serde_json::json!({"fingerprint":c.fingerprint(),"instances":c.instances,
 "placements":c.placements.values().map(|p|serde_json::json!({"id":p.id,"variant":p.variant,"facing":format!("{:?}",p.facing),"anchor":p.anchor,"delayed":p.delayed,"blocks":p.blocks})).collect::<Vec<_>>(),
 "boundaries":c.boundaries.values().collect::<Vec<_>>(),"output_boundary_aliases":c.output_boundary_aliases.iter().collect::<Vec<_>>(),
 "connections":c.connections.values().collect::<Vec<_>>(),"routes":c.routes.values().collect::<Vec<_>>(),"repeater_handovers":c.repeater_handovers.iter().collect::<Vec<_>>(),
 "junctions":c.junctions.values().map(|p|serde_json::json!({"id":p.id,"at":p.at,"facing":format!("{:?}",p.facing),"contributors":p.contributors,"cells":p.cells})).collect::<Vec<_>>(),
 "observations":c.observations.values().collect::<Vec<_>>(),"pins_empty":c.pins.is_empty(),"pin_contracts_empty":c.pin_contracts.is_empty(),"pin_name_bindings_empty":c.pin_name_bindings.is_empty()})
}
fn world(path:&std::path::Path,w:&World){let mut cells=vec![];let (sx,sy,sz)=w.size();for y in 0..sy{for z in 0..sz{for x in 0..sx{let state=w.get(x,y,z);if state.kind!=BlockKind::Air{cells.push(((x,y,z),state));}}}}let enc=GzEncoder::new(std::fs::File::create(path).unwrap(),Compression::default());serde_json::to_writer(enc,&serde_json::json!({"size":w.size(),"fingerprint":canonical_world_fingerprint(w),"cells":cells})).unwrap();}
fn main(){
 let source="module m(input a,b,c,d,e,f, output u,z,o,v,w,x,y,q,r); assign u=a|b; assign z=1'b0; assign o=1'b1; assign v=c&d; assign w=c&d; assign x=c; assign y=e^f; assign q=~(e|f); assign r=f; endmodule";
 let a=compile_systemverilog(&[SourceInput{name:"source.sv",text:source}],&CompileOptions::new("m")).unwrap();let n=lower_optimised(&a.netlist).unwrap();let c=compile(&n).unwrap();
 let out=std::env::args().nth(1).unwrap();let dir=std::path::Path::new(&out);std::fs::create_dir_all(dir).unwrap();std::fs::write(dir.join("source.sv"),source).unwrap();std::fs::write(dir.join("source-netlist.json"),serde_json::to_vec_pretty(&graph(&n)).unwrap()).unwrap();
 match project_original(&n,&c){Ok(p)=>{
  std::fs::write(dir.join("identity-result.json"),serde_json::to_vec_pretty(&p.report).unwrap()).unwrap();
  world(&dir.join("original-whole-world.json.gz"),&c.world);
  for (name,r) in p.leaves.iter().enumerate().map(|(i,r)|(format!("leaf{i}"),r)).chain(std::iter::once(("root".into(),&p.original_root))) {
   std::fs::write(dir.join(format!("{name}-manifest.json")),serde_json::to_vec_pretty(&manifest(r.implemented_candidate())).unwrap()).unwrap();
   std::fs::write(dir.join(format!("{name}-source-netlist.json")),serde_json::to_vec_pretty(&graph(r.source())).unwrap()).unwrap();
   world(&dir.join(format!("{name}-world.json.gz")),r.physical().world());
  }
  println!("original physical projection: {} leaves, identity accepted",p.leaves.len());
 },Err(e)=>{std::fs::write(dir.join("identity-failure.json"),serde_json::to_vec_pretty(&serde_json::json!({"accepted":false,"error":e})).unwrap()).unwrap();eprintln!("projection refused: {e}");}}
}

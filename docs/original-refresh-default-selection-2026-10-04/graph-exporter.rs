use reda::compile::{compile, compile_without_rewrite, lowering::lower_optimised, Netlist, fragment_synth::{benchmark::canonical_world_fingerprint}};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use std::path::Path;
const SOURCE:&str="module m(input a,b,c,d,e,f, output u,z,o,v,w,x,y,q,r); assign u=a|b; assign z=1'b0; assign o=1'b1; assign v=c&d; assign w=c&d; assign x=c; assign y=e^f; assign q=~(e|f); assign r=f; endmodule";
fn graph(n:&Netlist)->serde_json::Value {serde_json::json!({"inputs":n.inputs,"outputs":n.outputs,"gates":n.gates.iter().map(|g|serde_json::json!({"name":g.name,"kind":format!("{:?}",g.kind),"inputs":g.inputs,"output":g.output})).collect::<Vec<_>>()})}
fn main(){
 let dir=std::env::args().nth(1).unwrap();let out=Path::new(&dir);
 let a=compile_systemverilog(&[SourceInput{name:"source.sv",text:SOURCE}],&CompileOptions::new("m")).unwrap();
 let source=lower_optimised(&a.netlist).unwrap();
 let baseline=compile_without_rewrite(&source).unwrap();
 let candidate=compile(&source).unwrap();
 for (name,c) in [("baseline",baseline),("candidate",candidate)] {
  let value=serde_json::json!({"source":graph(&source),"actual_implementation":graph(c.implementation_netlist(&source)),"world_fingerprint":canonical_world_fingerprint(&c.world)});
  std::fs::write(out.join(format!("{name}-actual-implementation.json")),serde_json::to_vec_pretty(&value).unwrap()).unwrap();
 }
}

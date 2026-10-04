use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::frontend::evaluate::Evaluator;
#[test]
fn assignment_context_observation() {
 let cases=[
  ("signed-literal", "module m(output [7:0] y); assign y=4'shf; endmodule",255u64),
  ("mixed-bitwise", "module m(output [7:0] y); assign y=4'shf & 8'hff; endmodule",15),
  ("signed-bitwise", "module m(output [7:0] y); assign y=4'shf & 8'shff; endmodule",255),
  ("mixed-comparison", "module m(output [7:0] y); assign y=4'shf == 8'hff; endmodule",0),
  ("signed-comparison", "module m(output [7:0] y); assign y=4'shf == 8'shff; endmodule",1),
  ("mixed-ternary", "module m(output [7:0] y); assign y=1'b1 ? 4'shf : 8'h0; endmodule",15),
  ("signed-ternary", "module m(output [7:0] y); assign y=1'b1 ? 4'shf : 8'sh0; endmodule",255),
  ("concat-self-determined", "module m(output [7:0] y); assign y={4'shf}; endmodule",15),
  ("concat-target", "module m(output [7:0] y); assign {y[7:4],y[3:0]}=4'shf; endmodule",255),
  ("procedural", "module m(output logic [7:0] y); always_comb y=4'shf; endmodule",255),
  ("case-context", "module m(output logic [7:0] y); always_comb begin case(1'b0) 1'b0:y=4'shf; default:y=4'h0; endcase end endmodule",255),
  ("hierarchy-output", "module child(output signed [3:0] o); assign o=4'shf; endmodule module m(output [7:0] y); child u(y); endmodule",255),
  ("hierarchy-input", "module child(input [7:0] a,output [7:0] o); assign o=a; endmodule module m(output [7:0] y); child u(4'shf,y); endmodule",255),

  ("context-bitwise-inversion", "module m #(parameter logic [3:0] P=4'hf)(output [7:0] y); assign y=~P; endmodule",240u64),
  ("signed-bitwise-inversion", "module m #(parameter logic signed [3:0] P=4'shf)(output [7:0] y); assign y=~P; endmodule",0),
  ("logical-not-self-determined", "module m #(parameter logic [3:0] P=4'h0)(output [7:0] y); assign y=!P; endmodule",1),
  ("comparison-self-determined", "module m #(parameter logic [3:0] P=4'h0)(output [7:0] y); assign y=P!=4'hf; endmodule",1),
  ("signed-parameter-extension", "module m #(parameter logic signed [3:0] P=4'shf)(output [7:0] y); assign y=P; endmodule",255u64),
  ("unsigned-parameter-extension", "module m #(parameter logic [3:0] P=4'hf)(output [7:0] y); assign y=P; endmodule",15),
  ("assignment-truncation", "module m #(parameter logic [7:0] P=8'hab)(output [3:0] y); assign y=P; endmodule",11),
 ];
 let mut missing=0;
 for (label,text,expected) in cases {
  match compile_systemverilog(&[SourceInput{name:"context.sv",text}],&CompileOptions::new("m")) {
   Err(e)=>{eprintln!("MISSING {label}: {e:?}");missing+=1;},
   Ok(a)=>{let values=Evaluator::new(&a.netlist).unwrap().evaluate(&Default::default()).unwrap();let mut word=0u64;for (name,signal) in a.output_map(){let bit:usize=name.trim_start_matches("y[").trim_end_matches(']').parse().unwrap();word|=(values[&signal] as u64)<<bit;}assert_eq!(word,expected,"{label}");eprintln!("SUPPORTED {label} {word}");}
  }
 }
 assert_eq!(missing,0,"required contextual sizing cases missing");
}

#[test]
fn signal_signedness_and_self_determined_selections() {
 use std::collections::BTreeMap;
 for (label,body,sign_extend,invert) in [
  ("signed signal", "assign y=a;", true,false),
  ("signed inverse", "assign y=~a;", true,true),
  ("unsigned mixed inverse", "assign y=~a & 8'hff;", false,true),
  ("part selection", "assign y=a[3:0];", false,false),
  ("concatenation", "assign y={a};", false,false),
  ("internal signed net", "wire signed [3:0] t; assign t=a; assign y=t;",true,false),
  ("procedural signed net", "logic signed [3:0] t; always_comb begin t=a; y=t; end",true,false),
 ] {
  let text=format!("module m(input signed [3:0] a, output logic [7:0] y); {body} endmodule");
  let a=compile_systemverilog(&[SourceInput{name:"signals.sv",text:&text}],&CompileOptions::new("m")).unwrap();
  let evaluator=Evaluator::new(&a.netlist).unwrap();
  for word in 0..16u64 {
   let inputs:BTreeMap<_,_>=(0..4).map(|bit|(format!("a[{bit}]"),word&(1<<bit)!=0)).collect();
   let values=evaluator.evaluate(&inputs).unwrap();
   let actual=(0..8).fold(0u64,|v,bit|v|((values[&a.output_map()[&format!("y[{bit}]")]] as u64)<<bit));
   let mut expected=if sign_extend && word&8!=0 {word|240}else{word};if invert{expected^=255;}
   assert_eq!(actual,expected,"{label} input{word}");
  }
 }
}

#[test]
fn clocked_assignment_context_matches_explicit_source() {
 for event in ["posedge","negedge"] {
  let source=format!("module m(input clk,output logic [7:0] y); always_ff @({event} clk) y<=4'shf; endmodule");
  let reference=source.replace("4'shf","8'hff");
  let compile=|text:&str|compile_systemverilog(&[SourceInput{name:"dff.sv",text}],&CompileOptions::new("m")).unwrap();
  assert_eq!(compile(&source).netlist,compile(&reference).netlist);
 }
}

#[test]
fn widened_signed_source_matches_explicit_physical_control() {
 use reda::compile::{compile,lowering::lower_optimised};
 use reda::redstone::{simulator::Simulator,world::block::BlockKind};
 use std::collections::BTreeMap;
 let source="module m(input signed [3:0] a, output [7:0] y); assign y=a; endmodule";
 let reference="module m(input [3:0] a, output [7:0] y); assign y={a[3],a[3],a[3],a[3],a}; endmodule";
 let native=|text:&str|compile_systemverilog(&[SourceInput{name:"physical.sv",text}],&CompileOptions::new("m")).unwrap();
 let a=native(source);let b=native(reference);
 let net=lower_optimised(&a.netlist).unwrap();let control_net=lower_optimised(&b.netlist).unwrap();assert_eq!(net,control_net);
 let physical=compile(&net).unwrap();let control=compile(&control_net).unwrap();
 let hash=reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
 assert_eq!(hash(&physical.world),hash(&control.world));
 physical.validate_rewrite_view(&net).unwrap();
 let implementation=physical.implementation_netlist(&net);let oracle=Evaluator::new(implementation).unwrap();
 let mut sim=Simulator::new(physical.world.clone());sim.run_until_stable(2000).unwrap();
 for from in 0..16u64 {for to in 0..16u64 {for word in [from,to] {
  let inputs:BTreeMap<_,_>=(0..4).map(|bit|(format!("a[{bit}]"),word&(1<<bit)!=0)).collect();
  for (name,at) in &physical.input_positions {let mut state=sim.world().get(at.0,at.1,at.2).clone();state.lit=inputs[name];sim.world_mut().set(at.0,at.1,at.2,state);}
  sim.run_until_stable(2000).unwrap();
  assert_eq!(implementation.inputs.len(),net.inputs.len());
  let actual_inputs:BTreeMap<_,_>=implementation.inputs.iter().zip(&net.inputs).map(|(actual,source)|(actual.clone(),inputs[source])).collect();
  let values=oracle.settle(&actual_inputs).unwrap();
  for gate in &implementation.gates {let at=physical.gate_output_positions[&gate.output];let state=sim.world().get(at.0,at.1,at.2);let actual=match state.kind {BlockKind::RedstoneWire=>state.power>0,BlockKind::Torch|BlockKind::WallTorch|BlockKind::Repeater|BlockKind::Comparator=>state.lit,_=>panic!("unknown observation")};assert_eq!(actual,values[&gate.output]);}
  let expected=if word&8!=0 {word|240}else{word};
  for bit in 0..8 {let at=physical.output_positions[&a.output_map()[&format!("y[{bit}]")]];assert_eq!(sim.world().get(at.0,at.1,at.2).lit,expected&(1<<bit)!=0);}
 }}}
}

#[test]
fn context_origin_bits_have_real_elaborated_widths() {
 for text in [
 "module m(input signed [3:0] a,output [7:0] y); assign y=a; endmodule",
 "module m(output [7:0] y); assign y=4'shf; endmodule",
 "module m(input signed [3:0] a,output [7:0] y); assign y=~a; endmodule",
 "module child(output signed [3:0] o); assign o=4'shf; endmodule module m(output [7:0] y); child u(y); endmodule",
 ] {
  let a=compile_systemverilog(&[SourceInput{name:"origins.sv",text}],&CompileOptions::new("m")).unwrap();
  for origin in a.debug.logic_origins.iter().chain(a.debug.extra_origins.iter().map(|(_,o)|o)) {
   let node=&a.debug.elab_nodes[origin.elab.0 as usize];
   assert!(origin.bit<node.width,"source{text}: origin{origin:?} exceeds{node:?}");
  }
 }
}

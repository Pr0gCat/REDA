use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
#[test]
fn widening_preserves_declaration_types_and_context_ownership() {
 for text in [
  "module m(input signed [3:0] a,output [7:0] y); assign y=a; endmodule",
  "module m(input signed [3:0] a,output [7:0] y); assign y=~a; endmodule",
  "module child(output signed [3:0] o); assign o=4'shf; endmodule module m(output [7:0] y); child u(y); endmodule",
 ] {
  let a=compile_systemverilog(&[SourceInput{name:"ownership.sv",text}],&CompileOptions::new("m")).unwrap();
  let declarations:Vec<_>=a.debug.elab_nodes.iter().filter(|n| matches!(serde_json::to_value(n.kind).unwrap().as_str(),Some("Input"|"Output"|"Net"))).collect();
  assert!(declarations.iter().any(|n|n.width==4),"narrow source declaration lost: {text}");
  assert!(declarations.iter().any(|n|n.width==8),"wide destination declaration lost: {text}");
  assert_eq!(declarations.len(),2,"context must not invent declarations: {text}");
  for origin in a.debug.logic_origins.iter().chain(a.debug.extra_origins.iter().map(|(_,o)|o)) {
   let node=&a.debug.elab_nodes[origin.elab.0 as usize];
   assert!(origin.bit<node.width);
   if origin.bit>=4 { assert!(!matches!(serde_json::to_value(node.kind).unwrap().as_str(),Some("Input"|"Net")),"extension attributed to narrow declaration: {origin:?}"); }
  }
 }
}

#[test]
fn formerly_rejected_sources_have_exhaustive_independent_truth() {
 use reda::frontend::evaluate::Evaluator;
 let cases=[
 ("module m(input logic [1:0] a, output logic y);\n  assign y = a;\nendmodule\n",vec!["a[0]","a[1]"],vec!["y"],0),
 ("module m(input logic a, input logic [1:0] b, output logic y);\n  assign y = a & b;\nendmodule\n",vec!["a","b[0]","b[1]"],vec!["y"],1),
 ("module m(input logic a, output logic y);\n  always_comb y = a;\nendmodule\n",vec!["a"],vec!["y"],0),
 ("module m(input logic a, output logic y);\n  assign y = 2'b11;\nendmodule\n",vec!["a"],vec!["y"],2),
 ("module m(input [2:0] a, output y,z); assign {y,z} = a; endmodule",vec!["a[0]","a[1]","a[2]"],vec!["z","y"],0),
 ("module word #(parameter W=1)(input [W-1:0] a,output [W-1:0] q); localparam L=1; assign q=a; endmodule  module m(input a,output q); word #(.W(2)) u(a,q); endmodule",vec!["a"],vec!["q"],0),
 ];
 for (text,inputs,outputs,mode) in cases {
 let a=compile_systemverilog(&[SourceInput{name:"retained.sv",text}],&CompileOptions::new("m")).unwrap();
 let e=Evaluator::new(&a.netlist).unwrap();
 for word in 0..(1usize<<inputs.len()) {
 let values=e.evaluate(&inputs.iter().enumerate().map(|(i,n)|(n.to_string(),word&(1<<i)!=0)).collect()).unwrap();
 let expected=match mode {1=>(word&1)&((word>>1)&1),2=>1,_=>word};
 for (bit,name) in outputs.iter().enumerate(){assert_eq!(values[&a.output_map()[*name]],expected&(1<<bit)!=0,"{text}, word {word}, output {name}");}
 }
 }
}

#[test]
fn integral_context_width_boundaries_preserve_all_output_bits() {
 use reda::frontend::evaluate::Evaluator;
 for (text,expected) in [
 ("module m(output [63:0] y); assign y=1'sb1; endmodule",u64::MAX),
 ("module m(output [63:0] y); assign y=1'b1; endmodule",1),
 ("module m(output [63:0] y); assign y=64'sh8000000000000000; endmodule",1u64<<63),
 ("module child(output signed [63:0] o); assign o=64'sh8000000000000001; endmodule module m(output y); child u(y); endmodule",1),
 ("module child(output signed o); assign o=1'sb1; endmodule module m(output [63:0] y); child u(y); endmodule",u64::MAX),
 ] {
 let a=compile_systemverilog(&[SourceInput{name:"bounds.sv",text}],&CompileOptions::new("m")).unwrap();
 let values=Evaluator::new(&a.netlist).unwrap().evaluate(&Default::default()).unwrap();
 for (name,signal) in a.output_map(){let bit=if name=="y"{0}else{name.trim_start_matches("y[").trim_end_matches(']').parse::<usize>().unwrap()};assert_eq!(values[&signal],expected&(1u64<<bit)!=0,"{text}, {name}");}
 }
}

#[test]
fn reset_assignment_extension_preserves_both_edge_state_trajectories() {
 for edge in ["posedge","negedge"] {
 let text=format!("module m(input clk,reset,output reg [1:0] q,r); always @({edge} clk) if(reset) q<=2'b00; else q<=1'b1; assign r=2'b00; endmodule");
 let a=compile_systemverilog(&[SourceInput{name:"reset-context.sv",text:&text}],&CompileOptions::new("m")).unwrap();
 let mut evaluator=reda::frontend::evaluate::Evaluator::new(&a.netlist).unwrap();let(mut previous,mut q)=(false,0usize);
 for cycle in 0..32 {for clock in [false,true,true,false] {
 let reset=cycle%3==0;
 if (edge=="posedge"&&!previous&&clock)||(edge=="negedge"&&previous&&!clock){q=usize::from(!reset);}
 let values=evaluator.step(&[("clk".into(),clock),("reset".into(),reset)].into_iter().collect()).unwrap();
 for bit in 0..2 {assert_eq!(values[&a.output_map()[&format!("q[{bit}]")]],q&(1<<bit)!=0);assert!(!values[&a.output_map()[&format!("r[{bit}]")]]);}
 previous=clock;
 }}
 }
}

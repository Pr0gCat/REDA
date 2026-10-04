use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::frontend::evaluate::Evaluator;
#[test]
fn assignment_context_observation() {
 let cases=[
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

use reda::frontend::{compile_systemverilog,CompileOptions,SourceInput};
use reda::frontend::evaluate::Evaluator;
#[test]
fn inherited_comma_port_signedness_is_exhaustive() {
 let text="module m(input signed [3:0] a,b,output [7:0] y); assign y=a & b; endmodule";
 let artifact=compile_systemverilog(&[SourceInput{name:"inherited.sv",text}],&CompileOptions::new("m")).unwrap();
 let evaluator=Evaluator::new(&artifact.netlist).unwrap();
 for a in 0usize..16 {for b in 0usize..16 {
 let inputs=(0..4).flat_map(|bit|[(format!("a[{bit}]"),a&(1<<bit)!=0),(format!("b[{bit}]"),b&(1<<bit)!=0)]).collect();
 let values=evaluator.evaluate(&inputs).unwrap();let low=a&b;let expected=if low&8!=0{low|240}else{low};
 for bit in 0..8 {assert_eq!(values[&artifact.output_map()[&format!("y[{bit}]")]],expected&(1<<bit)!=0,"a={a} b={b} bit={bit}");}
 }}
}

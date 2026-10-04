fn main(){
let text="module shared(input a,b, output z,x, output [1:0] pair, output tail); assign z=a&b; assign x=a&b; assign pair={a,a}; assign tail=a&b; endmodule";
let a=reda::frontend::compile_systemverilog(&[reda::frontend::SourceInput{name:"shared.sv",text}],&reda::frontend::CompileOptions::new("shared")).unwrap();
println!("native baseline declared_output_bits={} netlist_output_occurrences={} gates={}",a.ports.iter().filter(|p|p.direction==reda::frontend::PortDirection::Output).count(),a.netlist.outputs.len(),a.netlist.gates.len());
assert_eq!(a.netlist.outputs.len(),5,"every declared source output bit must survive");
}

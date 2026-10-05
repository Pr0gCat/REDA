use reda::frontend::{compile_systemverilog_physical,CompileOptions,SourceInput};
fn main() {
 for text in ["module top(output [16:0] y); assign y=17'b0; endmodule", "module top(input a,b,output y,alias_y); assign y=a^b; assign alias_y=y; endmodule"] {
 let c=compile_systemverilog_physical(&[SourceInput{name:"probe.sv",text}],&CompileOptions::new("top")).unwrap();
 println!("{} outputs, {} positions",c.pinout.outputs.len(),c.pinout.outputs.values().collect::<std::collections::BTreeSet<_>>().len());
 }
}

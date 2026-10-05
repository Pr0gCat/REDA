use reda::frontend::{compile_systemverilog_physical,CompileOptions,SourceInput};
fn main() {
 for width in [0,1,2,3,7] {
  let inputs=if width==0 {String::new()} else {format!("input [{}:0] a,",width-1)};
  let value=if width==0 {"1'b0"} else {"a[0]"};
  let source=format!("module probe({inputs}output low,high,primary,inverse,alias_primary,alias_low,alias_high,alias_inverse); assign low=1'b0; assign high=1'b1; assign primary={value}; assign inverse=~{value}; assign alias_primary={value}; assign alias_low=1'b0; assign alias_high=1'b1; assign alias_inverse=~{value}; endmodule");
  let result=compile_systemverilog_physical(&[SourceInput{name:"probe.sv",text:&source}],&CompileOptions::new("probe"));
  match result {Ok(c)=>eprintln!("WIDTH {width} OK output_occurrences={} unique_public_positions={}",c.physical.output_occurrences().len(),c.pinout.outputs.values().collect::<std::collections::BTreeSet<_>>().len()),Err(e)=>eprintln!("WIDTH {width} ERROR {e}")};
 }
}

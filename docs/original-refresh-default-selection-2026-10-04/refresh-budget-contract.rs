use reda::compile::{selection::compile_selected_with_refresh_budget,compile_without_rewrite,lowering::lower_optimised,fragment_synth::benchmark::canonical_world_fingerprint};
use reda::frontend::{compile_systemverilog,CompileOptions,SourceInput};
fn main(){
let text="module m(input a,b,c,d,e,f, output u,z,o,v,w,x,y,q,r); assign u=a|b; assign z=1'b0; assign o=1'b1; assign v=c&d; assign w=c&d; assign x=c; assign y=e^f; assign q=~(e|f); assign r=f; endmodule";
let artifact=compile_systemverilog(&[SourceInput{name:"m.sv",text}],&CompileOptions::new("m")).unwrap();
let source=lower_optimised(&artifact.netlist).unwrap();
let raw=compile_without_rewrite(&source).unwrap();
let zero=compile_selected_with_refresh_budget(&source,0).unwrap();
assert_eq!(canonical_world_fingerprint(&raw.world),canonical_world_fingerprint(&zero.world));
let one=compile_selected_with_refresh_budget(&source,1).unwrap();
let again=compile_selected_with_refresh_budget(&source,1).unwrap();
assert_eq!(canonical_world_fingerprint(&one.world),canonical_world_fingerprint(&again.world));
assert_ne!(canonical_world_fingerprint(&one.world),canonical_world_fingerprint(&zero.world));
assert_eq!(one.output_occurrences().len(),9);
println!("budget0 preserved; budget1 deterministic, improves qualified waveform with 9 aliases-preserved public occurrences");
}

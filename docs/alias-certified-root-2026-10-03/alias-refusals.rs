use reda::compile::{Gate,Netlist,geometry::Anchor,planner::PortPlacements};
use reda::compile::fragment_synth::{compile_fragment_synth,SynthesisInput,SynthesisBudget,SynthesisError};
use reda::redstone::world::block::Facing;
fn main(){
 let pure=Netlist{inputs:vec!["a".into()],outputs:vec!["a".into()],gates:vec![]};
 assert!(matches!(compile_fragment_synth(SynthesisInput{lowered:&pure,source_provenance:None,pins:None},SynthesisBudget::Evaluations(0)),Err(SynthesisError::RecursiveContract(_))));
 let mixed=Netlist{inputs:vec!["a".into(),"b".into()],outputs:vec!["a".into(),"y".into(),"a".into()],gates:vec![Gate::nor("y",&["a","b"])]};
 let mut pins=PortPlacements::default();pins.pin("a",Anchor{x:1,y:1,z:4},Facing::South);
 assert!(matches!(compile_fragment_synth(SynthesisInput{lowered:&mixed,source_provenance:None,pins:Some(&pins)},SynthesisBudget::Evaluations(0)),Err(SynthesisError::RecursiveContract(_))));
 let unknown=Netlist{inputs:vec!["a".into()],outputs:vec!["missing".into()],gates:vec![]};
 assert!(matches!(compile_fragment_synth(SynthesisInput{lowered:&unknown,source_provenance:None,pins:None},SynthesisBudget::Evaluations(0)),Err(SynthesisError::InvalidNetlist(_))));
 let duplicate=Netlist{inputs:vec!["a".into()],outputs:vec!["a".into()],gates:vec![Gate::nor("a",&[])]};
 assert!(matches!(compile_fragment_synth(SynthesisInput{lowered:&duplicate,source_provenance:None,pins:None},SynthesisBudget::Evaluations(0)),Err(SynthesisError::InvalidNetlist(_))));
 println!("four trust-boundary refusal checks pass; refusals are not support");
}

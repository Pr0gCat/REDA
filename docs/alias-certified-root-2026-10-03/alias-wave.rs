use reda::compile::{Gate, Netlist};
use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisInput, SynthesisBudget};
use reda::formats::litematic::{load,save};
use reda::redstone::{simulator::{Simulator,position::Position},world::{storage::World,block::BlockKind}};
use serde_json::json;
use std::collections::{BTreeMap,BTreeSet,HashMap};
fn signal(world:&World,p:Position)->bool { let state=world.get(p.x,p.y,p.z); if state.kind==BlockKind::RedstoneWire {state.power>0} else {state.lit} }
fn main(){
 for depth in [0,32] { for repeat in 1..=3 {
  let mut gates=Vec::new(); let mut driver="a".to_string();
  for index in 0..depth {let name=format!("stage{index}");gates.push(Gate::nor(&name,&[driver.as_str()]));driver=name;}
  gates.push(Gate::nor("y",&[driver.as_str(),"b"]));
  let source=Netlist {inputs:vec!["a".into(),"b".into()],outputs:vec!["a".into(),"y".into(),"a".into()],gates};
  let result=compile_fragment_synth(SynthesisInput{lowered:&source,source_provenance:None,pins:None},SynthesisBudget::Evaluations(0)).expect("public alias boundary must certify");
  let rewrite=result.compiled.physical_rewrite().expect("actual source rewrite retained");assert_eq!(rewrite.source(),&source);
  let world=rewrite.physical().world();
  let artifact=format!("/tmp/reda-alias-certified-root-20261003/alias-depth{depth}-repeat{repeat}.litematic");
  save(std::path::Path::new(&artifact),world,"primary-alias").unwrap();let loaded=load(std::path::Path::new(&artifact)).unwrap();
  let fingerprint=reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
  assert_eq!(fingerprint(world),fingerprint(&loaded));
  let point=|&(x,y,z):&(i32,i32,i32)|Position::new(x,y,z);
  let inputs:Vec<_>=rewrite.source_input_positions().iter().map(point).collect();
  let drivers:Vec<_>=rewrite.source_output_driver_positions().iter().map(point).collect();
  let outputs:Vec<_>=rewrite.source_output_positions().iter().map(point).collect();assert_eq!(outputs.len(),3);
  assert_eq!(outputs.iter().map(|p|(p.x,p.y,p.z)).collect::<BTreeSet<_>>().len(),3);
  let implemented=rewrite.implemented();let views=rewrite.implemented_observations();
  let mut watched:BTreeMap<String,Position>=implemented.gates.iter().map(|g|(format!("gate:{}",g.output),point(&views.gate_output_positions[&g.output]))).collect();
  for (index,&p) in inputs.iter().enumerate(){watched.insert(format!("input:{index}"),p);}
  for (index,&p) in outputs.iter().enumerate(){watched.insert(format!("output:{index}"),p);}
  let positions:BTreeSet<_>=watched.values().map(|p|(p.x,p.y,p.z)).collect();
  for (flat,&palette) in world.cells().iter().enumerate(){let kind=world.palette().get(palette).unwrap().kind;
   if matches!(kind,BlockKind::Lever|BlockKind::WallTorch|BlockKind::Repeater|BlockKind::Lamp){assert!(positions.contains(&world.decode(flat)),"unobserved physical primitive");}}
  let mut sim=Simulator::new(loaded);sim.run_until_stable(2000).unwrap();
  let power_on:BTreeMap<_,_>=watched.iter().map(|(label,&p)|(label.clone(),signal(sim.world(),p))).collect();
  sim.attach_observer(watched.iter().map(|(label,&p)|(p,label.clone())));
  let mut phases=Vec::new();
  for from in 0..4 {for to in 0..4 {for (phase,address) in [from,to].into_iter().enumerate(){
   sim.reset_observer();let start=sim.current_tick();for (bit,p) in inputs.iter().enumerate(){let mut state=sim.world().get(p.x,p.y,p.z).clone();state.lit=address&(1<<bit)!=0;sim.world_mut().set(p.x,p.y,p.z,state);}
   let ticks=sim.run_until_stable(2000).unwrap();
   let internal:BTreeMap<_,_>=watched.iter().map(|(label,&p)|(label.clone(),signal(sim.world(),p))).collect();
   let mut values:HashMap<_,_>=implemented.inputs.iter().enumerate().map(|(bit,name)|(name.clone(),address&(1<<bit)!=0)).collect();
   for index in implemented.combinational_order().unwrap(){let gate=&implemented.gates[index];let value=gate.kind.evaluate(&gate.inputs.iter().map(|name|values[name]).collect::<Vec<_>>());values.insert(gate.output.clone(),value);assert_eq!(internal[&format!("gate:{}",gate.output)],value);}
   let a=address&1!=0;let b=address&2!=0;for (index,value) in [a,!(a||b),a].into_iter().enumerate(){assert_eq!(internal[&format!("output:{index}")],value);assert_eq!(signal(sim.world(),drivers[index]),value);}
   let events:Vec<_>=sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect();
   phases.push(json!({"from":from,"to":to,"phase":phase,"address":address,"ticks":ticks,"internal":internal,"drivers":drivers.iter().map(|&p|signal(sim.world(),p)).collect::<Vec<_>>(),"events":events}));
  }}}
  println!("{}",json!({"depth":depth,"repeat":repeat,"artifact":artifact,"world_fingerprint":fingerprint(world).as_str(),"outputs":outputs.iter().map(|p|[p.x,p.y,p.z]).collect::<Vec<_>>(),"source_gates":source.gates.iter().map(|g|json!({"inputs":g.inputs,"output":g.output,"kind":g.kind.wire_name()})).collect::<Vec<_>>(),"implemented_inputs":implemented.inputs,"implemented_outputs":implemented.outputs,"implemented_gates":implemented.gates.iter().map(|g|json!({"inputs":g.inputs,"output":g.output,"kind":g.kind.wire_name()})).collect::<Vec<_>>(),"power_on":power_on,"phases":phases}));
 }}
}

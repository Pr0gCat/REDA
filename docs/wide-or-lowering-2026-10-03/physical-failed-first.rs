use reda::compile::{compile,Gate,Netlist,lowering::lower_optimised};
use reda::compile::topology::GateKind;
use reda::formats::litematic::{save,load};
use reda::redstone::{simulator::{Simulator,position::Position},world::{storage::World,block::BlockKind}};
use std::collections::BTreeMap;
use serde_json::json;
fn signal(world:&World,p:(i32,i32,i32))->bool {let b=world.get(p.0,p.1,p.2);if b.kind==BlockKind::RedstoneWire {b.power>0}else{b.lit}}
fn main() {
 let args:Vec<_>=std::env::args().collect();let count:usize=args[1].parse().unwrap();let repeat:usize=args[2].parse().unwrap();let root=std::path::Path::new(&args[3]);
 let inputs:Vec<_>=(0..count).map(|i|format!("i{i}")).collect();
 let source=Netlist{inputs:inputs.clone(),outputs:vec!["merged".into()],gates:vec![Gate{name:"wide-cell".into(),inputs:inputs.clone(),output:"merged".into(),kind:GateKind::Or(count)}]};
 let lowered=lower_optimised(&source).expect("wide source lowers");
 let circuit=compile(&lowered).expect("actual default compiler accepts bounded tree");
 let fingerprint=reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&circuit.world);
 let path=root.join(format!("wide-or-{count}-repeat{repeat}.litematic"));save(&path,&circuit.world,"wide-or").unwrap();
 let mut sim=Simulator::new(load(&path).unwrap());sim.run_until_stable(2000).unwrap();
 let mut sites:BTreeMap<String,(i32,i32,i32)>=lowered.gates.iter().map(|g|(format!("gate:{}",g.output),circuit.gate_output_positions[&g.output])).collect();
 sites.insert("lamp:merged".into(),circuit.output_positions["merged"]);
 let initial:BTreeMap<_,_>=sites.iter().map(|(s,p)|(s.clone(),signal(sim.world(),*p))).collect();
 sim.attach_observer(sites.iter().map(|(s,p)|(Position::new(p.0,p.1,p.2),s.clone())).collect());
 let mut phases=Vec::new();let max=(1usize<<count)-1;
 let transitions:Vec<_>=if count<=5 {(0..=max).flat_map(|from|(0..=max).map(move|to|(from,to))).collect()}else{(0..=max).flat_map(|from|(0..count).map(move|bit|(from,from^(1<<bit))).chain(std::iter::once((from,from^max)))).collect()};
 for (from,to) in transitions {for bits in [from,to] {
  sim.reset_observer();let start=sim.current_tick();
  let mut values:BTreeMap<String,bool>=inputs.iter().enumerate().map(|(i,s)|(s.clone(),bits&(1<<i)!=0)).collect();
  for name in &inputs {let p=circuit.input_positions[name];let mut b=sim.world().get(p.0,p.1,p.2).clone();b.lit=values[name];sim.world_mut().set(p.0,p.1,p.2,b);}
  sim.run_until_stable(2000).expect("unchanged settling cap");
  let mut expected=BTreeMap::new();
  for index in lowered.combinational_order().unwrap() {let g=&lowered.gates[index];let value=g.kind.evaluate(&g.inputs.iter().map(|s|values[s]).collect::<Vec<_>>());values.insert(g.output.clone(),value);expected.insert(format!("gate:{}",g.output),value);}
  expected.insert("lamp:merged".into(),bits!=0);assert_eq!(values["merged"],bits!=0);
  let actual:BTreeMap<_,_>=sites.iter().map(|(s,p)|(s.clone(),signal(sim.world(),*p))).collect();assert_eq!(actual,expected,"count={count} bits={bits}");
  phases.push(json!({"bits":bits,"ticks":sim.current_tick()-start,"actual":actual,"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>() }));
 }}
 println!("{}",json!({"count":count,"repeat":repeat,"world_fingerprint":fingerprint,"artifact":path,"inputs":inputs,"gates":lowered.gates.iter().map(|g|json!({"output":g.output,"inputs":g.inputs,"kind":g.kind.wire_name()})).collect::<Vec<_>>(),"power_on":initial,"phases":phases}));
}

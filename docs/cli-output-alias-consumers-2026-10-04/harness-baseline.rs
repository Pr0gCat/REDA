use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}, process::Command};
use reda::{compile::{compile, lowering::lower_optimised}, formats::litematic, frontend::{compile_systemverilog, CompileOptions, SourceInput}, redstone::{simulator::Simulator, world::block::BlockKind}};
use serde::{de::{MapAccess, Visitor}, Deserialize, Deserializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

const REWRITE: &str = "module m(input [1:0] a, output [1:0] y, output z);\nassign y[0]=~a[1]; assign y[1]=~a[0]; assign z=~a[1];\nendmodule\n";
const RAW: &str = "module m(input [5:0] a, output [7:0] y, output z);\nassign y[0]=a[5]|a[4]; assign y[1]=1'b0; assign y[2]=1'b1;\nassign y[3]=a[3]&a[2]; assign y[4]=a[3]&a[2]; assign y[5]=a[3];\nassign y[6]=a[1]^a[0]; assign y[7]=~(a[1]|a[0]); assign z=a[0];\nendmodule\n";

type At=(i32,i32,i32);
struct UniqueMap(BTreeMap<String, Value>);
impl<'de> Deserialize<'de> for UniqueMap {
    fn deserialize<D: Deserializer<'de>>(d:D)->Result<Self,D::Error> {
        struct MapVisitor;
        impl<'de> Visitor<'de> for MapVisitor {
            type Value=UniqueMap;
            fn expecting(&self,f:&mut std::fmt::Formatter)->std::fmt::Result {f.write_str("a JSON map with unique keys")}
            fn visit_map<M:MapAccess<'de>>(self,mut map:M)->Result<Self::Value,M::Error> {
                let mut values=BTreeMap::new();
                while let Some((key,value))=map.next_entry::<String,Value>()? {
                    if values.insert(key.clone(),value).is_some() {return Err(serde::de::Error::custom(format!("duplicate pinout key {key}")));}
                }
                Ok(UniqueMap(values))
            }
        }
        d.deserialize_map(MapVisitor)
    }
}
#[derive(Deserialize)]
struct Pinout {inputs:UniqueMap,outputs:UniqueMap}
fn at(value:&Value)->At {let a=value.as_array().unwrap();assert_eq!(a.len(),3);(a[0].as_i64().unwrap() as i32,a[1].as_i64().unwrap() as i32,a[2].as_i64().unwrap() as i32)}
fn run(bin:&str,args:&[&str],cwd:&Path)->String {
    let output=Command::new(bin).args(args).current_dir(cwd).env("PATH","/nonexistent-python-path").output().unwrap();
    assert!(output.status.success(),"{} {:?}: {}",bin,args,String::from_utf8_lossy(&output.stderr));
    fs::write(cwd.join(format!("{}.stderr",Path::new(bin).file_name().unwrap().to_str().unwrap())),&output.stderr).unwrap();
    String::from_utf8(output.stdout).unwrap()
}
fn expected(raw:bool,label:&str,input:u32)->bool {
    let bit=|n| input & (1u32<<n)!=0u32;
    if !raw {return match label {"y[0]"|"z"=>!bit(1),"y[1]"=>!bit(0),_=>panic!("unknown output {label}")};}
    match label {"y[0]"=>bit(5)||bit(4),"y[1]"=>false,"y[2]"=>true,"y[3]"|"y[4]"=>bit(3)&&bit(2),"y[5]"=>bit(3),"y[6]"=>bit(1)^bit(0),"y[7]"=>!(bit(1)||bit(0)),"z"=>bit(0),_=>panic!("unknown output {label}")}
}

#[test]
fn actual_native_and_auto_cli_preserve_aliases_bus_order_and_reloaded_truth() {
    let root=PathBuf::from(std::env::var("REDA_CONSUMER_ARTIFACT_DIR").unwrap_or_else(|_| "/tmp/reda-cli-alias-consumers-20261004".into()));
    let mut reports=Vec::new();
    for (name,text,bits,raw) in [("rewrite",REWRITE,2,false),("raw",RAW,6,true)] {
        let artifact=compile_systemverilog(&[SourceInput{name:"source.sv",text}],&CompileOptions::new("m")).unwrap();
        let source=lower_optimised(&artifact.netlist).unwrap();
        let labels:Vec<_>=artifact.ordered_output_ports.iter().map(|port|port.name.clone()).collect();
        let reference=compile(&source).unwrap();
        assert_eq!(reference.physical_rewrite().is_none(),raw,"fixture must exercise its promised owner");
        let occurrences=reference.output_occurrences();
        let alias_indices=if raw {(3,4)} else {(0,2)};
        assert_eq!(occurrences[alias_indices.0].0,occurrences[alias_indices.1].0);
        assert_eq!(occurrences[alias_indices.0].1==occurrences[alias_indices.1].1,raw);
        let mut modes=Vec::new();
        for mode in ["native","auto"] {
            let directory=root.join(name).join(mode);fs::create_dir_all(&directory).unwrap();
            let path=directory.join("source.sv");fs::write(&path,text).unwrap();
            let stdout=run(env!("CARGO_BIN_EXE_build_circuit"),&["verilog",path.to_str().unwrap(),"m","--frontend",mode],&directory);
            fs::write(directory.join("build.stdout"),&stdout).unwrap();
            let dump=run(env!("CARGO_BIN_EXE_mc_dump"),&["verilog",path.to_str().unwrap(),"m","--frontend",mode],&directory);
            fs::write(directory.join("circuit.dump"),&dump).unwrap();
            let pinout_text=fs::read_to_string(directory.join("output/m.pinout.json")).unwrap();
            let pinout:Pinout=serde_json::from_str(&pinout_text).unwrap();
            assert_eq!(pinout.outputs.0.len(),labels.len());assert_eq!(pinout.inputs.0.len(),bits);
            let expected_json_keys:BTreeSet<_>=source.outputs.iter().enumerate().map(|(i,signal)|if source.outputs.iter().filter(|s|*s==signal).count()>1 {labels[i].clone()} else {signal.clone()}).collect();
            assert_eq!(pinout.outputs.0.keys().cloned().collect::<BTreeSet<_>>(),expected_json_keys);
            let output_lines:Vec<_>=stdout.split("  outputs (lamp, unless pinned):\n").nth(1).unwrap().lines().filter(|line|line.starts_with("    ")).collect();
            assert_eq!(output_lines.len(),labels.len());
            let ports:Vec<_>=dump.lines().filter(|line|line.starts_with("OUTPUTPORT ")).collect();assert_eq!(ports.len(),labels.len());
            for (ordinal,((signal,position),label)) in occurrences.iter().zip(&labels).enumerate() {
                let fields:Vec<_>=ports[ordinal].split_whitespace().collect();assert_eq!(fields.len(),8);
                assert_eq!(fields[1].parse::<usize>().unwrap(),ordinal);assert_eq!(fields[2],signal);assert_eq!(fields[7],label);
                let parsed=(fields[4].parse().unwrap(),fields[5].parse().unwrap(),fields[6].parse().unwrap());assert_eq!(*position,parsed);
                assert_eq!(output_lines[ordinal].trim().split_whitespace().next().unwrap(),label);
                let key=if source.outputs.iter().filter(|s|*s==signal).count()>1 {label} else {signal};assert_eq!(at(&pinout.outputs.0[key]),*position);
            }
            let loaded=litematic::load(&directory.join("output/m.litematic")).unwrap();assert_eq!(loaded.size(),reference.world.size());
            for (flat,&index) in reference.world.cells().iter().enumerate() {let (x,y,z)=reference.world.decode(flat);assert_eq!(reference.world.palette().get(index),Some(loaded.get(x,y,z)),"Litematic changed cell ({x},{y},{z})");}
            let mut sim=Simulator::new(loaded);let mut phases=0;
            for from in 0..(1<<bits) {for to in 0..(1<<bits) {for input in [from,to] {
                for (label,value) in &pinout.inputs.0 {let bit:usize=label.strip_prefix("a[").unwrap().strip_suffix(']').unwrap().parse().unwrap();let (x,y,z)=at(value);let mut state=sim.world().get(x,y,z).clone();assert_eq!(state.kind,BlockKind::Lever);state.lit=input&(1<<bit)!=0;sim.world_mut().set(x,y,z,state);}
                sim.run_until_stable(2000).unwrap();
                for ((_,(x,y,z)),label) in occurrences.iter().zip(&labels) {assert_eq!(sim.world().get(*x,*y,*z).kind,BlockKind::Lamp);assert_eq!(sim.world().get(*x,*y,*z).lit,expected(raw,label,input),"{name}/{mode} {from}->{to} value {input} output {label}");}
                phases+=1;
            }}}
            modes.push(serde_json::json!({"mode":mode,"phases":phases,"ports":ports.len(),"unique_json_keys":pinout.outputs.0.len(),"world_cells_identical":true,"truth":"all retained setup-target phases pass","python_path":"unavailable"}));
        }
        reports.push(serde_json::json!({"fixture":name,"source_sha256":format!("{:x}",Sha256::digest(text.as_bytes())),"inputs":bits,"outputs":labels,"alias_indices":alias_indices,"shared_physical_lamp":raw,"modes":modes}));
    }
    fs::write(root.join("report.json"),serde_json::to_string_pretty(&reports).unwrap()).unwrap();
}

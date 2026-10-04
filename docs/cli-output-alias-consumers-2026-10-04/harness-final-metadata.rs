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

#[derive(Deserialize)]
struct LabelledPinout { inputs:UniqueMap, outputs:UniqueMap, output_labels:UniqueMap }
#[test]
fn actual_final_public_label_mapping_and_collision_keep_frozen_physics() {
 let root=PathBuf::from("/tmp/reda-cli-alias-consumers-20261004");
 let collision="module m(input [1:0] a, output g1,other,alias);\nassign g1=~a[1]; assign other=~a[0]; assign alias=~a[1];\nendmodule\n";
 let mut reports=Vec::new();
 for (name,text,baseline) in [("rewrite",REWRITE,"rewrite"),("raw",RAW,"raw"),("collision",collision,"rewrite")] {
  let artifact=compile_systemverilog(&[SourceInput{name:"source.sv",text}],&CompileOptions::new("m")).unwrap();
  let labels:Vec<_>=artifact.ordered_output_ports.iter().map(|port|port.name.clone()).collect();
  for mode in ["native","auto"] {
   let old=root.join(baseline).join(mode);let cwd=root.join("final-metadata").join(name).join(mode);fs::create_dir_all(&cwd).unwrap();
   let path=cwd.join("source.sv");fs::write(&path,text).unwrap();
   let stdout=run(env!("CARGO_BIN_EXE_build_circuit"),&["verilog",path.to_str().unwrap(),"m","--frontend",mode],&cwd);fs::write(cwd.join("build.stdout"),&stdout).unwrap();
   let dump=run(env!("CARGO_BIN_EXE_mc_dump"),&["verilog",path.to_str().unwrap(),"m","--frontend",mode],&cwd);fs::write(cwd.join("circuit.dump"),&dump).unwrap();
   let pinout:LabelledPinout=serde_json::from_str(&fs::read_to_string(cwd.join("output/m.pinout.json")).unwrap()).unwrap();
   let original:Pinout=serde_json::from_str(&fs::read_to_string(old.join("output/m.pinout.json")).unwrap()).unwrap();assert_eq!(pinout.inputs.0,original.inputs.0);
   assert_eq!(pinout.outputs.0.keys().cloned().collect::<BTreeSet<_>>(),labels.iter().cloned().collect());
   assert_eq!(pinout.output_labels.0.keys().cloned().collect::<BTreeSet<_>>(),labels.iter().cloned().collect());
   let original_dump=fs::read_to_string(old.join("circuit.dump")).unwrap();
   let previous:Vec<_>=original_dump.lines().filter(|s|s.starts_with("OUTPUTPORT ")).collect();let ports:Vec<_>=dump.lines().filter(|s|s.starts_with("OUTPUTPORT ")).collect();assert_eq!(ports.len(),labels.len());
   let signal_names:BTreeSet<_>=dump.lines().filter(|line|line.starts_with("INPUT ")||line.starts_with("GATEOUT ")).map(|line|line.split_whitespace().nth(1).unwrap()).collect();
   for (ordinal,label) in labels.iter().enumerate() {
    let fields:Vec<_>=ports[ordinal].split_whitespace().collect();let old_fields:Vec<_>=previous[ordinal].split_whitespace().collect();assert_eq!(fields.len(),8);assert_eq!(fields[1].parse::<usize>().unwrap(),ordinal);assert_eq!(fields[7],label);assert!(signal_names.contains(fields[3]));
    assert_eq!(&fields[..7],&old_fields[..7],"renaming public labels changed actual implementation binding");
    let position=(fields[4].parse().unwrap(),fields[5].parse().unwrap(),fields[6].parse().unwrap());
    let key=pinout.output_labels.0[label].as_str().unwrap();assert_eq!(key,label);assert_eq!(at(&pinout.outputs.0[key]),position);
    let output_lines:Vec<_>=stdout.split("  outputs (lamp, unless pinned):\n").nth(1).unwrap().lines().filter(|line|line.starts_with("    ")).collect();assert_eq!(output_lines.len(),labels.len());assert_eq!(output_lines[ordinal].trim().split_whitespace().next().unwrap(),label);
   }
   let physical_records=|s:&str|s.lines().filter(|line|line.starts_with("BLOCK ")||line.starts_with("BLOCKPROP ")||line.starts_with("SIZE ")||line.starts_with("INPUT ")||line.starts_with("GATEOUT ")||line.starts_with("GATE ")).map(str::to_string).collect::<Vec<_>>();assert_eq!(physical_records(&dump),physical_records(&original_dump));
   let frozen=litematic::load(&old.join("output/m.litematic")).unwrap();let loaded=litematic::load(&cwd.join("output/m.litematic")).unwrap();assert_eq!(frozen.size(),loaded.size());
   for (flat,&index) in frozen.cells().iter().enumerate(){let(x,y,z)=frozen.decode(flat);assert_eq!(frozen.palette().get(index),Some(loaded.get(x,y,z)));}
   reports.push(serde_json::json!({"fixture":name,"mode":mode,"source_sha256":format!("{:x}",Sha256::digest(text.as_bytes())),"public_labels":labels,"output_keys_unique":true,"label_mapping_complete":true,"implemented_dump_bindings_exist":true,"frozen_world_identical":true,"wave_reused_from":format!("{baseline}/{mode}"),"logical_rename":if name=="collision" {serde_json::json!({"y[0]":"g1","y[1]":"other","z":"alias"})} else {Value::Null}}));
  }
 }
 fs::write(root.join("final-metadata-report.json"),serde_json::to_string_pretty(&reports).unwrap()).unwrap();
}

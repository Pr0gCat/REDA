use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::frontend::evaluate::Evaluator;
use std::collections::BTreeMap;
fn main() {
    let source=include_str!("../evidence/fixture.sv");
    let artifact=compile_systemverilog(&[SourceInput{name:"fixture.sv",text:source}],&CompileOptions::new("top")).unwrap();
    let mut evaluator=Evaluator::new(&artifact.netlist).unwrap();
    let mut rows=0;
    for line in include_str!("../evidence/oracle.csv").lines() {
        let fields=line.split(',').collect::<Vec<_>>();
        if fields.len()!=11 {continue;}
        let row:usize=fields[0].parse().unwrap();
        let mut inputs=BTreeMap::new();
        for bit in 0..4 {
            for (port,shift) in [("a",0),("b",4)] {
                inputs.insert(artifact.port(&format!("{port}[{bit}]")).unwrap().signal.clone(), row&(1<<(shift+bit))!=0);
            }
        }
        let values=evaluator.step(&inputs).unwrap();
        for output in 0..10 {
            let actual=(0..8).fold(0u32,|word,bit| word|((values[&artifact.port(&format!("y{output}[{bit}]")).unwrap().signal] as u32)<<bit));
            let expected:u32=fields[output+1].parse().unwrap();
            assert_eq!(actual,expected,"row={row},output=y{output}");
        }
        rows+=1;
    }
    assert_eq!(rows,256);
    println!("standard HDL assignment controls: 256 rows, 20480 public bits matched");
}

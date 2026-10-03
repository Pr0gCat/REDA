//! Measure the actual default compiled circuit against an immutable family.
use reda::compile::{
    lowering::lower_optimised,
    selection::{qualify_with_phases, METRICS},
};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{BufRead, BufReader},
};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let control = args.get(1).expect("frozen family jsonl.gz");
    let start = args.get(2).map_or(0, |s| s.parse::<usize>().unwrap());
    let end = args.get(3).map_or(4096, |s| s.parse::<usize>().unwrap());
    let rows: Vec<Value> =
        BufReader::new(flate2::read::GzDecoder::new(File::open(control).unwrap()))
            .lines()
            .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
            .collect();
    assert_eq!(rows.len(), 4096);
    assert!(start < end && end <= 4096);
    let mut failed = false;
    for encoding in start..end {
        let fixed = &rows[encoding];
        assert_eq!(fixed["encoding"], encoding);
        let words: [u8; 4] = std::array::from_fn(|a| ((encoding >> (3 * a)) & 7) as u8);
        assert_eq!(fixed["words"], json!(words));
        let mut text = String::from("module source_rom(input logic a,b,output logic y0,y1,y2);\n");
        for bit in 0..3 {
            let values = words.map(|w| (w >> bit) & 1);
            text.push_str(&format!(
                "assign y{bit} = b ? (a ? 1'b{} : 1'b{}) : (a ? 1'b{} : 1'b{});\n",
                values[3], values[2], values[1], values[0]
            ));
        }
        text.push_str("endmodule");
        let artifact = compile_systemverilog(
            &[SourceInput {
                name: "source_rom.sv",
                text: &text,
            }],
            &CompileOptions::new("source_rom"),
        )
        .unwrap();
        let ports = artifact.output_map();
        let mut source = lower_optimised(&artifact.netlist).unwrap();
        source.outputs = (0..3)
            .map(|bit| ports[&format!("y{bit}")].clone())
            .collect();
        let candidate = match reda::compile::compile(&source) {
            Ok(compiled) => {
                let rewrite = compiled
                    .physical_rewrite()
                    .expect("selected circuit is certified");
                let graph = reda::compile::primitive_graph::from_certified_rewrite(rewrite);
                reda::compile::equivalence::verify_expansion_matches_compiled(
                    &source, &graph, &compiled,
                )
                .unwrap();
                let partition =
                    reda::compile::world_partition::partition_world(&source, &graph, &compiled)
                        .unwrap();
                assert!(partition.unexplained.is_empty());
                let mut phases = Vec::new();
                let quality=qualify_with_phases(&source,&compiled,|p| {
                    phases.push(json!({"from":p.from,"to":p.to,"phase":p.phase,"address":p.address,"ticks":p.ticks,"expected":p.expected,"driver":p.driver,"lamp":p.lamp,"events":p.events.iter().map(|e| {
                        let index=e.label.strip_prefix("boundary").unwrap().parse::<usize>().unwrap();
                        json!({"label":format!("{}{}",if index<3 {"driver"} else {"lamp"},index%3),"tick":e.tick-p.start,"value":e.value})
                    }).collect::<Vec<_>>() }));
                }).unwrap();
                let metrics = |q: [u64; 12]| {
                    METRICS
                        .iter()
                        .zip(q)
                        .map(|(key, value)| (key.to_string(), json!(value)))
                        .collect::<serde_json::Map<_, _>>()
                };
                let exact_cells: Vec<_> = compiled
                    .world
                    .cells()
                    .iter()
                    .enumerate()
                    .filter_map(|(flat, &index)| {
                        let block = compiled.world.palette().get(index).unwrap();
                        (block.kind != reda::redstone::world::block::BlockKind::Air)
                            .then(|| (compiled.world.decode(flat), block))
                    })
                    .collect();
                let exact = reda::compile::metrics::canonical_fingerprint(
                    &serde_json::to_vec(&(compiled.world.size(), exact_cells)).unwrap(),
                );
                json!({"correct":true,"certified":true,"fingerprint":reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&compiled.world),"exact_world_fingerprint":exact,"choice":if rewrite.implemented()==&source {"backup"} else {"player"},"quality":{"setup":metrics(quality.setup),"target":metrics(quality.target)},"phases":phases})
            }
            Err(error) => {
                failed = true;
                json!({"correct":false,"certified":false,"generation_error":format!("{error:?}"),"phases":[]})
            }
        };
        println!(
            "{}",
            json!({"encoding":encoding,"words":words,"control":fixed["control"],"candidate":candidate})
        );
    }
    if failed {
        std::process::exit(1);
    }
}

//! Compiles the seven-segment decoder and writes it to a `.litematic` file
//! that can be pasted straight into Minecraft via Litematica.
//!
//! Run with `cargo run --bin build_seven_segment_decoder` from the repo root.
//! The output path is relative to the current working directory, so running
//! it from anywhere else writes `output/` there instead.

use std::path::Path;

use reda::circuits::seven_segment::build_seven_segment_netlist;
use reda::compile::compile;
use reda::formats::litematic;
use reda::redstone::world::block::BlockKind;

fn main() {
    let (netlist, _segment_signal) = build_seven_segment_netlist();
    let gate_count = netlist.gates.len();

    let compiled = compile(&netlist).expect("the seven-segment netlist is acyclic and fully driven");
    let (size_x, size_y, size_z) = compiled.world.size();

    let mut non_air_blocks = 0usize;
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if compiled.world.get(x, y, z).kind != BlockKind::Air {
                    non_air_blocks += 1;
                }
            }
        }
    }

    let output_dir = Path::new("output");
    std::fs::create_dir_all(output_dir).expect("failed to create the output directory");
    let output_path = output_dir.join("seven_segment.litematic");

    litematic::save(&output_path, &compiled.world, "seven_segment_decoder")
        .expect("failed to write the litematic file");

    println!("bounding box: {size_x} x {size_y} x {size_z}");
    println!("non-air blocks: {non_air_blocks}");
    println!("gate count: {gate_count}");
    println!("wrote {}", output_path.display());
}

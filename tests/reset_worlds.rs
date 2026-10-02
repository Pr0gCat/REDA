//! Repeated complete emitted worlds, including power-on state, not block counts.
use reda::compile::{compile, lowering::lower_optimised, metrics::canonical_fingerprint};
use reda::formats::litematic::{load, save};
use reda::frontend::{compile_systemverilog, synthesize_verilog, CompileOptions, SourceInput};
use reda::redstone::world::{block::BlockKind, storage::World};
use serde_json::json;

fn exact(world: &World) -> reda::compile::metrics::Fingerprint {
    let cells: Vec<_> = world
        .cells()
        .iter()
        .enumerate()
        .filter_map(|(flat, &index)| {
            let block = world.palette().get(index).unwrap();
            (block.kind != BlockKind::Air).then(|| (world.decode(flat), block))
        })
        .collect();
    canonical_fingerprint(&serde_json::to_vec(&(world.size(), cells)).unwrap())
}

#[test]
fn reset_emitted_and_loaded_worlds_repeat_exactly() {
    for (top, source, falling) in [
        (
            "reset_counter",
            include_str!("fixtures/reset_counter.sv"),
            false,
        ),
        (
            "hierarchy_reset_counter",
            include_str!("fixtures/hierarchy_reset_counter.sv"),
            false,
        ),
        ("reset_fsm", include_str!("fixtures/reset_fsm.sv"), false),
        (
            "conditional_state_enable",
            include_str!("fixtures/conditional_state_enable.sv"),
            false,
        ),
        (
            "reset_counter",
            include_str!("fixtures/reset_counter.sv"),
            true,
        ),
        (
            "hierarchy_reset_counter",
            include_str!("fixtures/hierarchy_reset_counter.sv"),
            true,
        ),
    ] {
        let text = if falling {
            source.replace("posedge", "negedge")
        } else {
            source.to_owned()
        };
        for yosys in [false, true] {
            let case = format!(
                "{}-{}{top}",
                if yosys { "yosys" } else { "native" },
                if falling { "negedge_" } else { "" }
            );
            let mut baseline = None;
            for run in 0..3 {
                let netlist = if yosys {
                    synthesize_verilog(&text, top).unwrap().0
                } else {
                    compile_systemverilog(
                        &[SourceInput {
                            name: top,
                            text: &text,
                        }],
                        &CompileOptions::new(top),
                    )
                    .unwrap()
                    .netlist
                };
                let physical = compile(&lower_optimised(&netlist).unwrap()).unwrap();
                let fingerprint = exact(&physical.world);
                if let Some(previous) = &baseline {
                    assert_eq!(&fingerprint, previous, "{case}, run={run}");
                }
                baseline = Some(fingerprint.clone());
                let path = std::env::temp_dir().join(format!(
                    "reda-reset-world-{}-{case}.litematic",
                    std::process::id()
                ));
                save(&path, &physical.world, &case).unwrap();
                let loaded = load(&path).unwrap();
                std::fs::remove_file(path).unwrap();
                assert_eq!(exact(&loaded), fingerprint, "{case}, loaded run={run}");
                println!(
                    "RESET_WORLD {}",
                    json!({"case":case,"run":run,"exact_world_fingerprint":fingerprint,"loaded_equal":true})
                );
            }
        }
    }
}

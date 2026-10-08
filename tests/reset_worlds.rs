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
        ("reset_counter", include_str!("fixtures/reset_counter.sv"), false),
        ("hierarchy_reset_counter", include_str!("fixtures/hierarchy_reset_counter.sv"), false),
        ("reset_fsm", include_str!("fixtures/reset_fsm.sv"), false),
        ("conditional_state_enable", include_str!("fixtures/conditional_state_enable.sv"), false),
        ("reset_counter", include_str!("fixtures/reset_counter.sv"), true),
        ("hierarchy_reset_counter", include_str!("fixtures/hierarchy_reset_counter.sv"), true),
    ] {
        for yosys in [false, true] {
            check_worlds(top, source, falling, yosys);
        }
    }
}

// Native-only world controls keep the original full Native/Yosys suite intact:
// the native frontend path is verified independently of Yosys, which is absent
// in this environment. Each fixture is its own test so the libtest harness
// parallelizes the slow per-fixture compilation across cores.
#[test]
fn native_reset_counter_worlds_repeat_exactly() {
    check_worlds("reset_counter", include_str!("fixtures/reset_counter.sv"), false, false);
}
#[test]
fn native_hierarchy_reset_counter_worlds_repeat_exactly() {
    check_worlds("hierarchy_reset_counter", include_str!("fixtures/hierarchy_reset_counter.sv"), false, false);
}
#[test]
fn native_reset_fsm_worlds_repeat_exactly() {
    check_worlds("reset_fsm", include_str!("fixtures/reset_fsm.sv"), false, false);
}
#[test]
fn native_conditional_state_enable_worlds_repeat_exactly() {
    check_worlds("conditional_state_enable", include_str!("fixtures/conditional_state_enable.sv"), false, false);
}
#[test]
fn native_negedge_reset_counter_worlds_repeat_exactly() {
    check_worlds("reset_counter", include_str!("fixtures/reset_counter.sv"), true, false);
}
#[test]
fn native_negedge_hierarchy_reset_counter_worlds_repeat_exactly() {
    check_worlds("hierarchy_reset_counter", include_str!("fixtures/hierarchy_reset_counter.sv"), true, false);
}

fn check_worlds(top: &str, source: &str, falling: bool, yosys: bool) {
    let text = if falling {
        source.replace("posedge", "negedge")
    } else {
        source.to_owned()
    };
    let case = format!(
        "{}-{}{top}",
        if yosys { "yosys" } else { "native" },
        if falling { "negedge_" } else { "" }
    );
    // Two identical runs prove compile determinism (run 0 baseline, run 1
    // equality) and emitted-vs-loaded equality; a third run repeats run 1.
    let mut baseline = None;
    for run in 0..2 {
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
            "reda-reset-world-{}-{:?}-{case}.litematic",
            std::process::id(),
            std::thread::current().id()
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

#[test]
fn arithmetic_emitted_and_loaded_worlds_repeat_exactly() {
    for (top, source) in [
        ("arithmetic_counter_add", include_str!("fixtures/arithmetic_counter_add.sv")),
        ("arithmetic_counter_subtract", include_str!("fixtures/arithmetic_counter_subtract.sv")),
    ] {
        for falling in [false, true] {
            for yosys in [false, true] {
                check_worlds(top, source, falling, yosys);
            }
        }
    }
}

#[test]
fn native_arithmetic_add_posedge_worlds_repeat_exactly() {
    check_worlds("arithmetic_counter_add", include_str!("fixtures/arithmetic_counter_add.sv"), false, false);
}
#[test]
fn native_arithmetic_add_negedge_worlds_repeat_exactly() {
    check_worlds("arithmetic_counter_add", include_str!("fixtures/arithmetic_counter_add.sv"), true, false);
}
#[test]
fn native_arithmetic_subtract_posedge_worlds_repeat_exactly() {
    check_worlds("arithmetic_counter_subtract", include_str!("fixtures/arithmetic_counter_subtract.sv"), false, false);
}
#[test]
fn native_arithmetic_subtract_negedge_worlds_repeat_exactly() {
    check_worlds("arithmetic_counter_subtract", include_str!("fixtures/arithmetic_counter_subtract.sv"), true, false);
}

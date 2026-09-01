//! Black-box tests for `build_circuit --pins`: the IO-terminals contract
//! (docs/superpowers/specs/2026-08-30-io-terminals.md) exercised through the
//! public command line, exactly as a caller with a pins file would drive it.
//!
//! Plus the one claim the command line cannot make about itself: that the pins
//! file is an *adapter* over `PortPlacements` and not the place the rules live.
//! That is stated here, from outside the binary, against the same public entry
//! an editor or an in-game mod would call.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use reda::circuits::and4::build_and4_netlist;
use reda::compile::fragment_synth::benchmark::legacy_benchmark_evaluator;
use reda::compile::planner::{Anchor, PinRefusal, PortPin, PortPlacements, PortRole};
use reda::compile::{
    compile_fragment_synth, compile_grown, CompileError, SynthesisBudget, SynthesisInput,
};
use reda::redstone::world::block::{BlockKind, BlockState, Facing};

/// One test's own scratch directory, created empty-or-reused. The binary is
/// run *in* it because the `output/` tree it writes is relative to the
/// working directory.
fn scratch_dir(test: &str) -> PathBuf {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    std::fs::create_dir_all(&scratch).expect("the scratch directory is creatable");
    scratch
}

/// Run the binary with `args` in `test`'s scratch directory, returning the
/// process output plus that directory for reading the sidecars back.
fn run_in_scratch(test: &str, args: &[&str]) -> (Output, PathBuf) {
    let scratch = scratch_dir(test);
    let output = Command::new(env!("CARGO_BIN_EXE_build_circuit"))
        .args(args)
        .current_dir(&scratch)
        .output()
        .expect("build_circuit must run");
    (output, scratch)
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Every way a pins file can be wrong exits before any compile, with a
/// message that names the defect -- the file is hand-written, and a --grown
/// run costs minutes on the circuits pins exist for.
#[test]
fn pins_file_defects_exit_by_name_before_any_compile() {
    // A file that is not there is named by its path.
    let (output, _) = run_in_scratch(
        "pins-missing-file",
        &["and4", "--grown", "--pins", "no-such-pins.json"],
    );
    assert!(
        !output.status.success(),
        "a missing pins file must not compile anything"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("could not read the pins file") && stderr.contains("no-such-pins.json"),
        "the refusal names the file: {stderr}"
    );

    // A malformed pin is named through the CLI surface, port and defect both.
    let scratch = scratch_dir("pins-bad-facing");
    let bad_facing = scratch.join("bad-facing.pins.json");
    std::fs::write(
        &bad_facing,
        r#"{"inputs": {"a": {"at": [1,1,1], "toward": "up"}}}"#,
    )
    .expect("the pins file is writable");
    let (output, _) = run_in_scratch(
        "pins-bad-facing",
        &[
            "and4",
            "--grown",
            "--pins",
            bad_facing.to_str().expect("utf-8 path"),
        ],
    );
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("pins file") && stderr.contains("\"up\"") && stderr.contains("\"a\""),
        "the refusal names the port and the bad facing: {stderr}"
    );

    // An output label the circuit does not declare is refused by name, with
    // the labels that would have worked.
    let scratch = scratch_dir("pins-bad-label");
    let bad_label = scratch.join("bad-label.pins.json");
    std::fs::write(
        &bad_label,
        r#"{"outputs": {"q": {"at": [5,1,2], "toward": "north"}}}"#,
    )
    .expect("the pins file is writable");
    let (output, _) = run_in_scratch(
        "pins-bad-label",
        &[
            "and4",
            "--grown",
            "--pins",
            bad_label.to_str().expect("utf-8 path"),
        ],
    );
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("\"q\"") && stderr.contains('y'),
        "the refusal names the label and offers the real ones: {stderr}"
    );

    // Pins compile through the generation front door only.
    let scratch = scratch_dir("pins-without-grown");
    let lawful = scratch.join("lawful.pins.json");
    std::fs::write(
        &lawful,
        r#"{"inputs": {"a": {"at": [21,1,62], "toward": "north"}}}"#,
    )
    .expect("the pins file is writable");
    let (output, _) = run_in_scratch(
        "pins-without-grown",
        &["and4", "--pins", lawful.to_str().expect("utf-8 path")],
    );
    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("--pins requires --grown"),
        "the refusal names the missing flag: {}",
        stderr_of(&output)
    );
}

/// A refusal only the door can make still reaches the caller by name, through
/// the file adapter, as the sentence it was written to be.
///
/// The file here is syntactically perfect and the parser is right to accept
/// it: an input pinned at z = 0 with `toward: north` puts its handover at
/// z = -1, and only `PortPlacements` -- which knows the port's role, and so
/// which neighbour `toward` resolves to -- can say so. The adapter does not
/// repeat that rule; it inherits it and renders it.
#[test]
fn a_refusal_only_the_door_can_make_reaches_the_caller_by_name() {
    let scratch = scratch_dir("pins-off-the-board");
    let off_board = scratch.join("off-board.pins.json");
    std::fs::write(
        &off_board,
        r#"{"inputs": {"a": {"at": [1,1,0], "toward": "north"}}}"#,
    )
    .expect("the pins file is writable");

    let (output, _) = run_in_scratch(
        "pins-off-the-board",
        &[
            "and4",
            "--grown",
            "--pins",
            off_board.to_str().expect("utf-8 path"),
        ],
    );
    assert!(
        !output.status.success(),
        "an unbuildable pin must not ship a circuit"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("`a`") && stderr.contains("(1, 1, -1)"),
        "the refusal names the pin and the cell that put it off the board: {stderr}"
    );
    assert!(
        !stderr.contains("OutsideEveryGrowableWorld"),
        "and renders it, rather than dumping the variant at the caller: {stderr}"
    );
}

/// A pin set nobody parsed meets exactly the same door, and the refusal comes
/// back as **data**.
///
/// The pins file is one adapter over `PortPlacements`; an editor and an
/// in-game mod are coming, and a rule written into the JSON reader would
/// protect the file and abandon both. So this assembles the pin set the way
/// they will -- `PortPlacements::pin`, no parser anywhere near it -- and calls
/// the same public entry `--pins` calls.
///
/// Both defects here are ones the reader can never even hand on: its grammar
/// admits four horizontal facing names, so `Facing::Up` cannot be spelled in a
/// file at all, and it leaves input names to the door on purpose. What they
/// exercise is therefore precisely the semantics a second adapter inherits for
/// free.
///
/// The assertion is `assert_eq!` against the whole error rather than a
/// substring of its sentence, because that is the difference the spec asks
/// for: an editor points at the pin that is wrong and a mod highlights a
/// block, and neither can do it with prose.
#[test]
fn a_pin_set_built_without_the_parser_meets_the_same_door() {
    let (netlist, _) = build_and4_netlist();
    let cell = Anchor { x: 21, y: 1, z: 62 };

    let mut vertical = PortPlacements::default();
    vertical.pin("a", cell, Facing::Up);
    let Err(error) = compile_grown(&netlist, &vertical) else {
        panic!("a vertical `toward` names no neighbour of the caller's cell");
    };
    assert_eq!(
        error,
        CompileError::InvalidPortPin {
            port: "a".to_string(),
            at: cell,
            refusal: PinRefusal::VerticalToward { toward: Facing::Up },
        }
    );

    let mut undeclared = PortPlacements::default();
    undeclared.pin("nowhere", cell, Facing::North);
    let Err(error) = compile_grown(&netlist, &undeclared) else {
        panic!("and4 declares no port `nowhere`");
    };
    assert_eq!(
        error,
        CompileError::InvalidPortPin {
            port: "nowhere".to_string(),
            at: cell,
            refusal: PinRefusal::UndeclaredPort,
        }
    );
}

/// The round trip: a pins file in, `compile_grown` under `--grown --pins`,
/// and a sidecar out that records each pinned port as the caller's cell, its
/// `toward`, and the resolved handover -- the output keyed by the display
/// label the caller pinned it under, unpinned ports keeping today's bare
/// `[x,y,z]` -- while the world itself ships every pinned cell empty with its
/// handover repeater in the one neighbour the pin names.
///
/// and4, because it grows at iteration 1 in milliseconds; the pin geometry
/// mirrors the acceptance tests' shape (input row south of the free layout,
/// output north of it, every `toward` carrying the signal along the circuit's
/// own north-running flow).
#[test]
fn a_pinned_and4_round_trips_through_the_flags() {
    let scratch = scratch_dir("pins-round-trip");
    let pins = scratch.join("and4.pins.json");
    std::fs::write(
        &pins,
        r#"{"inputs": {"a": {"at": [21, 1, 62], "toward": "north"}},
            "outputs": {"y": {"at": [53, 1, 10], "toward": "north"}}}"#,
    )
    .expect("the pins file is writable");

    let (output, scratch) = run_in_scratch(
        "pins-round-trip",
        &[
            "and4",
            "--grown",
            "--pins",
            pins.to_str().expect("utf-8 path"),
        ],
    );
    assert!(
        output.status.success(),
        "the pinned and4 compiles through the flags:\n{}",
        stderr_of(&output)
    );

    let sidecar = std::fs::read_to_string(scratch.join("output/and4.grown.pinout.json"))
        .expect("the pinout sidecar is written");
    assert!(
        sidecar.contains(r#""a":{"at":[21,1,62],"toward":"north","handover":[21,1,61]}"#),
        "the pinned input records the caller's cell, its toward, and the handover: {sidecar}"
    );
    assert!(
        sidecar.contains(r#""y":{"at":[53,1,10],"toward":"north","handover":[53,1,11]}"#),
        "the pinned output is keyed by its display label: {sidecar}"
    );
    assert!(
        !sidecar.contains("g6"),
        "the internal signal name does not leak into a pinned entry: {sidecar}"
    );
    assert!(
        sidecar.contains(r#""b":["#) && sidecar.contains(r#""c":["#),
        "unpinned ports keep today's bare coordinates: {sidecar}"
    );

    // The world behind the sidecar honours the contract at both pinned
    // cells: nothing at all in the caller's cell, and the handover repeater
    // exactly where the sidecar says it is.
    let dump = std::fs::read_to_string(scratch.join("output/and4.grown.blocks.txt"))
        .expect("the block dump is written");
    let cell = |x: i32, y: i32, z: i32| -> Option<String> {
        let prefix = format!("{x} {y} {z} ");
        dump.lines()
            .find(|line| line.starts_with(&prefix))
            .map(str::to_string)
    };
    assert_eq!(cell(21, 1, 62), None, "`a`'s pinned cell ships empty");
    assert_eq!(cell(53, 1, 10), None, "`y`'s pinned cell ships empty");
    for (x, y, z) in [(21, 1, 61), (53, 1, 11)] {
        let handover = cell(x, y, z).unwrap_or_else(|| panic!("({x}, {y}, {z}) holds a block"));
        assert!(
            handover.contains("Repeater") && handover.contains("South"),
            "the handover at ({x}, {y}, {z}) carries the signal north: {handover}"
        );
    }
}

type CheckedPin = (PortPin, PortRole, Anchor, Anchor);

fn checked_seven_segment_pin_contract() -> BTreeMap<String, CheckedPin> {
    let output_names = ["g18", "g21", "g25", "g17", "g27", "g28", "g30"];
    let outputs = [
        (
            Anchor { x: 76, y: 1, z: 24 },
            Facing::North,
            Anchor { x: 76, y: 1, z: 25 },
            Anchor { x: 76, y: 1, z: 26 },
        ),
        (
            Anchor { x: 84, y: 1, z: 32 },
            Facing::East,
            Anchor { x: 83, y: 1, z: 32 },
            Anchor { x: 82, y: 1, z: 32 },
        ),
        (
            Anchor { x: 84, y: 1, z: 48 },
            Facing::East,
            Anchor { x: 83, y: 1, z: 48 },
            Anchor { x: 82, y: 1, z: 48 },
        ),
        (
            Anchor { x: 76, y: 1, z: 56 },
            Facing::South,
            Anchor { x: 76, y: 1, z: 55 },
            Anchor { x: 76, y: 1, z: 54 },
        ),
        (
            Anchor { x: 68, y: 1, z: 48 },
            Facing::West,
            Anchor { x: 69, y: 1, z: 48 },
            Anchor { x: 70, y: 1, z: 48 },
        ),
        (
            Anchor { x: 68, y: 1, z: 32 },
            Facing::West,
            Anchor { x: 69, y: 1, z: 32 },
            Anchor { x: 70, y: 1, z: 32 },
        ),
        (
            Anchor { x: 76, y: 1, z: 40 },
            Facing::West,
            Anchor { x: 77, y: 1, z: 40 },
            Anchor { x: 78, y: 1, z: 40 },
        ),
    ];
    let inputs = [
        (
            "d3",
            Anchor {
                x: 76,
                y: 1,
                z: 120,
            },
            Anchor {
                x: 76,
                y: 1,
                z: 119,
            },
            Anchor {
                x: 76,
                y: 1,
                z: 118,
            },
        ),
        (
            "d2",
            Anchor {
                x: 88,
                y: 1,
                z: 120,
            },
            Anchor {
                x: 88,
                y: 1,
                z: 119,
            },
            Anchor {
                x: 88,
                y: 1,
                z: 118,
            },
        ),
        (
            "d1",
            Anchor {
                x: 100,
                y: 1,
                z: 120,
            },
            Anchor {
                x: 100,
                y: 1,
                z: 119,
            },
            Anchor {
                x: 100,
                y: 1,
                z: 118,
            },
        ),
        (
            "d0",
            Anchor {
                x: 112,
                y: 1,
                z: 120,
            },
            Anchor {
                x: 112,
                y: 1,
                z: 119,
            },
            Anchor {
                x: 112,
                y: 1,
                z: 118,
            },
        ),
    ];

    output_names
        .iter()
        .map(|name| (*name).to_string())
        .zip(outputs)
        .map(|(name, (at, toward, handover, net_cell))| {
            (
                name,
                (PortPin { at, toward }, PortRole::Output, handover, net_cell),
            )
        })
        .chain(inputs.into_iter().map(|(name, at, handover, net_cell)| {
            (
                name.to_string(),
                (
                    PortPin {
                        at,
                        toward: Facing::North,
                    },
                    PortRole::Input,
                    handover,
                    net_cell,
                ),
            )
        }))
        .collect()
}

fn actual_pin_contract(
    fixture: &reda::compile::fragment_synth::benchmark::BenchmarkFixture,
) -> BTreeMap<String, CheckedPin> {
    fixture
        .placements()
        .iter()
        .map(|(name, pin)| {
            let role = if fixture.lowered_netlist().inputs.contains(name) {
                PortRole::Input
            } else {
                PortRole::Output
            };
            (
                name.clone(),
                (*pin, role, pin.handover(role), pin.net_cell(role)),
            )
        })
        .collect()
}

#[test]
fn checked_seven_segment_fixture_binds_every_signal_to_its_literal_pin() {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    let fixture = evaluator.fixture("pinned:verilog:seven_segment").unwrap();
    let expected = checked_seven_segment_pin_contract();
    assert_eq!(actual_pin_contract(fixture), expected);
    for (name, (pin, _, _, _)) in expected {
        assert_eq!(
            fixture.placements().get(&name),
            Some(pin),
            "{name} moved or changed outside-facing direction"
        );
    }
}

fn expected_handover_repeater(toward: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Repeater;
    state.name = "minecraft:repeater".to_string();
    state.facing = Some(toward.opposite());
    state.delay = 1;
    state.lit = true;
    state
}

#[test]
fn topology_aware_seed_preserves_the_checked_seven_segment_pin_contract() {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    let fixture = evaluator.fixture("pinned:verilog:seven_segment").unwrap();
    let expected = checked_seven_segment_pin_contract();
    assert_eq!(actual_pin_contract(fixture), expected);

    let result = compile_fragment_synth(
        SynthesisInput {
            lowered: fixture.lowered_netlist(),
            source_provenance: None,
            pins: Some(fixture.placements()),
        },
        SynthesisBudget::Evaluations(0),
    )
    .unwrap();

    for (name, (pin, _, handover, net_cell)) in &expected {
        assert_eq!(
            result.compiled.world.get(pin.at.x, pin.at.y, pin.at.z),
            &BlockState::air(),
            "{name}'s caller-owned pin cell must remain exactly air"
        );
        assert_eq!(
            result
                .compiled
                .world
                .get(handover.x, handover.y, handover.z),
            &expected_handover_repeater(pin.toward),
            "{name}'s handover repeater changed state"
        );
        let net_state = result
            .compiled
            .world
            .get(net_cell.x, net_cell.y, net_cell.z);
        assert!(
            matches!(
                net_state.kind,
                BlockKind::RedstoneWire | BlockKind::Repeater
            ),
            "{name}'s exact net cell {net_cell:?} must be a route conductor, got {net_state:?}"
        );
    }

    let (size_x, size_y, size_z) = result.compiled.world.size();
    for z in 120..size_z {
        for y in 0..size_y {
            for x in 0..size_x {
                assert_eq!(
                    result.compiled.world.get(x, y, z).kind,
                    BlockKind::Air,
                    "internal or boundary block escaped the inputs' inward half-space at ({x}, {y}, {z})"
                );
            }
        }
    }
}

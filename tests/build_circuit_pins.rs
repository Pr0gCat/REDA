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
    compile_fragment_synth, compile_grown, compile_hierarchical, CompileError, HierarchicalNetlist,
    Module, ModuleInstance, PortBinding, SynthesisBudget, SynthesisInput,
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

/// The hierarchical front door, on a design of exactly one module, must be
/// the flat front door -- including under pins.
///
/// The seven-segment fixture is the strongest available statement of that:
/// eleven caller-owned `(Anchor, toward)` pins, each with a handover
/// repeater the caller reads and a net cell the route has to reach. If
/// `compile_hierarchical`'s single-module path diverged anywhere -- a
/// different lowering, a different case fingerprint, pins not forwarded --
/// the candidate would move and these cells would stop agreeing.
///
/// The fixture only exposes an already-lowered netlist, so wrapping it as a
/// module means `lower_hierarchy` lowers it a second time. That second pass
/// being the identity is asserted directly, and cheaply, by
/// `compile::fragment_synth::hierarchy_api::tests::lowering_an_already_lowered_netlist_is_the_identity`.
#[test]
fn compile_hierarchical_preserves_the_checked_seven_segment_pin_contract() {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    let fixture = evaluator.fixture("pinned:verilog:seven_segment").unwrap();
    let expected = checked_seven_segment_pin_contract();
    assert_eq!(expected.len(), 11, "the checked contract pins eleven ports");
    assert_eq!(actual_pin_contract(fixture), expected);

    let netlist = fixture.lowered_netlist();
    let mut modules = BTreeMap::new();
    modules.insert(
        "seven_segment".to_string(),
        Module {
            inputs: netlist.inputs.clone(),
            outputs: netlist.outputs.clone(),
            gates: netlist.gates.clone(),
            instances: vec![],
        },
    );
    let design = HierarchicalNetlist {
        top: "seven_segment".to_string(),
        modules,
    };

    let flat = compile_fragment_synth(
        SynthesisInput {
            lowered: netlist,
            source_provenance: None,
            pins: Some(fixture.placements()),
        },
        SynthesisBudget::Evaluations(0),
    )
    .unwrap();
    let hierarchical = compile_hierarchical(
        &design,
        SynthesisBudget::Evaluations(0),
        Some(fixture.placements()),
    )
    .unwrap();

    assert_eq!(
        hierarchical.candidate_fingerprint, flat.candidate_fingerprint,
        "a one-module hierarchy must compile to the very same candidate"
    );
    assert_eq!(
        hierarchical.case_fingerprint, flat.case_fingerprint,
        "a one-module hierarchy must be the very same synthesis case"
    );

    for (name, (pin, _, handover, net_cell)) in &expected {
        assert_eq!(
            hierarchical
                .compiled
                .world
                .get(pin.at.x, pin.at.y, pin.at.z),
            &BlockState::air(),
            "{name}'s caller-owned pin cell must remain exactly air"
        );
        assert_eq!(
            hierarchical
                .compiled
                .world
                .get(handover.x, handover.y, handover.z),
            &expected_handover_repeater(pin.toward),
            "{name}'s handover repeater changed state"
        );
        let net_state = hierarchical
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

/// A pinned *hierarchy with a child* -- not a one-module wrapper -- keeps
/// every caller-owned cell it was given, at the seed and again after the
/// proposal stream has run out.
///
/// `compile_hierarchical_preserves_the_checked_seven_segment_pin_contract`
/// already states the one-module case, where the hierarchical door is the
/// flat door and pins never cross a module boundary. This is the case that
/// door cannot state: `y` is the *parent's* output, bound to the child's own
/// output signal by a `ModuleInstance`, so the pin has to survive lowering,
/// the child's compile, and the splice back into one flat candidate before it
/// can be honoured -- and `a` has to survive the same trip as an input the
/// parent forwards to the child under the same name.
///
/// Both budget points are asserted because the two claims differ. At budget
/// zero nothing has been proposed, so the pins holding is a statement about
/// placement alone; at `u64::MAX` the finite block-edge proposal stream runs
/// to exhaustion, so the pins holding is the statement that search cannot
/// spend a caller-owned cell. Budget is not part of the synthesis case, so
/// both runs must share a case fingerprint, and the candidate may differ from
/// the seed's exactly when a proposal was accepted -- asserted as an iff, so
/// that a run which silently stopped accepting cannot pass by looking like a
/// run that had nothing to accept.
///
/// and4 is the smallest circuit that still proves pins survive a child-module
/// compile and splice. Both pins use the parent's direct east frame; the older
/// north-facing coordinates belong to the separate grown-layout engine.
#[test]
fn hierarchy_with_a_child_preserves_requested_pins_through_exhaustion() {
    let (netlist, output_signal) = build_and4_netlist();

    // The child owns every gate; the top owns none and exists only to
    // instance it, so nothing here can be honoured by a flat compile that
    // never looked at the hierarchy.
    let mut modules = BTreeMap::new();
    modules.insert(
        "and4".to_string(),
        Module {
            inputs: netlist.inputs.clone(),
            outputs: vec![output_signal.clone()],
            gates: netlist.gates.clone(),
            instances: vec![],
        },
    );
    let mut ports = BTreeMap::new();
    for input in &netlist.inputs {
        ports.insert(input.clone(), PortBinding::Signal(input.clone()));
    }
    ports.insert(output_signal.clone(), PortBinding::Signal("y".to_string()));
    modules.insert(
        "top".to_string(),
        Module {
            inputs: netlist.inputs.clone(),
            outputs: vec!["y".to_string()],
            gates: vec![],
            instances: vec![ModuleInstance {
                name: "and4_0".to_string(),
                module: "and4".to_string(),
                ports,
            }],
        },
    );
    let design = HierarchicalNetlist {
        top: "top".to_string(),
        modules,
    };

    let input_pin = PortPin {
        at: Anchor { x: 50, y: 1, z: 20 },
        toward: Facing::East,
    };
    let output_pin = PortPin {
        at: Anchor {
            x: 200,
            y: 1,
            z: 20,
        },
        toward: Facing::East,
    };
    let mut pins = PortPlacements::default();
    pins.pin("a", input_pin.at, input_pin.toward);
    pins.pin("y", output_pin.at, output_pin.toward);

    // Spelled out rather than derived, so a change to how a handover or a
    // net cell is resolved has to be re-agreed here in literal coordinates
    // instead of following the code that changed.
    let expected: BTreeMap<String, CheckedPin> = [
        (
            "a".to_string(),
            (
                input_pin,
                PortRole::Input,
                Anchor { x: 51, y: 1, z: 20 },
                Anchor { x: 52, y: 1, z: 20 },
            ),
        ),
        (
            "y".to_string(),
            (
                output_pin,
                PortRole::Output,
                Anchor {
                    x: 199,
                    y: 1,
                    z: 20,
                },
                Anchor {
                    x: 198,
                    y: 1,
                    z: 20,
                },
            ),
        ),
    ]
    .into_iter()
    .collect();
    for (name, (pin, role, handover, net_cell)) in &expected {
        assert_eq!(
            pin.handover(*role),
            *handover,
            "{name}'s handover is the one neighbour its `toward` names"
        );
        assert_eq!(
            pin.net_cell(*role),
            *net_cell,
            "{name}'s net cell is one step past its handover"
        );
    }

    let mut results = Vec::new();
    for budget in [0, u64::MAX] {
        let result =
            compile_hierarchical(&design, SynthesisBudget::Evaluations(budget), Some(&pins))
                .unwrap_or_else(|error| {
                    panic!("budget={budget}: the pinned hierarchy must certify: {error}")
                });

        assert_eq!(
            result.compiled.input_positions.get("a"),
            Some(&(50, 1, 20)),
            "budget={budget}: `a` reports the caller's own cell, not a lever REDA chose"
        );
        assert_eq!(
            result.compiled.output_positions.get("y"),
            Some(&(200, 1, 20)),
            "budget={budget}: `y` reports the caller's own cell, not a lamp REDA chose"
        );
        assert_eq!(
            result.compiled.output_positions.len(),
            1,
            "budget={budget}: the top declares exactly one output, and the child's \
             internal signal name is not one of them"
        );
        for name in &netlist.inputs {
            assert!(
                result.compiled.input_positions.contains_key(name),
                "budget={budget}: the parent forwards every declared input, {name} included"
            );
        }

        for (name, (pin, _, handover, net_cell)) in &expected {
            assert_eq!(
                result.compiled.world.get(pin.at.x, pin.at.y, pin.at.z),
                &BlockState::air(),
                "budget={budget}: {name}'s caller-owned pin cell must remain exactly air"
            );
            assert_eq!(
                result
                    .compiled
                    .world
                    .get(handover.x, handover.y, handover.z),
                &expected_handover_repeater(pin.toward),
                "budget={budget}: {name}'s handover repeater changed state"
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
                "budget={budget}: {name}'s exact net cell {net_cell:?} must be a route \
                 conductor, got {net_state:?}"
            );
        }

        results.push(result);
    }

    let exhausted = results.pop().expect("the exhausted point ran");
    let seeded = results.pop().expect("the seeded point ran");
    assert_eq!(
        exhausted.case_fingerprint, seeded.case_fingerprint,
        "budget is not part of the synthesis case, so both runs compile the same case"
    );
    let accepted_any = exhausted.trace.iter().any(|entry| entry.accepted);
    assert_eq!(
        exhausted.candidate_fingerprint == seeded.candidate_fingerprint,
        !accepted_any,
        "the exhausted run's candidate may differ from the seed's exactly when it \
         accepted a proposal: accepted_any={accepted_any}, trace={:?}",
        exhausted.trace
    );
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

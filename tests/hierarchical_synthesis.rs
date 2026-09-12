//! End-to-end proof that the Verilog front end reaches `compile_hierarchical`:
//! Yosys synthesizes a design with a real module instance (not a flattened
//! gate soup), the reader (`reda::frontend::synthesize_verilog_hierarchical`)
//! keeps that instance as a `ModuleInstance` rather than inlining it, and
//! `compile_hierarchical` compiles the result -- the same front door task
//! 2-12 built and `tests/fixtures/ripple_adder8.v` describes, this time
//! synthesized by a real tool instead of `circuits::hierarchical_builder`.
//!
//! # Why this needs Python + `yowasp-yosys`
//!
//! Same dependency as `tests/verilog_frontend.rs` (see that file's module
//! doc comment): `synthesize_verilog_hierarchical` shells out to Yosys via
//! the `yowasp-yosys` Python package. This test is additionally marked
//! `#[ignore]` because `compile_hierarchical` on even a small hierarchy is a
//! release-only number of seconds, not something the fast default suite
//! should pay for on every run.

use std::collections::BTreeMap;

use reda::compile::planner::{Anchor, PortPlacements};
use reda::compile::topology::GateKind;
use reda::compile::{
    compile_hierarchical, Gate, HierarchicalNetlist, HierarchyError, Module, ModuleInstance,
    PortBinding, SynthesisBudget,
};
use reda::frontend::synthesize_verilog_hierarchical;
use reda::redstone::world::block::{BlockKind, Facing};

/// Every distinct module reachable from `design.top` (`top` included),
/// walking `ModuleInstance::module` transitively.
///
/// Private, and only ever called on an already-specialised design -- see
/// [`compiled_module_count`], the sole caller.
fn distinct_modules(design: &HierarchicalNetlist) -> usize {
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    let mut pending = vec![design.top.clone()];
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(module) = design.modules.get(&name) {
            for instance in &module.instances {
                pending.push(instance.module.clone());
            }
        }
    }
    seen.len()
}

/// "How many distinct modules does `compile_hierarchical` compile once each
/// for `design`" -- the same count `seed.rs`'s hierarchical harness reports
/// as `blocks_compiled=`, duplicated here (specialise, then walk) because
/// this integration test cannot reach that harness's `#[cfg(test)]`-only
/// helper across the crate boundary -- see
/// `src/compile/fragment_synth/seed.rs`'s `compiled_module_count` for the
/// full rationale (`compile_hierarchical` specialises constant-tied ports
/// before deriving the module set it compiles, so walking the raw design
/// undercounts whenever a port is tied to a constant).
fn compiled_module_count(design: &HierarchicalNetlist) -> Result<usize, HierarchyError> {
    design
        .specialise_constants()
        .map(|specialised| distinct_modules(&specialised))
}

#[test]
fn bounded_parent_stamps_one_module_on_two_decks() {
    let child = Module {
        inputs: vec!["a".into()],
        outputs: vec!["y".into()],
        gates: vec![Gate {
            name: "not".into(),
            inputs: vec!["a".into()],
            output: "y".into(),
            kind: GateKind::Nor(1),
        }],
        instances: vec![],
    };
    let instance = |name: &str, input: &str, output: &str| ModuleInstance {
        name: name.into(),
        module: "child".into(),
        ports: BTreeMap::from([
            ("a".into(), PortBinding::Signal(input.into())),
            ("y".into(), PortBinding::Signal(output.into())),
        ]),
    };
    let top = Module {
        inputs: vec!["a".into()],
        outputs: vec!["y".into()],
        gates: vec![],
        instances: vec![instance("low", "a", "mid"), instance("high", "mid", "y")],
    };
    let design = HierarchicalNetlist {
        top: "top".into(),
        modules: BTreeMap::from([("child".into(), child), ("top".into(), top)]),
    };
    let mut pins = PortPlacements::default();
    pins.pin("a", Anchor { x: 0, y: 1, z: 0 }, Facing::East)
        .pin(
            "y",
            Anchor {
                x: 100,
                y: 1,
                z: 30,
            },
            Facing::East,
        );

    assert_eq!(compiled_module_count(&design).unwrap(), 2);
    assert_eq!(design.modules["top"].instances[0].module, "child");
    assert_eq!(design.modules["top"].instances[1].module, "child");

    let result = compile_hierarchical(&design, SynthesisBudget::Evaluations(0), Some(&pins))
        .expect("the bounded parent certifies");
    assert_eq!(result.compiled.input_positions["a"], (0, 1, 0));
    assert_eq!(result.compiled.output_positions["y"], (100, 1, 30));
    let (size_x, size_y, size_z) = result.compiled.world.size();
    assert!((5..size_y).any(|y| {
        (0..size_z)
            .any(|z| (0..size_x).any(|x| result.compiled.world.get(x, y, z).kind != BlockKind::Air))
    }));
}

#[test]
#[ignore = "release-only: needs python + yosys; cargo test --release --test hierarchical_synthesis -- --ignored"]
fn the_verilog_ripple_adder8_keeps_eight_full_adder_instances_and_certifies() {
    let source = std::fs::read_to_string("tests/fixtures/ripple_adder8.v").expect("fixture must exist");
    let (design, _port_map) = synthesize_verilog_hierarchical(&source, "ripple_adder8")
        .unwrap_or_else(|error| panic!("ripple_adder8.v must synthesize: {error}"));

    let top = &design.modules["ripple_adder8"];
    eprintln!(
        "ripple_adder8: {} top-level instances, modules = {:?}",
        top.instances.len(),
        design.modules.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        top.instances.len(),
        8,
        "expected eight `full_adder` instances under `ripple_adder8` -- if this is 0, Yosys \
         likely inlined the generate loop into one flat gate list instead of keeping instances; \
         see `tests/fixtures/ripple_adder8.v`'s `(* keep_hierarchy *)` comment"
    );
    assert!(
        design.modules.keys().any(|name| name.contains("full_adder")),
        "expected a module named `full_adder` (possibly Yosys-mangled) among {:?}",
        design.modules.keys().collect::<Vec<_>>()
    );

    let gate_count = design
        .specialise_constants()
        .ok()
        .and_then(|specialised| specialised.flatten().ok())
        .map(|(flat, _)| flat.gates.len());
    let blocks_compiled = compiled_module_count(&design).ok();

    let started = std::time::Instant::now();
    let result = compile_hierarchical(&design, SynthesisBudget::Evaluations(0), None)
        .unwrap_or_else(|error| panic!("the synthesized ripple_adder8 must certify: {error}"));
    eprintln!(
        "CIRCUIT verilog_ripple_adder8 (hierarchical): OK gates={} blocks_compiled={} \
         ticks={} blocks={} in {:?}",
        gate_count.map(|count| count.to_string()).unwrap_or_else(|| "?".to_string()),
        blocks_compiled.map(|count| count.to_string()).unwrap_or_else(|| "?".to_string()),
        result.metrics.quality.observed_settle,
        result.metrics.quality.non_air_blocks,
        started.elapsed()
    );
    assert!(result.metrics.quality.observed_settle > 0);
}

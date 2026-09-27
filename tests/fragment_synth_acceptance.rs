use std::path::PathBuf;

use reda::compile::fragment_synth::benchmark::{
    build_acceptance_report, deterministic_budget_orders, evaluate_fragment_budget,
    evaluate_fragment_case_with_world, legacy_benchmark_evaluator, shipping_config_source,
    BenchmarkBaseline, FragmentAcceptanceCase,
};
use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisBudget, SynthesisInput};
use reda::compile::CompiledCircuit;
use reda::redstone::world::block::BlockKind;

const CASES: [&str; 6] = [
    "and4",
    "verilog:and4",
    "full_adder",
    "segment_a",
    "seven_segment",
    "pinned:verilog:seven_segment",
];

fn baseline() -> BenchmarkBaseline {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fragment_synth_baseline.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn replacement_corpus_is_complete_and_has_checked_pinned_glyph_io() {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    assert_eq!(
        evaluator
            .fixtures()
            .iter()
            .map(|fixture| fixture.name())
            .collect::<Vec<_>>(),
        CASES
    );
    let pinned = evaluator.fixture("pinned:verilog:seven_segment").unwrap();
    assert_eq!(pinned.placements().iter().count(), 11);
}

#[test]
fn topology_aware_seed_v2_and4_quality() {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    let fixture = evaluator.fixture("and4").unwrap();
    let result = compile_fragment_synth(
        SynthesisInput {
            lowered: fixture.lowered_netlist(),
            source_provenance: None,
            pins: Some(fixture.placements()),
        },
        SynthesisBudget::Evaluations(0),
    )
    .expect("the and4 recursive contract circuit must route and certify");
    let measured = evaluator.evaluate_world("and4", &result.compiled).unwrap();
    let ticks = measured.max_observed_settle_game_ticks_on_manifest.unwrap();
    let blocks = measured.physical.unwrap().non_air_blocks;

    assert!(
        ticks <= 36,
        "and4 measured {ticks} ticks, expected at most 36"
    );
    assert!(
        blocks <= 944,
        "and4 measured {blocks} non-air blocks, expected at most 944"
    );
}

/// One line per case, printed the moment the case finishes: what the
/// aggregate run used to print only after every case had run.
///
/// To watch one case as it runs, run its wrapper alone and unbuffered:
///
/// ```text
/// cargo test --release --test fragment_synth_acceptance budget_zero_and4 -- --exact --nocapture
/// ```
///
/// or build once with `--no-run` and run the printed binary the same way,
/// which is what a wall-clock cap around the test process needs.
fn report_case(case: &FragmentAcceptanceCase) {
    let ticks = case
        .measured
        .as_ref()
        .and_then(|measured| measured.max_observed_settle_game_ticks_on_manifest);
    let blocks = case
        .measured
        .as_ref()
        .and_then(|measured| measured.physical.as_ref())
        .map(|physical| physical.non_air_blocks);
    eprintln!(
        "budget-zero {}: certified={} ticks={ticks:?} blocks={blocks:?} passed={} error={:?}",
        case.name,
        case.compiled_and_certified,
        case.passed(),
        case.error,
    );
}

/// One canonical case at budget zero, scored exactly as the corpus run scores
/// it, with the circuit that was scored when there is one. Judges nothing.
fn budget_zero_case(name: &str) -> (FragmentAcceptanceCase, Option<CompiledCircuit>) {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    let fixture = evaluator.fixture(name).unwrap();
    evaluate_fragment_case_with_world(&evaluator, &baseline(), fixture, 0)
}

/// Every pin the fixture supplied is a caller's own cell: the product reports
/// the port under its declared role at exactly that coordinate, never under
/// the other role, and ships the cell empty. Direction is not on the product
/// surface -- positions are coordinates alone -- so it is not asserted.
///
/// Vacuous for an unpinned fixture, by design: the pinned glyph is the one
/// case with pins, and `replacement_corpus_is_complete_and_has_checked_pinned_glyph_io`
/// holds its pin count to eleven, so this checks eleven cells there and none
/// elsewhere.
fn assert_supplied_pins_honoured(name: &str, compiled: &CompiledCircuit) -> usize {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    let fixture = evaluator.fixture(name).unwrap();
    let netlist = fixture.lowered_netlist();
    let mut checked = 0;
    for (signal, pin) in fixture.placements().iter() {
        let expected = (pin.at.x, pin.at.y, pin.at.z);
        let reported = if netlist.inputs.contains(signal) {
            assert!(
                !compiled.output_positions.contains_key(signal),
                "{name}: {signal} is an input, reported as an output"
            );
            compiled.input_positions.get(signal)
        } else {
            assert!(
                netlist.outputs.contains(signal),
                "{name}: {signal} is pinned but not declared"
            );
            assert!(
                !compiled.input_positions.contains_key(signal),
                "{name}: {signal} is an output, reported as an input"
            );
            compiled.output_positions.get(signal)
        };
        assert_eq!(
            reported,
            Some(&expected),
            "{name}: {signal} is not reported at the caller's pinned cell"
        );
        assert_eq!(
            compiled.world.get(pin.at.x, pin.at.y, pin.at.z).kind,
            BlockKind::Air,
            "{name}: {signal}'s caller cell {expected:?} did not ship empty"
        );
        checked += 1;
    }
    checked
}

/// The per-case gate for one name: report first, so the line is there
/// whatever follows; check the scored circuit's pins whenever there is a
/// circuit, even when scoring failed, so a geometry fault is named alongside
/// a scoring one; judge last.
fn run_budget_zero_case(name: &str) {
    let (case, compiled) = budget_zero_case(name);
    report_case(&case);
    if let Some(compiled) = &compiled {
        let checked = assert_supplied_pins_honoured(name, compiled);
        eprintln!("budget-zero {name}: {checked} supplied pins honoured");
    }
    assert!(
        case.passed(),
        "{name}: passed={} compiled_and_certified={} no_tick_regression={} \
         no_block_regression={} pinned_ten_percent_tick_improvement={:?} \
         strict_block_improvement={:?} error={:?}",
        case.passed(),
        case.compiled_and_certified,
        case.no_tick_regression,
        case.no_block_regression,
        case.pinned_ten_percent_tick_improvement,
        case.strict_block_improvement,
        case.error,
    );
}

/// The per-case gate: one exact test per canonical case, and the list the
/// anti-drift test holds to `CASES`, both from this one mapping. `check.sh`
/// counts the registered `budget_zero_*` tests off `--list` against the same
/// six.
macro_rules! budget_zero_case_tests {
    ($($test:ident => $case:literal),* $(,)?) => {
        const WRAPPED_CASES: &[&str] = &[$($case),*];
        $(
            #[test]
            fn $test() {
                run_budget_zero_case($case);
            }
        )*
    };
}

budget_zero_case_tests! {
    budget_zero_and4 => "and4",
    budget_zero_verilog_and4 => "verilog:and4",
    budget_zero_full_adder => "full_adder",
    budget_zero_segment_a => "segment_a",
    budget_zero_seven_segment => "seven_segment",
    budget_zero_pinned_verilog_seven_segment => "pinned:verilog:seven_segment",
}

/// The per-case wrappers are the gate, so they must cover exactly the
/// canonical corpus: no case without a wrapper, no wrapper for a case that
/// is not in the corpus, no case wrapped twice.
#[test]
fn per_case_wrappers_cover_exactly_the_canonical_cases() {
    let mut wrapped = WRAPPED_CASES.to_vec();
    wrapped.sort_unstable();
    let mut canonical = CASES.to_vec();
    canonical.sort_unstable();
    assert_eq!(wrapped, canonical, "per-case wrappers drifted from CASES");
    assert_eq!(WRAPPED_CASES.len(), CASES.len());
}

/// The whole corpus in one process. Superseded as the gate by the six
/// `budget_zero_*` tests above, which score each case identically through
/// `evaluate_fragment_case` and report it as soon as it finishes; this stays
/// as the one-shot corpus run for a report, not for CI.
#[test]
#[ignore = "the six budget_zero_* per-case tests are the gate; this is the same corpus in one process"]
fn topology_aware_seed_v2_budget_zero_corpus_certifies() {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    let run = evaluate_fragment_budget(&evaluator, &baseline(), 0);
    let failures = run
        .cases
        .iter()
        .filter(|case| !case.passed())
        .map(|case| {
            format!(
                "{}: passed={} compiled_and_certified={} no_tick_regression={} \
                 no_block_regression={} pinned_ten_percent_tick_improvement={:?} \
                 strict_block_improvement={:?} error={:?}",
                case.name,
                case.passed(),
                case.compiled_and_certified,
                case.no_tick_regression,
                case.no_block_regression,
                case.pinned_ten_percent_tick_improvement,
                case.strict_block_improvement,
                case.error,
            )
        })
        .collect::<Vec<_>>();

    for case in &run.cases {
        report_case(case);
    }

    assert!(
        run.passed,
        "budget-zero acceptance run failed: {failures:#?}"
    );
    assert!(
        failures.is_empty(),
        "budget-zero acceptance failures: {failures:#?}"
    );
}

#[test]
fn shuffled_budget_orders_are_seeded_repeatable_and_order_sensitive() {
    let budgets = [0, 1, 2, 4, 8];
    let first = deterministic_budget_orders(&budgets, 3, 0x5245_4441_2026_0831);
    let repeated = deterministic_budget_orders(&budgets, 3, 0x5245_4441_2026_0831);
    let changed = deterministic_budget_orders(&budgets, 3, 0x5245_4441_2026_0832);
    assert_eq!(first, repeated);
    assert_ne!(first, changed);
    for order in first {
        let mut sorted = order;
        sorted.sort_unstable();
        assert_eq!(sorted, budgets);
    }
}

/// A failed gate produces no shipping budget, no shipping source text, and
/// leaves no generated `shipping_config.rs` in the tree.
#[test]
fn a_failed_gate_cannot_generate_a_shipping_configuration() {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    let report = build_acceptance_report(&evaluator, &baseline(), &[], 0x5245_4441_2026_0831);
    assert!(!report.replacement_gate_passed);
    assert!(report.shipping_evaluations.is_none());
    assert!(shipping_config_source(&report).is_err());
    assert!(!PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/compile/fragment_synth/shipping_config.rs")
        .exists());
}

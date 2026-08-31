use std::path::PathBuf;

use reda::compile::fragment_synth::benchmark::{
    build_acceptance_report, deterministic_budget_orders, legacy_benchmark_evaluator,
    shipping_config_source, BenchmarkBaseline,
};

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

#[test]
fn a_failed_gate_cannot_generate_a_shipping_configuration() {
    let evaluator = legacy_benchmark_evaluator().unwrap();
    let report = build_acceptance_report(&evaluator, &baseline(), &[], 0x5245_4441_2026_0831);
    assert!(!report.replacement_gate_passed);
    assert!(report.shipping_evaluations.is_none());
    assert!(shipping_config_source(&report).is_err());
}

#[test]
fn checked_failure_report_names_every_failed_condition_and_no_shipping_source_exists() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("tests/fixtures/fragment_synth_shipping.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report["replacement_gate_passed"], false);
    assert_eq!(report["repeatability_executed"], false);
    assert!(report["shipping_evaluations"].is_null());
    assert_eq!(report["failures"].as_array().unwrap().len(), CASES.len());
    assert!(!root
        .join("src/compile/fragment_synth/shipping_config.rs")
        .exists());
}

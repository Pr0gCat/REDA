use reda::compile::lowering::{
    lower, lower_optimised, lower_optimised_with_provenance, lower_with_assignment,
    lower_with_provenance,
};
use reda::compile::topology::{expansion_for_polarity, GateKind, SignalPolarity};
use reda::compile::{Gate, Netlist};
use reda::frontend::evaluate::Evaluator;
use std::collections::BTreeMap;
fn source(count: usize, hybrid: bool) -> Netlist {
    let inputs: Vec<_> = (0..count).map(|i| format!("n_{i}")).collect();
    let mut gates = vec![Gate {
        name: "wide-cell".into(),
        inputs: inputs.clone(),
        output: "merged".into(),
        kind: GateKind::Or(count),
    }];
    let mut outputs = vec!["merged".into()];
    if hybrid {
        gates.push(Gate {
            name: "dependent".into(),
            inputs: vec!["merged".into(), inputs[0].clone()],
            output: "dependent".into(),
            kind: GateKind::And,
        });
        outputs.push("dependent".into());
    }
    Netlist {
        inputs,
        outputs,
        gates,
    }
}
fn check(source: &Netlist, physical: &Netlist) {
    assert_eq!(source.inputs, physical.inputs);
    assert_eq!(source.outputs, physical.outputs);
    assert!(physical
        .gates
        .iter()
        .all(|g| matches!(g.kind, GateKind::Nor(0..=3) | GateKind::Or(2..=3))));
    assert_eq!(
        lower(physical).unwrap(),
        *physical,
        "lowering must be idempotent"
    );
    let evaluator = Evaluator::new(physical).unwrap();
    for bits in 0..1usize << source.inputs.len() {
        let values: BTreeMap<_, _> = source
            .inputs
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), bits & (1 << i) != 0))
            .collect();
        let actual = evaluator.evaluate(&values).unwrap();
        assert_eq!(actual["merged"], bits != 0);
        if source.outputs.len() > 1 {
            assert_eq!(actual["dependent"], bits & 1 != 0);
        }
    }
}
#[test]
fn all_lowering_entries_preserve_wide_or_truth_and_ownership() {
    for count in [4, 5, 7, 10] {
        for hybrid in [false, true] {
            let s = source(count, hybrid);
            check(&s, &lower(&s).unwrap());
            let (p, owners) = lower_with_provenance(&s).unwrap();
            check(&s, &p);
            assert_eq!(owners.len(), p.gates.len());
            assert!(owners.iter().all(|o| *o < s.gates.len()));
            let p = lower_optimised(&s).unwrap();
            check(&s, &p);
            let p = lower_optimised_with_provenance(&s).unwrap();
            check(&s, &p.netlist);
            assert_eq!(p.provenance.len(), p.netlist.gates.len());
            assert_eq!(p.source_terminals.len(), s.gates.len());
            for (owner, terminals) in p.source_terminals.iter().enumerate() {
                assert!(!terminals.is_empty());
                for t in terminals {
                    assert_eq!(p.provenance[*t], owner);
                }
                assert!(terminals
                    .iter()
                    .any(|t| p.netlist.gates[*t].output == s.gates[owner].output));
            }
            for wide in [SignalPolarity::Positive, SignalPolarity::Negative] {
                let mut assignment = vec![SignalPolarity::Positive; s.gates.len()];
                assignment[0] = wide;
                check(&s, &lower_with_assignment(&s, &assignment).unwrap());
                let e = expansion_for_polarity(GateKind::Or(count), wide);
                e.validate(GateKind::Or(count));
                for bits in 0..1usize << count {
                    let inputs: Vec<_> = (0..count).map(|i| bits & (1 << i) != 0).collect();
                    assert_eq!(
                        e.evaluate(&inputs),
                        (bits != 0) ^ (wide == SignalPolarity::Negative)
                    );
                }
            }
        }
    }
}

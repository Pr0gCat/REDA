//! Per-gate choices of which physical rail lowering should emit.

use std::collections::HashMap;

use crate::compile::lowering::{lower_with_assignment, LowerError};
use crate::compile::topology::{entry_cost, GateKind, Library, SignalPolarity};
use crate::compile::Netlist;

/// One requested output polarity for every source gate, in `Netlist::gates`
/// order.
pub type PolarityAssignment = Vec<SignalPolarity>;

/// Why a polarity assignment could not be selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolarityError {
    /// The assignment pass needs one stable dependency order for every full
    /// lowering it scores.
    CyclicNetlist,
    /// A declared output must resolve to a source-gate rail or to a primary
    /// input rail before lowering can preserve it.
    OutputHasNoProducer { output: String },
    /// A directly realisable gate reached scoring, but the default physical
    /// library has no footprint for its declared kind.
    MissingDefaultLibraryEntry { kind: GateKind },
    /// Validation performed by the assigned lowering itself, such as a bad
    /// gate arity or an unresolved gate input.
    Lowering(LowerError),
}

impl std::fmt::Display for PolarityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolarityError::CyclicNetlist => write!(f, "cannot assign polarities to a cyclic netlist"),
            PolarityError::OutputHasNoProducer { output } => {
                write!(f, "declared output `{output}` has no gate or input producer")
            }
            PolarityError::MissingDefaultLibraryEntry { kind } => {
                write!(f, "the default library has no entry for {kind:?}")
            }
            PolarityError::Lowering(error) => write!(f, "cannot score a polarity assignment: {error}"),
        }
    }
}

impl std::error::Error for PolarityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PolarityError::Lowering(error) => Some(error),
            PolarityError::CyclicNetlist
            | PolarityError::OutputHasNoProducer { .. }
            | PolarityError::MissingDefaultLibraryEntry { .. } => None,
        }
    }
}

/// The static cost of a fully lowered candidate.  This intentionally omits
/// routing: physical placement measures that separately after lowering has
/// found a Pareto candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LoweredScore {
    area: u32,
    gates: usize,
    torch_depth: u32,
}

/// Choose physical output rails with a deterministic whole-netlist local
/// search.
///
/// Every candidate is lowered before it is scored, so `NetlistBuilder`'s
/// shared inverter cache and reconvergent fan-out contribute exactly once.
/// This is deliberately a local search, not a claim of a mathematical global
/// optimum: it descends over all eligible single-gate flips, then tests every
/// eligible pair once and descends over singles again if that pair wins.
pub fn assign_polarities(netlist: &Netlist) -> Result<PolarityAssignment, PolarityError> {
    netlist.topological_order().ok_or(PolarityError::CyclicNetlist)?;
    validate_outputs(netlist)?;

    let mut assignment = vec![SignalPolarity::Positive; netlist.gates.len()];
    let eligible: Vec<usize> = netlist
        .gates
        .iter()
        .enumerate()
        .filter_map(|(index, gate)| (!gate.kind.is_realisable()).then_some(index))
        .collect();
    let mut current = score(netlist, &assignment)?;

    single_gate_descent(netlist, &eligible, &mut assignment, &mut current)?;

    let mut pair_best = assignment.clone();
    let mut pair_score = current;
    for (offset, &first) in eligible.iter().enumerate() {
        for &second in &eligible[offset + 1..] {
            let mut candidate = assignment.clone();
            flip(&mut candidate[first]);
            flip(&mut candidate[second]);
            let candidate_score = score(netlist, &candidate)?;
            if candidate_score < pair_score {
                pair_best = candidate;
                pair_score = candidate_score;
            }
        }
    }
    if pair_score < current {
        assignment = pair_best;
        current = pair_score;
        single_gate_descent(netlist, &eligible, &mut assignment, &mut current)?;
    }

    Ok(assignment)
}

fn validate_outputs(netlist: &Netlist) -> Result<(), PolarityError> {
    for output in &netlist.outputs {
        let is_primary_input = netlist.inputs.iter().any(|input| input == output);
        let is_gate_output = netlist.gates.iter().any(|gate| gate.output == *output);
        if !is_primary_input && !is_gate_output {
            return Err(PolarityError::OutputHasNoProducer { output: output.clone() });
        }
    }
    Ok(())
}

fn single_gate_descent(
    netlist: &Netlist,
    eligible: &[usize],
    assignment: &mut PolarityAssignment,
    current: &mut LoweredScore,
) -> Result<(), PolarityError> {
    loop {
        let mut best_assignment = assignment.clone();
        let mut best_score = *current;
        for &index in eligible {
            let mut candidate = assignment.clone();
            flip(&mut candidate[index]);
            let candidate_score = score(netlist, &candidate)?;
            if candidate_score < best_score {
                best_assignment = candidate;
                best_score = candidate_score;
            }
        }
        if best_score == *current {
            return Ok(());
        }
        *assignment = best_assignment;
        *current = best_score;
    }
}

fn flip(polarity: &mut SignalPolarity) {
    *polarity = match *polarity {
        SignalPolarity::Positive => SignalPolarity::Negative,
        SignalPolarity::Negative => SignalPolarity::Positive,
    };
}

fn score(netlist: &Netlist, assignment: &[SignalPolarity]) -> Result<LoweredScore, PolarityError> {
    let lowered = lower_with_assignment(netlist, assignment).map_err(PolarityError::Lowering)?;
    score_realisable_netlist(&lowered)
}

fn score_realisable_netlist(netlist: &Netlist) -> Result<LoweredScore, PolarityError> {
    let library = Library::default_library();
    let producer_of: HashMap<&str, usize> =
        netlist.gates.iter().enumerate().map(|(index, gate)| (gate.output.as_str(), index)).collect();
    let order = netlist
        .topological_order()
        .expect("lower_with_assignment returns a topologically ordered realisable netlist");
    let mut torch_depth_of_gate = vec![0u32; netlist.gates.len()];
    let mut area = 0u32;
    let mut torch_depth = 0u32;

    for index in order {
        let gate = &netlist.gates[index];
        debug_assert!(gate.kind.is_realisable());
        let entry = library
            .choose(gate.kind)
            .ok_or(PolarityError::MissingDefaultLibraryEntry { kind: gate.kind })?;
        area += entry_cost(gate.kind, entry).area;

        let upstream_depth = gate
            .inputs
            .iter()
            .filter_map(|input| producer_of.get(input.as_str()).map(|&producer| torch_depth_of_gate[producer]))
            .max()
            .unwrap_or(0);
        let depth = match gate.kind {
            GateKind::Nor(_) => upstream_depth + 1,
            GateKind::Or(_) => upstream_depth,
            _ => unreachable!("lowered netlists contain only realisable gates"),
        };
        torch_depth_of_gate[index] = depth;
        torch_depth = torch_depth.max(depth);
    }

    Ok(LoweredScore { area, gates: netlist.gates.len(), torch_depth })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuits::verilog;
    use crate::compile::topology::GateKind;
    use crate::compile::{Gate, Netlist};

    fn netlist(inputs: &[&str], outputs: &[&str], gates: Vec<Gate>) -> Netlist {
        Netlist {
            inputs: inputs.iter().map(|input| (*input).to_string()).collect(),
            outputs: outputs.iter().map(|output| (*output).to_string()).collect(),
            gates,
        }
    }

    fn gate(kind: GateKind, output: &str, inputs: &[&str]) -> Gate {
        Gate {
            name: output.to_string(),
            inputs: inputs.iter().map(|input| (*input).to_string()).collect(),
            output: output.to_string(),
            kind,
        }
    }

    /// Removing the full-netlist scoring pass would leave this producer on
    /// its positive rail, even though its only consumer can directly use the
    /// negative rail and thereby avoid a shared inverse.
    #[test]
    fn assignment_prefers_a_producers_negative_output_when_its_only_consumer_reads_that_polarity() {
        let source = netlist(
            &["a", "b", "c"],
            &["y"],
            vec![gate(GateKind::And, "p", &["a", "b"]), gate(GateKind::Nand, "y", &["p", "c"])],
        );

        assert_eq!(assign_polarities(&source).unwrap()[0], SignalPolarity::Negative);
    }

    /// Removing stable candidate ordering would make this result depend on
    /// hash iteration or another unstable traversal of the same decoder.
    #[test]
    fn assignment_is_deterministic_for_the_baked_decoder() {
        let (source, _) = verilog::find("verilog:seven_segment")
            .expect("the baked decoder is registered")
            .baked_netlist();

        let first = assign_polarities(&source).unwrap();
        assert_eq!(first, assign_polarities(&source).unwrap());
    }

    /// Removing cycle validation would let the optimiser return a polarity
    /// vector for a graph it cannot lower in dependency order.
    #[test]
    fn assignment_rejects_a_cyclic_netlist() {
        let source = netlist(
            &[],
            &["a"],
            vec![gate(GateKind::And, "a", &["b", "b"]), gate(GateKind::And, "b", &["a", "a"])],
        );

        assert_eq!(assign_polarities(&source), Err(PolarityError::CyclicNetlist));
    }

    /// Removing declared-output validation would make a polarity assignment
    /// look successful even though the requested output can never be
    /// materialised by lowering.
    #[test]
    fn assignment_rejects_an_output_without_a_gate_or_input_producer() {
        let source = netlist(&["a"], &["missing"], vec![]);

        assert_eq!(
            assign_polarities(&source),
            Err(PolarityError::OutputHasNoProducer { output: "missing".to_string() })
        );
    }

    /// A malformed directly realisable gate can pass assigned lowering but
    /// cannot be priced without a default-library entry. Assignment must
    /// report that validation failure instead of panicking while scoring.
    #[test]
    fn assignment_rejects_realisable_kinds_missing_from_the_default_library() {
        for kind in [GateKind::Nor(4), GateKind::Or(1)] {
            let source = netlist(&["a", "b", "c", "d"], &["y"], vec![gate(kind, "y", &["a", "b", "c", "d"][..kind.arity()])]);

            assert_eq!(
                assign_polarities(&source),
                Err(PolarityError::MissingDefaultLibraryEntry { kind }),
                "{kind:?}"
            );
        }
    }

    /// This `p` rail fans out into two gates that reconverge at `y`.  The
    /// selected negative rail saves one shared inverse exactly once; scoring
    /// a gate in isolation would charge that inverse to each consumer.
    #[test]
    fn assignment_scores_shared_inverters_across_reconvergent_fanout() {
        let source = netlist(
            &["a", "b", "c"],
            &["y"],
            vec![
                gate(GateKind::Buf, "p", &["a"]),
                gate(GateKind::And, "left", &["p", "b"]),
                gate(GateKind::And, "right", &["p", "c"]),
                gate(GateKind::Or(2), "y", &["left", "right"]),
            ],
        );

        let assignment = assign_polarities(&source).unwrap();
        assert_eq!(
            assignment,
            vec![
                SignalPolarity::Negative,
                SignalPolarity::Positive,
                SignalPolarity::Positive,
                SignalPolarity::Positive,
            ]
        );
        assert_eq!(
            score(&source, &assignment),
            Ok(LoweredScore { area: 42, gates: 6, torch_depth: 2 })
        );

        let lowered = lower_with_assignment(&source, &assignment).unwrap();
        assert_eq!(lowered.gates.len(), 6);
        assert_eq!(
            lowered
                .gates
                .iter()
                .filter(|gate| gate.kind == GateKind::Nor(1) && gate.inputs == ["a"])
                .count(),
            1,
            "the shared inverse of `a` is lowered once"
        );
    }

    /// These are real lowered netlists, so the comparison covers the scorer
    /// that reads default-library area, lowered gate count, and DAG depth.
    #[test]
    fn lowered_score_orders_area_then_gates_then_torch_depth() {
        let lower_area = netlist(
            &["a", "b"],
            &["m0", "m1", "m2", "m3", "m4", "m5"],
            vec![
                gate(GateKind::Or(2), "m0", &["a", "b"]),
                gate(GateKind::Or(2), "m1", &["a", "b"]),
                gate(GateKind::Or(2), "m2", &["a", "b"]),
                gate(GateKind::Or(2), "m3", &["a", "b"]),
                gate(GateKind::Or(2), "m4", &["a", "b"]),
                gate(GateKind::Or(2), "m5", &["a", "b"]),
            ],
        );
        let higher_area = netlist(
            &["a", "b", "c"],
            &["n0", "n1", "n2", "n3"],
            vec![
                gate(GateKind::Nor(3), "n0", &["a", "b", "c"]),
                gate(GateKind::Nor(3), "n1", &["a", "b", "c"]),
                gate(GateKind::Nor(3), "n2", &["a", "b", "c"]),
                gate(GateKind::Nor(3), "n3", &["a", "b", "c"]),
            ],
        );
        let fewer_gates = netlist(
            &["a", "b", "c"],
            &["n"],
            vec![gate(GateKind::Nor(3), "n", &["a", "b", "c"])],
        );
        let more_gates = netlist(
            &["a", "b"],
            &["m0", "m1"],
            vec![gate(GateKind::Or(2), "m0", &["a", "b"]), gate(GateKind::Or(2), "m1", &["a", "b"])],
        );
        let shallow = netlist(
            &["a", "b"],
            &["p", "q"],
            vec![gate(GateKind::Nor(1), "p", &["a"]), gate(GateKind::Nor(1), "q", &["b"])],
        );
        let deep = netlist(
            &["a"],
            &["p", "q"],
            vec![gate(GateKind::Nor(1), "p", &["a"]), gate(GateKind::Nor(1), "q", &["p"])],
        );

        assert_eq!(score_realisable_netlist(&lower_area), Ok(LoweredScore { area: 36, gates: 6, torch_depth: 0 }));
        assert_eq!(score_realisable_netlist(&higher_area), Ok(LoweredScore { area: 48, gates: 4, torch_depth: 1 }));
        assert!(score_realisable_netlist(&lower_area).unwrap() < score_realisable_netlist(&higher_area).unwrap());

        assert_eq!(score_realisable_netlist(&fewer_gates), Ok(LoweredScore { area: 12, gates: 1, torch_depth: 1 }));
        assert_eq!(score_realisable_netlist(&more_gates), Ok(LoweredScore { area: 12, gates: 2, torch_depth: 0 }));
        assert!(score_realisable_netlist(&fewer_gates).unwrap() < score_realisable_netlist(&more_gates).unwrap());

        assert_eq!(score_realisable_netlist(&shallow), Ok(LoweredScore { area: 12, gates: 2, torch_depth: 1 }));
        assert_eq!(score_realisable_netlist(&deep), Ok(LoweredScore { area: 12, gates: 2, torch_depth: 2 }));
        assert!(score_realisable_netlist(&shallow).unwrap() < score_realisable_netlist(&deep).unwrap());
    }

    /// Directly realisable gates have no alternate output rail. They must be
    /// included in the returned vector but excluded from every candidate flip.
    #[test]
    fn assignment_never_flips_direct_nor_or_gates() {
        let source = netlist(
            &["a", "b"],
            &["y"],
            vec![gate(GateKind::Nor(1), "not_a", &["a"]), gate(GateKind::Or(2), "y", &["not_a", "b"])],
        );

        assert_eq!(
            assign_polarities(&source),
            Ok(vec![SignalPolarity::Positive, SignalPolarity::Positive])
        );
    }

    /// Each negative buffer can donate one cached primary-input inverse to
    /// the NAND. Flipping only one buffer leaves the same lexicographic
    /// score, but flipping both together removes one torch level.
    #[test]
    fn assignment_escapes_a_single_flip_local_minimum_with_a_pair_flip() {
        let source = netlist(
            &["b", "c"],
            &["y"],
            vec![
                gate(GateKind::Buf, "b_cache", &["b"]),
                gate(GateKind::Buf, "c_cache", &["c"]),
                gate(GateKind::Nand, "y", &["b", "c"]),
            ],
        );
        let all_positive = vec![SignalPolarity::Positive; 3];
        let only_b = vec![SignalPolarity::Negative, SignalPolarity::Positive, SignalPolarity::Positive];
        let only_c = vec![SignalPolarity::Positive, SignalPolarity::Negative, SignalPolarity::Positive];
        let pair = vec![SignalPolarity::Negative, SignalPolarity::Negative, SignalPolarity::Positive];

        assert_eq!(score(&source, &all_positive), Ok(LoweredScore { area: 30, gates: 5, torch_depth: 2 }));
        assert_eq!(score(&source, &only_b), score(&source, &all_positive));
        assert_eq!(score(&source, &only_c), score(&source, &all_positive));
        assert_eq!(score(&source, &pair), Ok(LoweredScore { area: 30, gates: 5, torch_depth: 1 }));
        assert_eq!(assign_polarities(&source), Ok(pair));
    }
}

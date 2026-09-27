//! Read-only reports over the lowered netlist and the world that actually shipped.
//! Timing is present only when a matching fragment-synthesis certificate is supplied.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use super::fragment_synth::benchmark::canonical_world_fingerprint;
use super::fragment_synth::certification::CandidateMetrics;
use super::fragment_synth::manifest::TransitionManifest;
use super::fragment_synth::SynthesisResult;
use super::metrics::{physical_metrics, PhysicalMetrics, Ratio};
use super::{CompiledCircuit, Netlist};
use crate::redstone::world::block::BlockKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CircuitEvaluation {
    pub schema_version: u32,
    pub logical: LogicalMetrics,
    pub physical: PhysicalEvaluation,
    /// Delays use simulator game ticks, not redstone ticks or wall-clock time.
    pub timing: Option<TimingMetrics>,
    pub confidence: EvaluationConfidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogicalMetrics {
    pub input_count: u64,
    pub output_count: u64,
    /// All counts/depths describe the lowered netlist, including wire merges.
    pub lowered_gate_count: u64,
    pub dff_count: u64,
    /// Longest combinational gate chain, with primary inputs and DFF Q at depth 0.
    /// None for a combinational cycle or an unresolved signal.
    pub combinational_depth: Option<u64>,
    /// Gate input pins only, including DFF data/clock pins; excludes output ports.
    pub max_gate_input_fanout: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalEvaluation {
    #[serde(flatten)]
    pub metrics: PhysicalMetrics,
    /// Non-air bounding-box extents in x/y/z order; zero for an empty world.
    pub occupied_size: [u64; 3],
    /// Non-air blocks / occupied volume; undefined for an empty world.
    pub density: Option<Ratio>,
    pub block_counts: Vec<BlockCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockCount {
    pub kind: BlockKind,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimingMetrics {
    /// Maximum over the certificate's transition manifest, not a universal bound.
    /// None when that manifest contains no transitions.
    pub observed_max_settle_game_ticks: Option<u64>,
    pub static_routed_delay_game_ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EvaluationConfidence {
    /// Physical/logical counts only. This report did not run functional simulation.
    NotMeasured,
    /// Successful existing certification; its limits and identity travel with the report.
    Certified {
        transition_coverage: TransitionCoverage,
        metrics: CandidateMetrics,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionCoverage {
    ExhaustiveDistinctInputPairs,
    FixedV1Sampled,
}

impl CircuitEvaluation {
    /// `lowered` must be the netlist used to create `compiled`.
    /// This does not compile, simulate, or certify the circuit again.
    pub fn from_compiled(lowered: &Netlist, compiled: &CompiledCircuit) -> Self {
        let metrics = physical_metrics(&compiled.world, lowered.gates.len() as u64);
        let occupied_size =
            metrics
                .occupied_min
                .zip(metrics.occupied_max)
                .map_or([0; 3], |(min, max)| {
                    [
                        (i64::from(max.x) - i64::from(min.x) + 1) as u64,
                        (i64::from(max.y) - i64::from(min.y) + 1) as u64,
                        (i64::from(max.z) - i64::from(min.z) + 1) as u64,
                    ]
                });
        let density = (metrics.occupied_volume > 0)
            .then(|| Ratio::new(metrics.non_air_blocks, metrics.occupied_volume));
        // First occurrence in world cell order is deterministic, independent of palette IDs.
        let mut block_counts: Vec<BlockCount> = Vec::new();
        for &index in compiled.world.cells() {
            let kind = compiled
                .world
                .palette()
                .get(index)
                .expect("a world cell must reference an existing palette entry")
                .kind;
            if kind != BlockKind::Air {
                if let Some(entry) = block_counts.iter_mut().find(|entry| entry.kind == kind) {
                    entry.count += 1;
                } else {
                    block_counts.push(BlockCount { kind, count: 1 });
                }
            }
        }
        Self {
            schema_version: 1,
            logical: logical_metrics(lowered),
            physical: PhysicalEvaluation {
                metrics,
                occupied_size,
                density,
                block_counts,
            },
            timing: None,
            confidence: EvaluationConfidence::NotMeasured,
        }
    }

    /// `lowered` must be the synthesis input. Refuses stale/mismatched public result fields.
    /// Reuses the existing QualityKey without changing generator selection semantics.
    pub fn from_synthesis(lowered: &Netlist, result: &SynthesisResult) -> Result<Self, String> {
        if !describes(lowered, &result.compiled) {
            return Err("evaluation netlist is not the circuit this result compiled".into());
        }
        let mut report = Self::from_compiled(lowered, &result.compiled);
        let metrics = &result.metrics;
        let manifest = TransitionManifest::new(lowered.inputs.clone());
        if metrics.emitted_world_fingerprint != canonical_world_fingerprint(&result.compiled.world)
            || metrics.candidate_fingerprint != result.candidate_fingerprint
            || metrics.quality.non_air_blocks != report.physical.metrics.non_air_blocks
            || metrics.quality.occupied_volume != report.physical.metrics.occupied_volume
            || metrics.transition_manifest_hash != manifest.fingerprint()
            || metrics.transition_count != manifest.transitions().len() as u64
        {
            return Err("evaluation certificate does not match the synthesis result/input".into());
        }
        report.timing = Some(TimingMetrics {
            observed_max_settle_game_ticks: (metrics.transition_count > 0)
                .then_some(metrics.quality.observed_settle),
            static_routed_delay_game_ticks: metrics.quality.static_routed_delay.0,
        });
        report.confidence = EvaluationConfidence::Certified {
            transition_coverage: transition_coverage(&manifest),
            metrics: metrics.clone(),
        };
        Ok(report)
    }
}

/// Whether `compiled` is the circuit `lowered` describes: every port and gate
/// output placed, and one recorded facing per gate. The fingerprint checks in
/// [`CircuitEvaluation::from_synthesis`] bind the certificate to the *world*;
/// they say nothing about the netlist, which reaches the report as its own
/// argument and supplies every logical count in it.
fn describes(lowered: &Netlist, compiled: &CompiledCircuit) -> bool {
    compiled.gate_facings.len() == lowered.gates.len()
        && (lowered.inputs.iter()).all(|name| compiled.input_positions.contains_key(name))
        && (lowered.outputs.iter()).all(|name| compiled.output_positions.contains_key(name))
        && (lowered.gates.iter())
            .all(|gate| compiled.gate_output_positions.contains_key(&gate.output))
}

/// Read coverage off the manifest that was measured, rather than restating the
/// rule [`TransitionManifest`] applies: exhaustive exactly when the manifest
/// holds every ordered pair of distinct input vectors.
fn transition_coverage(manifest: &TransitionManifest) -> TransitionCoverage {
    let ordered_distinct_pairs = u32::try_from(manifest.input_ports().len())
        .ok()
        .and_then(|width| 1u64.checked_shl(width))
        .and_then(|states| states.checked_mul(states - 1));
    match ordered_distinct_pairs {
        Some(pairs) if pairs == manifest.transitions().len() as u64 => {
            TransitionCoverage::ExhaustiveDistinctInputPairs
        }
        _ => TransitionCoverage::FixedV1Sampled,
    }
}

fn logical_metrics(netlist: &Netlist) -> LogicalMetrics {
    let mut fanout = BTreeMap::<&str, u64>::new();
    for input in netlist.gates.iter().flat_map(|gate| &gate.inputs) {
        *fanout.entry(input).or_default() += 1;
    }
    let combinational_depth = netlist.combinational_order().and_then(|order| {
        let mut depths: BTreeMap<&str, u64> =
            netlist.inputs.iter().map(|s| (s.as_str(), 0)).collect();
        let mut maximum = 0;
        for index in order {
            let gate = &netlist.gates[index];
            let depth = if gate.kind.is_sequential() {
                0
            } else {
                let mut input_depth = 0;
                for input in &gate.inputs {
                    input_depth = input_depth.max(*depths.get(input.as_str())?);
                }
                input_depth + 1
            };
            maximum = maximum.max(depth);
            depths.insert(&gate.output, depth);
        }
        Some(maximum)
    });
    LogicalMetrics {
        input_count: netlist.inputs.len() as u64,
        output_count: netlist.outputs.len() as u64,
        lowered_gate_count: netlist.gates.len() as u64,
        dff_count: netlist
            .gates
            .iter()
            .filter(|gate| gate.kind.is_sequential())
            .count() as u64,
        combinational_depth,
        max_gate_input_fanout: fanout.values().copied().max().unwrap_or(0),
    }
}

impl fmt::Display for CircuitEvaluation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Circuit evaluation (schema {})", self.schema_version)?;
        writeln!(
            f,
            "Logical: {} inputs, {} outputs, {} lowered gates, {} DFFs",
            self.logical.input_count,
            self.logical.output_count,
            self.logical.lowered_gate_count,
            self.logical.dff_count
        )?;
        writeln!(
            f,
            "Combinational depth: {}; max gate-input fanout: {}",
            self.logical
                .combinational_depth
                .map_or_else(|| "unavailable".into(), |v| v.to_string()),
            self.logical.max_gate_input_fanout
        )?;
        let [x, y, z] = self.physical.occupied_size;
        writeln!(
            f,
            "Physical: {} non-air blocks; occupied {} x {} x {}; volume {} blocks^3",
            self.physical.metrics.non_air_blocks, x, y, z, self.physical.metrics.occupied_volume
        )?;
        if let Some(density) = self.physical.density {
            writeln!(f, "Density: {}/{}", density.numerator, density.denominator)?;
        }
        for entry in &self.physical.block_counts {
            writeln!(f, "  {:?}: {}", entry.kind, entry.count)?;
        }
        match &self.confidence {
            EvaluationConfidence::NotMeasured => writeln!(
                f,
                "Timing/functional certification: not measured by this report"
            ),
            EvaluationConfidence::Certified {
                transition_coverage,
                metrics,
            } => {
                writeln!(
                    f,
                    "Certification: {:?}; {} transitions (cap {}); worst indices {:?}",
                    transition_coverage,
                    metrics.transition_count,
                    metrics.transition_cap,
                    metrics.worst_transition_indices
                )?;
                if let Some(timing) = &self.timing {
                    writeln!(f, "Observed max settle: {} game ticks (manifest only); static routed delay: {} game ticks",
                        timing.observed_max_settle_game_ticks.map_or_else(|| "not measured".into(), |v| v.to_string()),
                        timing.static_routed_delay_game_ticks)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::{compile_fragment_synth, SynthesisBudget, SynthesisInput};
    use crate::compile::{topology::GateKind, Gate};

    #[test]
    fn sequential_feedback_breaks_depth_and_fanout_counts_pins() {
        let netlist = Netlist {
            inputs: vec!["clock".into()],
            outputs: vec!["q".into()],
            gates: vec![
                Gate::nor("d", &["q", "q"]),
                Gate {
                    name: "ff".into(),
                    inputs: vec!["d".into(), "clock".into()],
                    output: "q".into(),
                    kind: GateKind::DffPosedge,
                },
            ],
        };
        let metrics = logical_metrics(&netlist);
        assert_eq!(metrics.combinational_depth, Some(1));
        assert_eq!(metrics.dff_count, 1);
        assert_eq!(metrics.max_gate_input_fanout, 2);
        // Two combinational stages inside the same feedback loop: the depth is
        // the span between clock boundaries, not the unbounded walk around it.
        let deeper = Netlist {
            gates: vec![
                Gate::nor("d0", &["q", "clock"]),
                Gate::nor("d", &["d0"]),
                netlist.gates[1].clone(),
            ],
            ..netlist.clone()
        };
        assert_eq!(logical_metrics(&deeper).combinational_depth, Some(2));

        let cyclic = Netlist {
            inputs: vec![],
            outputs: vec!["x".into()],
            gates: vec![Gate::nor("x", &["x"])],
        };
        assert_eq!(logical_metrics(&cyclic).combinational_depth, None);
    }

    #[test]
    fn compiled_reports_serialize_counts_and_do_not_invent_timing() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let compiled = crate::compile::compile(&netlist).unwrap();
        let report = CircuitEvaluation::from_compiled(&netlist, &compiled);
        assert_eq!(report.logical.combinational_depth, Some(1));
        assert_eq!(
            report
                .physical
                .block_counts
                .iter()
                .map(|c| c.count)
                .sum::<u64>(),
            report.physical.metrics.non_air_blocks
        );
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["confidence"]["status"], "not_measured");
        assert!(json["timing"].is_null());
        assert!(report.to_string().contains("not measured"));
    }

    #[test]
    fn certified_synthesis_reuses_generator_metrics_and_names_coverage() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let result = compile_fragment_synth(
            SynthesisInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SynthesisBudget::Evaluations(0),
        )
        .expect("a NOT gate receives a certified circuit");

        let report = CircuitEvaluation::from_synthesis(&netlist, &result)
            .expect("the result and certificate match");
        assert_eq!(
            report.timing,
            Some(TimingMetrics {
                observed_max_settle_game_ticks: Some(result.metrics.quality.observed_settle),
                static_routed_delay_game_ticks: result.metrics.quality.static_routed_delay.0,
            })
        );
        assert!(matches!(
            report.confidence,
            EvaluationConfidence::Certified {
                transition_coverage: TransitionCoverage::ExhaustiveDistinctInputPairs,
                ..
            }
        ));

        // Same ports, so every fingerprint in the certificate still matches;
        // only the netlist's own shape says this is a different circuit.
        let impostor = Netlist {
            gates: vec![Gate::nor("y", &["a"]), Gate::nor("z", &["y"])],
            ..netlist.clone()
        };
        assert!(CircuitEvaluation::from_synthesis(&impostor, &result).is_err());
    }

    #[test]
    fn coverage_follows_the_manifest_rather_than_a_restated_input_threshold() {
        let ports = |count: usize| (0..count).map(|i| format!("i{i}")).collect::<Vec<_>>();
        for count in 0..=4 {
            let manifest = TransitionManifest::new(ports(count));
            let states = 1u64 << count;
            assert_eq!(manifest.transitions().len() as u64, states * (states - 1));
            assert_eq!(
                transition_coverage(&manifest),
                TransitionCoverage::ExhaustiveDistinctInputPairs
            );
        }
        for count in [5, 12] {
            assert_eq!(
                transition_coverage(&TransitionManifest::new(ports(count))),
                TransitionCoverage::FixedV1Sampled
            );
        }
        // A width whose pair count does not fit a u64 must not wrap around
        // into "exhaustive": the manifest is sparse there and says so.
        assert_eq!(
            transition_coverage(&TransitionManifest::new(ports(64))),
            TransitionCoverage::FixedV1Sampled
        );
    }
}

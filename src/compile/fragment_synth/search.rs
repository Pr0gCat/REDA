use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::compile::fragment_synth::certification::CertifiedCandidate;
use crate::compile::fragment_synth::certification::QualityKey;
#[cfg(test)]
use crate::compile::metrics::canonical_fingerprint;
use crate::compile::metrics::Fingerprint;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthesisBudget {
    Evaluations(u64),
    Time(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    EvaluationBudget,
    TimeBudget,
    ProposalStreamExhausted,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapWorkCounters {
    pub router_expansions: u64,
    pub backtracks: u64,
    pub proof_steps: u64,
    pub verification_checks: u64,
    pub certification_transitions: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalTerminal {
    Refused,
    RouterCapExhausted,
    BacktrackCapExhausted,
    ProofCapExhausted,
    VerificationFailed,
    CertificationCapExhausted,
    NoImprovement,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProposalTrace {
    pub proposal_index: u64,
    pub parent_fingerprint: Fingerprint,
    pub fragment_fingerprint: Fingerprint,
    pub choice_fingerprint: Fingerprint,
    pub terminal: ProposalTerminal,
    pub cap_work: CapWorkCounters,
    pub certified_quality: Option<QualityKey>,
    pub accepted: bool,
}

// Everything below that drives a *proposal loop* -- the candidate trait, the
// clock, the stream, the summary and `run_budgeted_proposals` itself -- is
// test-only. The one shipping producer, the recursive contract, has no
// proposal loop; the loop survives for the legacy whole-circuit seed's unit
// tests. The public trace and budget types above stay in production because
// `SynthesisResult` reports them.
#[cfg(test)]
pub(crate) trait SearchCandidate {
    fn candidate_fingerprint(&self) -> &Fingerprint;
    fn quality(&self) -> QualityKey;
}

#[cfg(test)]
impl SearchCandidate for CertifiedCandidate {
    fn candidate_fingerprint(&self) -> &Fingerprint {
        &self.metrics().candidate_fingerprint
    }

    fn quality(&self) -> QualityKey {
        self.metrics().quality
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchSeed {
    pub candidate_fingerprint: Fingerprint,
    pub quality: QualityKey,
}

#[cfg(test)]
impl SearchCandidate for SearchSeed {
    fn candidate_fingerprint(&self) -> &Fingerprint {
        &self.candidate_fingerprint
    }

    fn quality(&self) -> QualityKey {
        self.quality
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct SearchSummary<T> {
    pub best: T,
    pub trace: Vec<ProposalTrace>,
    pub evaluations_used: u64,
    pub stop_reason: StopReason,
}

#[cfg(test)]
pub(crate) trait MonotonicClock {
    fn elapsed(&self) -> Duration;
}

#[cfg(test)]
pub(crate) struct ProposalEvaluation<T> {
    pub fragment_fingerprint: Fingerprint,
    pub choice_fingerprint: Fingerprint,
    pub terminal: ProposalTerminal,
    pub cap_work: CapWorkCounters,
    pub certified: Option<T>,
}

#[cfg(test)]
impl<T: SearchCandidate> ProposalEvaluation<T> {
    fn certified(candidate: T) -> Self {
        let fragment_fingerprint = canonical_fingerprint(
            format!(
                "certified-fragment:{}",
                candidate.candidate_fingerprint().as_str()
            )
            .as_bytes(),
        );
        let choice_fingerprint = canonical_fingerprint(
            format!(
                "certified-choice:{}",
                candidate.candidate_fingerprint().as_str()
            )
            .as_bytes(),
        );
        Self {
            fragment_fingerprint,
            choice_fingerprint,
            terminal: ProposalTerminal::NoImprovement,
            cap_work: CapWorkCounters::default(),
            certified: Some(candidate),
        }
    }
}

#[cfg(test)]
pub(crate) trait ProposalStream<T: SearchCandidate> {
    fn next(&mut self, proposal_index: u64, incumbent: &T) -> Option<ProposalEvaluation<T>>;
}

#[cfg(test)]
pub(crate) struct SystemMonotonicClock {
    started: Instant,
}

#[cfg(test)]
impl SystemMonotonicClock {
    pub(crate) fn start() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

#[cfg(test)]
impl MonotonicClock for SystemMonotonicClock {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

#[cfg(test)]
pub(crate) struct DeterministicNoOpProposalStream {
    case_fingerprint: Fingerprint,
    remaining: u64,
}

#[cfg(test)]
impl DeterministicNoOpProposalStream {
    pub(crate) fn new(case_fingerprint: Fingerprint, proposal_count: u64) -> Self {
        Self {
            case_fingerprint,
            remaining: proposal_count,
        }
    }
}

#[cfg(test)]
impl<T: SearchCandidate> ProposalStream<T> for DeterministicNoOpProposalStream {
    fn next(&mut self, proposal_index: u64, incumbent: &T) -> Option<ProposalEvaluation<T>> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let fragment_fingerprint = canonical_fingerprint(
            format!(
                "noop-fragment-v1:{}:{proposal_index}",
                self.case_fingerprint.as_str()
            )
            .as_bytes(),
        );
        let choice_fingerprint = canonical_fingerprint(
            format!(
                "noop-choice-v1:{}:{proposal_index}:{}",
                self.case_fingerprint.as_str(),
                incumbent.candidate_fingerprint().as_str()
            )
            .as_bytes(),
        );
        Some(ProposalEvaluation {
            fragment_fingerprint,
            choice_fingerprint,
            terminal: ProposalTerminal::Refused,
            cap_work: CapWorkCounters::default(),
            certified: None,
        })
    }
}

#[cfg(test)]
pub(crate) fn run_budgeted_proposals<T: SearchCandidate>(
    mut best: T,
    budget: SynthesisBudget,
    clock: &dyn MonotonicClock,
    proposals: &mut dyn ProposalStream<T>,
) -> SearchSummary<T> {
    let started = clock.elapsed();
    let mut trace = Vec::new();
    let mut evaluations_used = 0u64;

    let stop_reason = loop {
        match budget {
            SynthesisBudget::Evaluations(limit) if evaluations_used >= limit => {
                break StopReason::EvaluationBudget;
            }
            SynthesisBudget::Time(limit) if clock.elapsed().saturating_sub(started) >= limit => {
                break StopReason::TimeBudget;
            }
            SynthesisBudget::Evaluations(_) | SynthesisBudget::Time(_) => {}
        }

        let parent_fingerprint = best.candidate_fingerprint().clone();
        let Some(mut evaluation) = proposals.next(evaluations_used, &best) else {
            break StopReason::ProposalStreamExhausted;
        };
        evaluations_used = evaluations_used.saturating_add(1);

        let certified_quality = evaluation.certified.as_ref().map(SearchCandidate::quality);
        let accepted = evaluation
            .certified
            .as_ref()
            .is_some_and(|candidate| candidate.quality() < best.quality());
        if let Some(candidate) = evaluation.certified.take() {
            if accepted {
                best = candidate;
                evaluation.terminal = ProposalTerminal::Accepted;
            } else {
                evaluation.terminal = ProposalTerminal::NoImprovement;
            }
        }
        trace.push(ProposalTrace {
            proposal_index: evaluations_used - 1,
            parent_fingerprint,
            fragment_fingerprint: evaluation.fragment_fingerprint,
            choice_fingerprint: evaluation.choice_fingerprint,
            terminal: evaluation.terminal,
            cap_work: evaluation.cap_work,
            certified_quality,
            accepted,
        });
    };

    SearchSummary {
        best,
        trace,
        evaluations_used,
        stop_reason,
    }
}

#[cfg(test)]
pub(crate) struct ScriptedProposalStream {
    proposals: std::vec::IntoIter<ProposalEvaluation<SearchSeed>>,
    after_each: Option<Box<dyn FnMut()>>,
}

#[cfg(test)]
impl ScriptedProposalStream {
    pub(crate) fn new(proposals: Vec<ProposalEvaluation<SearchSeed>>) -> Self {
        Self {
            proposals: proposals.into_iter(),
            after_each: None,
        }
    }

    #[cfg(test)]
    fn with_after_each(
        proposals: Vec<ProposalEvaluation<SearchSeed>>,
        after_each: impl FnMut() + 'static,
    ) -> Self {
        Self {
            proposals: proposals.into_iter(),
            after_each: Some(Box::new(after_each)),
        }
    }
}

#[cfg(test)]
impl ProposalStream<SearchSeed> for ScriptedProposalStream {
    fn next(
        &mut self,
        _proposal_index: u64,
        _incumbent: &SearchSeed,
    ) -> Option<ProposalEvaluation<SearchSeed>> {
        let proposal = self.proposals.next()?;
        if let Some(after_each) = &mut self.after_each {
            after_each();
        }
        Some(proposal)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use super::{
        run_budgeted_proposals, CapWorkCounters, DeterministicNoOpProposalStream, MonotonicClock,
        ProposalEvaluation, ProposalTerminal, ScriptedProposalStream, SearchSeed, StopReason,
        SynthesisBudget,
    };
    use crate::compile::fragment_synth::certification::QualityKey;
    use crate::compile::fragment_synth::timing_graph::ExactDelay;
    use crate::compile::metrics::canonical_fingerprint;

    fn quality(settle: u64, blocks: u64) -> QualityKey {
        QualityKey {
            observed_settle: settle,
            non_air_blocks: blocks,
            occupied_volume: blocks * 10,
            static_routed_delay: ExactDelay(settle),
        }
    }

    fn seed() -> SearchSeed {
        SearchSeed {
            candidate_fingerprint: canonical_fingerprint(b"certified-seed"),
            quality: quality(8, 80),
        }
    }

    fn outcomes(count: usize) -> Vec<ProposalEvaluation<SearchSeed>> {
        (0..count)
            .map(|index| ProposalEvaluation {
                fragment_fingerprint: canonical_fingerprint(format!("fragment-{index}").as_bytes()),
                choice_fingerprint: canonical_fingerprint(format!("choice-{index}").as_bytes()),
                terminal: ProposalTerminal::Refused,
                cap_work: CapWorkCounters {
                    router_expansions: index as u64,
                    ..CapWorkCounters::default()
                },
                certified: None,
            })
            .collect()
    }

    #[derive(Clone)]
    struct FakeClock {
        now_nanos: Rc<Cell<u64>>,
    }

    impl MonotonicClock for FakeClock {
        fn elapsed(&self) -> Duration {
            Duration::from_nanos(self.now_nanos.get())
        }
    }

    #[test]
    fn evaluation_budgets_are_exact_trace_prefixes_and_keep_the_seed() {
        let complete = run_budgeted_proposals(
            seed(),
            SynthesisBudget::Evaluations(8),
            &FakeClock {
                now_nanos: Rc::new(Cell::new(0)),
            },
            &mut ScriptedProposalStream::new(outcomes(8)),
        );

        for budget in [0, 1, 2, 4, 8] {
            let result = run_budgeted_proposals(
                seed(),
                SynthesisBudget::Evaluations(budget),
                &FakeClock {
                    now_nanos: Rc::new(Cell::new(0)),
                },
                &mut ScriptedProposalStream::new(outcomes(8)),
            );
            assert_eq!(result.evaluations_used, budget);
            assert_eq!(result.trace, complete.trace[..budget as usize]);
            assert_eq!(result.best, seed());
            assert_eq!(result.stop_reason, StopReason::EvaluationBudget);
        }
    }

    #[test]
    fn production_noop_enumeration_is_finite_and_reports_stream_exhaustion() {
        let result = run_budgeted_proposals(
            seed(),
            SynthesisBudget::Evaluations(9),
            &FakeClock {
                now_nanos: Rc::new(Cell::new(0)),
            },
            &mut DeterministicNoOpProposalStream::new(canonical_fingerprint(b"case"), 8),
        );

        assert_eq!(result.evaluations_used, 8);
        assert_eq!(result.trace.len(), 8);
        assert_eq!(result.stop_reason, StopReason::ProposalStreamExhausted);
    }

    #[test]
    fn every_refusal_is_terminal_and_cannot_erase_the_certified_best() {
        let terminals = [
            ProposalTerminal::Refused,
            ProposalTerminal::RouterCapExhausted,
            ProposalTerminal::BacktrackCapExhausted,
            ProposalTerminal::ProofCapExhausted,
            ProposalTerminal::VerificationFailed,
            ProposalTerminal::CertificationCapExhausted,
        ];
        let scripted = terminals
            .iter()
            .enumerate()
            .map(|(index, terminal)| ProposalEvaluation {
                fragment_fingerprint: canonical_fingerprint(format!("fragment-{index}").as_bytes()),
                choice_fingerprint: canonical_fingerprint(format!("choice-{index}").as_bytes()),
                terminal: *terminal,
                cap_work: CapWorkCounters::default(),
                certified: None,
            })
            .collect();
        let result = run_budgeted_proposals(
            seed(),
            SynthesisBudget::Evaluations(terminals.len() as u64),
            &FakeClock {
                now_nanos: Rc::new(Cell::new(0)),
            },
            &mut ScriptedProposalStream::new(scripted),
        );

        assert_eq!(result.best, seed());
        assert_eq!(result.trace.len(), terminals.len());
        assert_eq!(
            result
                .trace
                .iter()
                .map(|trace| trace.terminal)
                .collect::<Vec<_>>(),
            terminals
        );
        assert!(result.trace.iter().all(|trace| !trace.accepted));
    }

    #[test]
    fn a_time_budget_stops_only_after_the_crossing_proposal_finishes() {
        let now_nanos = Rc::new(Cell::new(0));
        let clock = FakeClock {
            now_nanos: now_nanos.clone(),
        };
        let mut timed = ScriptedProposalStream::with_after_each(outcomes(8), move || {
            now_nanos.set(now_nanos.get() + 10);
        });
        let time_result = run_budgeted_proposals(
            seed(),
            SynthesisBudget::Time(Duration::from_nanos(35)),
            &clock,
            &mut timed,
        );
        let evaluation_result = run_budgeted_proposals(
            seed(),
            SynthesisBudget::Evaluations(4),
            &FakeClock {
                now_nanos: Rc::new(Cell::new(0)),
            },
            &mut ScriptedProposalStream::new(outcomes(8)),
        );

        assert_eq!(time_result.stop_reason, StopReason::TimeBudget);
        assert_eq!(time_result.evaluations_used, 4);
        assert_eq!(time_result.trace, evaluation_result.trace);
        assert_eq!(time_result.best, evaluation_result.best);
    }

    #[test]
    fn only_a_strictly_better_certified_proposal_replaces_the_best() {
        let worse = SearchSeed {
            candidate_fingerprint: canonical_fingerprint(b"worse"),
            quality: quality(9, 20),
        };
        let better = SearchSeed {
            candidate_fingerprint: canonical_fingerprint(b"better"),
            quality: quality(7, 100),
        };
        let scripted = vec![
            ProposalEvaluation::certified(worse),
            ProposalEvaluation::certified(better.clone()),
        ];
        let result = run_budgeted_proposals(
            seed(),
            SynthesisBudget::Evaluations(2),
            &FakeClock {
                now_nanos: Rc::new(Cell::new(0)),
            },
            &mut ScriptedProposalStream::new(scripted),
        );

        assert_eq!(result.best, better);
        assert_eq!(
            result
                .trace
                .iter()
                .map(|trace| trace.accepted)
                .collect::<Vec<_>>(),
            [false, true]
        );
    }
}

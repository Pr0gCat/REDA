//! Deterministic parallel synthesis of every child in an [`AllocationPlan`].
//!
//! Workers are scoped `std` threads on a fixed stride over the plan's child
//! order; every result is filed by canonical index and only read after all
//! workers have joined, so the output -- and which failure is reported -- is
//! the same whatever the completion timing.

// Crate-private until the public synthesis API unfreezes at Gate 3.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::thread;

use thiserror::Error;

use crate::compile::fragment_synth::allocation::{AllocationPlan, ChildAllocation};
use crate::compile::fragment_synth::leaf::{synthesise_leaf, LeafArtifact, LeafError};
use crate::compile::fragment_synth::partition::{Chunk, ChunkId};

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ScheduleError {
    #[error("at least one worker is needed")]
    ZeroWorkers,
    #[error("chunk {chunk:?} appears more than once")]
    DuplicateChunk { chunk: ChunkId },
    #[error("the plan allocates chunk {chunk:?} but no such chunk was given")]
    MissingChunk { chunk: ChunkId },
    #[error("chunk {chunk:?} has no allocation in the plan")]
    UnexpectedChunk { chunk: ChunkId },
    #[error("child {index} ({chunk:?}) failed: {error}")]
    Leaf {
        index: usize,
        chunk: ChunkId,
        #[source]
        error: LeafError,
    },
    #[error("child {index} ({chunk:?}) panicked during synthesis")]
    Panicked { index: usize, chunk: ChunkId },
}

/// Compile every child of `plan`, returning artifacts in
/// `plan.children` order.  On failure, the error of the lowest-index child.
pub fn synthesise_children(
    chunks: &[Chunk],
    plan: &AllocationPlan,
    workers: usize,
) -> Result<Vec<LeafArtifact>, ScheduleError> {
    if workers == 0 {
        return Err(ScheduleError::ZeroWorkers);
    }
    let mut by_id: BTreeMap<&ChunkId, &Chunk> = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for chunk in chunks {
        if by_id.insert(&chunk.id, chunk).is_some() {
            duplicates.insert(chunk.id.clone());
        }
    }
    if let Some(chunk) = duplicates.into_iter().next() {
        return Err(ScheduleError::DuplicateChunk { chunk });
    }
    let jobs: Vec<(&Chunk, &ChildAllocation)> = plan
        .children
        .iter()
        .map(|child| {
            by_id
                .remove(&child.chunk)
                .map(|chunk| (chunk, child))
                .ok_or_else(|| ScheduleError::MissingChunk {
                    chunk: child.chunk.clone(),
                })
        })
        .collect::<Result<_, _>>()?;
    if let Some(chunk) = by_id.keys().next() {
        return Err(ScheduleError::UnexpectedChunk {
            chunk: (*chunk).clone(),
        });
    }

    let workers = workers.min(jobs.len());
    let mut slots: Vec<Option<Result<LeafArtifact, ScheduleError>>> = Vec::new();
    slots.resize_with(jobs.len(), || None);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let jobs = &jobs;
                scope.spawn(move || {
                    (worker..jobs.len())
                        .step_by(workers)
                        .map(|index| {
                            let (chunk, allocation) = jobs[index];
                            let outcome = catch_unwind(AssertUnwindSafe(|| {
                                synthesise_leaf(chunk, allocation)
                            }));
                            let result = match outcome {
                                Ok(Ok(artifact)) => Ok(artifact),
                                Ok(Err(error)) => Err(ScheduleError::Leaf {
                                    index,
                                    chunk: chunk.id.clone(),
                                    error,
                                }),
                                Err(_) => Err(ScheduleError::Panicked {
                                    index,
                                    chunk: chunk.id.clone(),
                                }),
                            };
                            (index, result)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for handle in handles {
            // A worker only panics outside `catch_unwind`, which cannot happen
            // by construction; a join failure here is a bug, not a child fault.
            for (index, result) in handle.join().expect("worker completed") {
                slots[index] = Some(result);
            }
        }
    });

    slots
        .into_iter()
        .map(|slot| slot.expect("every job was filed"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::allocation::{allocate, AllocationLimits};
    use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;
    use crate::compile::fragment_synth::partition::{partition, root_chunk_id};
    use crate::compile::metrics::Fingerprint;
    use crate::compile::{Gate, Netlist};

    const LIMITS: AllocationLimits = AllocationLimits {
        delay_budget_ticks: 4,
        corridor_capacity: 8,
    };

    fn netlist(inputs: &[&str], outputs: &[&str], gates: Vec<Gate>) -> Netlist {
        Netlist {
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
            gates,
        }
    }

    fn chain() -> Netlist {
        netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        )
    }

    fn fanout() -> Netlist {
        netlist(
            &["x"],
            &["b", "c"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["a"]),
            ],
        )
    }

    fn prepared(net: &Netlist) -> (Vec<Chunk>, AllocationPlan) {
        let chunks = partition(net, &root_chunk_id(net).unwrap(), 1).unwrap();
        let plan = allocate(net, &chunks, LIMITS).unwrap();
        (chunks, plan)
    }

    fn summary(artifacts: &[LeafArtifact]) -> Vec<(ChunkId, Fingerprint)> {
        artifacts
            .iter()
            .map(|a| (a.chunk.clone(), canonical_world_fingerprint(&a.world)))
            .collect()
    }

    #[test]
    fn worker_count_and_slice_order_do_not_change_the_output() {
        for net in [chain(), fanout()] {
            let (chunks, plan) = prepared(&net);
            let serial = synthesise_children(&chunks, &plan, 1).unwrap();
            let expected: Vec<&ChunkId> = plan.children.iter().map(|c| &c.chunk).collect();
            let got: Vec<&ChunkId> = serial.iter().map(|a| &a.chunk).collect();
            assert_eq!(got, expected);

            let parallel = synthesise_children(&chunks, &plan, 4).unwrap();
            assert_eq!(summary(&parallel), summary(&serial));

            let mut reversed = chunks.clone();
            reversed.reverse();
            let shuffled = synthesise_children(&reversed, &plan, 2).unwrap();
            assert_eq!(summary(&shuffled), summary(&serial));
        }
    }

    #[test]
    fn malformed_inputs_are_typed() {
        let (chunks, plan) = prepared(&chain());
        assert_eq!(
            synthesise_children(&chunks, &plan, 0).err(),
            Some(ScheduleError::ZeroWorkers)
        );
        assert_eq!(
            synthesise_children(&chunks[..1], &plan, 2).err(),
            Some(ScheduleError::MissingChunk {
                chunk: plan
                    .children
                    .iter()
                    .map(|c| c.chunk.clone())
                    .find(|id| *id != chunks[0].id)
                    .unwrap()
            })
        );
        let mut doubled = chunks.clone();
        doubled.push(chunks[0].clone());
        assert_eq!(
            synthesise_children(&doubled, &plan, 2).err(),
            Some(ScheduleError::DuplicateChunk {
                chunk: chunks[0].id.clone()
            })
        );
        let mut twice_each = vec![chunks[1].clone(), chunks[0].clone()];
        twice_each.extend([chunks[1].clone(), chunks[0].clone()]);
        assert_eq!(
            synthesise_children(&twice_each, &plan, 2).err(),
            Some(ScheduleError::DuplicateChunk {
                chunk: chunks.iter().map(|c| c.id.clone()).min().unwrap()
            })
        );
        let (extra, _) = prepared(&fanout());
        let mut widened = chunks.clone();
        widened.push(extra[0].clone());
        assert_eq!(
            synthesise_children(&widened, &plan, 2).err(),
            Some(ScheduleError::UnexpectedChunk {
                chunk: extra[0].id.clone()
            })
        );
    }
}

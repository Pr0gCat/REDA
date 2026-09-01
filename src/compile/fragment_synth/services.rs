#![allow(dead_code)] // Task 9 is the first production caller of these Task-8 facades.

//! Sealed durable services used by the independent sparse-seed builder.

use crate::compile::emission::{
    emit_candidate, EmissionError, EmittedWorld, PhysicalCandidateView,
};
use crate::compile::fragment_synth::candidate::ExpandedPhysicalCandidate;
pub(crate) use crate::compile::fragment_synth::placement::TopologyAwareSeedPlacer;
use crate::compile::verification::{verify_expanded_candidate, ExpandedPhysicalError};

pub(crate) trait SeedEmitter {
    fn emit(
        &self,
        candidate: &dyn PhysicalCandidateView,
        size: (i32, i32, i32),
    ) -> Result<EmittedWorld, EmissionError>;
}

pub(crate) trait SeedVerifier {
    fn verify(
        &self,
        candidate: &ExpandedPhysicalCandidate,
        emitted: &EmittedWorld,
    ) -> Result<(), ExpandedPhysicalError>;
}

pub(crate) struct DurableSeedEmitter;

impl SeedEmitter for DurableSeedEmitter {
    fn emit(
        &self,
        candidate: &dyn PhysicalCandidateView,
        size: (i32, i32, i32),
    ) -> Result<EmittedWorld, EmissionError> {
        emit_candidate(candidate, size)
    }
}

pub(crate) struct DurableSeedVerifier;

impl SeedVerifier for DurableSeedVerifier {
    fn verify(
        &self,
        candidate: &ExpandedPhysicalCandidate,
        emitted: &EmittedWorld,
    ) -> Result<(), ExpandedPhysicalError> {
        verify_expanded_candidate(candidate, emitted)
    }
}

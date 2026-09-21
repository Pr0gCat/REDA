mod allocation;
mod api;
pub mod benchmark;
pub mod candidate;
pub mod certification;
pub mod composition;
pub mod config;
mod fragment;
pub mod identity;
pub mod instance_graph;
mod leaf;
pub mod legacy_adapter;
pub mod manifest;
pub(crate) mod partition;
pub(crate) mod placement;
pub mod realise;
pub(crate) mod route_schedule;
mod schedule;
mod search;
pub(crate) mod seed;
pub(crate) mod services;
pub(crate) mod terminal_geometry;
pub mod timing_graph;
pub mod topology;
pub mod verify;

pub use api::{
    compile_fragment_synth, SynthesisCaseFingerprint, SynthesisError, SynthesisInput,
    SynthesisResult,
};
pub use search::{CapWorkCounters, ProposalTerminal, ProposalTrace, StopReason, SynthesisBudget};

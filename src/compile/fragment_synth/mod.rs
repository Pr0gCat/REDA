mod api;
pub mod benchmark;
pub mod candidate;
pub mod certification;
pub mod config;
mod fragment;
pub mod identity;
pub mod instance_graph;
pub mod legacy_adapter;
pub mod manifest;
pub mod realise;
mod search;
pub(crate) mod seed;
pub(crate) mod services;
pub mod timing_graph;
pub mod topology;
pub mod verify;

pub use api::{
    compile_fragment_synth, SynthesisCaseFingerprint, SynthesisError, SynthesisInput,
    SynthesisResult,
};
pub use search::{CapWorkCounters, ProposalTerminal, ProposalTrace, StopReason, SynthesisBudget};

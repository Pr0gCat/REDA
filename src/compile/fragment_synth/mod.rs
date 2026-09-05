mod api;
pub mod benchmark;
pub(crate) mod blocks;
pub(crate) mod channel_layout;
pub(crate) mod channel_plan;
pub mod candidate;
pub mod certification;
pub mod config;
mod fragment;
pub mod identity;
pub mod instance_graph;
pub mod legacy_adapter;
pub mod manifest;
pub(crate) mod placement;
pub mod realise;
pub(crate) mod relocate;
pub(crate) mod route_schedule;
mod search;
pub(crate) mod seed;
pub(crate) mod services;
pub mod timing_graph;
pub mod topology;
pub(crate) mod union;
pub mod verify;

pub use api::{
    compile_fragment_synth, SynthesisCaseFingerprint, SynthesisError, SynthesisInput,
    SynthesisResult,
};
pub use search::{CapWorkCounters, ProposalTerminal, ProposalTrace, StopReason, SynthesisBudget};

//! Per-gate choices of which physical rail lowering should emit.

use crate::compile::topology::SignalPolarity;

/// One requested output polarity for every source gate, in `Netlist::gates`
/// order.
pub type PolarityAssignment = Vec<SignalPolarity>;

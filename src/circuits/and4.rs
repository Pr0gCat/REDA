//! 4-input AND gate: the smallest reference circuit that still needs more
//! than one NOR cell.
//!
//! Built with `NetlistBuilder::and_reduce`, the same fan-in-<=3 NOR-tree
//! expansion the seven-segment decoder uses for its minterms. This is meant
//! to be quick to open in a viewer: a handful of gates and a couple of
//! routing tracks, not a 232x298 grid.

use crate::circuits::netlist_builder::NetlistBuilder;
use crate::compile::Netlist;

pub const INPUT_NAMES: [&str; 4] = ["a", "b", "c", "d"];

/// Display name for and4's one output. and4 has no naturally-named output of
/// its own -- unlike `full_adder`'s `sum`/`cout` or `seven_segment`'s
/// `a`..`g`, which are named for what they mean -- so this is just the
/// conventional single-output label, kept here so every front end (the
/// `build_circuit` binary, the wasm viewer) reports the same name for the
/// same port instead of each hardcoding its own copy.
pub const OUTPUT_NAME: &str = "y";

/// Build the netlist for `out = a AND b AND c AND d`.
///
/// Returns the netlist and the name of its one output signal.
pub fn build_and4_netlist() -> (Netlist, String) {
    let mut builder = NetlistBuilder::new();
    let inputs: Vec<String> = INPUT_NAMES.iter().map(|s| s.to_string()).collect();
    let output = builder.and_reduce(inputs.clone());

    let netlist = Netlist { inputs, outputs: vec![output.clone()], gates: builder.gates };

    (netlist, output)
}

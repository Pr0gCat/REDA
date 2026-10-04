//! Reference circuits built on top of `compile`, ready to run through the
//! compiler as-is.
//!
//! A size ladder, smallest to largest: `and4` (a handful of gates) ->
//! `full_adder` (a few levels, some shared logic) -> `seven_segment`'s
//! `build_single_segment_netlist` (one real slice of the target circuit) ->
//! `seven_segment`'s full decoder. The full decoder is slow to render in a
//! browser-based litematic viewer (232x298 blocks), so the smaller circuits
//! give a fast way to sanity-check a viewer or the compiler itself.
//!
//! `verilog` is the odd one out: not a netlist this project builds itself,
//! but the Verilog sources for the same target functions, synthesized through
//! `crate::frontend`. See that module for why they are a separate catalog
//! with their own `verilog:`-prefixed names.

pub mod and4;
pub mod full_adder;
pub(crate) mod netlist_builder;
pub mod seven_segment;
pub mod verilog;

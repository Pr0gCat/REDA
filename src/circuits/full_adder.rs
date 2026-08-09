//! 1-bit full adder: `sum = a XOR b XOR cin`, `cout = majority(a, b, cin)`.
//!
//! `NetlistBuilder` only offers AND/OR/NOT (via NOR trees), so XOR is built
//! from those: `XOR(x, y) = AND(OR(x, y), NOT(AND(x, y)))`. `AND(a, b)` is
//! computed once and shared between `cout`'s majority function and the first
//! XOR, so the netlist has one gate feeding two different consumers -- a
//! small example of the same kind of gate sharing the decoder relies on for
//! its minterms. The critical path (a/b/cin -> and_ab/or_ab -> xor_ab ->
//! and_xorab_cin/or_xorab_cin -> sum) is several NOR levels deep, enough to
//! exercise levelised placement even at this size.

use std::collections::HashMap;

use crate::circuits::netlist_builder::NetlistBuilder;
use crate::compile::Netlist;

pub const INPUT_NAMES: [&str; 3] = ["a", "b", "cin"];
pub const OUTPUT_NAMES: [&str; 2] = ["sum", "cout"];

/// `XOR(x, y)`, given a precomputed `AND(x, y)` to reuse instead of building
/// a second AND gate for the same pair.
fn xor_with_and(builder: &mut NetlistBuilder, x: &str, y: &str, and_xy: String) -> String {
    let or_xy = builder.or_reduce(vec![x.to_string(), y.to_string()]);
    let not_and_xy = builder.not(&and_xy);
    builder.and_reduce(vec![or_xy, not_and_xy])
}

/// Build the full adder's netlist.
///
/// Returns the netlist and a lookup from output name (`"sum"`, `"cout"`) to
/// that output's actual signal name in `netlist.outputs`.
pub fn build_full_adder_netlist() -> (Netlist, HashMap<&'static str, String>) {
    let mut builder = NetlistBuilder::new();
    let a = INPUT_NAMES[0].to_string();
    let b = INPUT_NAMES[1].to_string();
    let cin = INPUT_NAMES[2].to_string();

    // Shared: and_ab feeds both the carry-out majority function below and
    // the first XOR stage.
    let and_ab = builder.and_reduce(vec![a.clone(), b.clone()]);
    let and_bc = builder.and_reduce(vec![b.clone(), cin.clone()]);
    let and_ac = builder.and_reduce(vec![a.clone(), cin.clone()]);
    let cout = builder.or_reduce(vec![and_ab.clone(), and_bc, and_ac]);

    let xor_ab = xor_with_and(&mut builder, &a, &b, and_ab);
    let and_xorab_cin = builder.and_reduce(vec![xor_ab.clone(), cin.clone()]);
    let sum = xor_with_and(&mut builder, &xor_ab, &cin, and_xorab_cin);

    let mut output_signal: HashMap<&'static str, String> = HashMap::new();
    output_signal.insert("sum", sum.clone());
    output_signal.insert("cout", cout.clone());

    let netlist = Netlist {
        inputs: INPUT_NAMES.iter().map(|s| s.to_string()).collect(),
        outputs: vec![sum, cout],
        gates: builder.into_gates(),
    };

    (netlist, output_signal)
}

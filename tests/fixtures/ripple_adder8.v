// The Verilog source for task 13's hierarchical-synthesis acceptance
// fixture: an 8-bit ripple adder built from eight instances of one
// `full_adder` module, so `reda::frontend::synthesize_verilog_hierarchical`
// has a real module hierarchy to preserve instead of one flat gate soup --
// see `tests/hierarchical_synthesis.rs`.
//
// `(* keep_hierarchy *)` on `full_adder` is deliberate, not decorative: with
// a two-gate module this small, Yosys's `opt`/`techmap` passes are free to
// dissolve every instance into `ripple_adder8`'s own gate list, and a
// flattened design has no hierarchy left for `compile_hierarchical` to
// prove anything about. The attribute pins `full_adder` as a real cell
// boundary that survives synthesis.
(* keep_hierarchy *)
module full_adder(input a, input b, input cin, output sum, output cout);
  assign sum = a ^ b ^ cin;
  assign cout = (a & b) | (b & cin) | (a & cin);
endmodule

module ripple_adder8(input [7:0] a, input [7:0] b, input cin, output [7:0] s, output cout);
  wire [8:0] c;
  assign c[0] = cin;
  genvar i;
  generate
    for (i = 0; i < 8; i = i + 1) begin : bit
      full_adder fa(.a(a[i]), .b(b[i]), .cin(c[i]), .sum(s[i]), .cout(c[i+1]));
    end
  endgenerate
  assign cout = c[8];
endmodule

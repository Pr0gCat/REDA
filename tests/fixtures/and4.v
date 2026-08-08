// The Verilog source for `reda::circuits::and4`'s target function, run
// through the Yosys frontend (`reda::frontend::synthesize_verilog`) as a
// second, independent implementation of the same circuit -- see
// `tests/verilog_frontend.rs`, which checks it against the exact same truth
// table the hand-written `and4` netlist is checked against.
module and4(
    input  a,
    input  b,
    input  c,
    input  d,
    output y
);
    assign y = a & b & c & d;
endmodule

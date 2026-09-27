// The SystemVerilog spelling of `tests/fixtures/and4.v`'s circuit, for the
// REDA compiler rather than for Yosys: ANSI ports, `logic` rather than
// implicit wires, and the same four-input conjunction.
//
// Additive. `and4.v` is unchanged and still feeds the Yosys frontend, the
// baked netlist, and the catalog; this file is compiled by
// `reda::frontend::compile_systemverilog` and checked against both the same
// independent predicate and the baked Yosys result.
module and4(
    input  logic a,
    input  logic b,
    input  logic c,
    input  logic d,
    output logic y
);
    assign y = a & b & c & d;
endmodule

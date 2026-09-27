// The smallest thing the REDA SystemVerilog compiler compiles end to end:
// two inputs, one gate, one output. `tests/systemverilog_compiler.rs` uses
// it to check all four input rows against a plain predicate, and to check
// that the source expression and the gate it became can each find the other.
//
// Additive: this compiles through `reda::frontend::compile_systemverilog`
// with no Python and no Yosys. The `.v` fixtures beside it still belong to
// the Yosys path and are unchanged.
module and2(
    input  logic a,
    input  logic b,
    output logic y
);
    assign y = a & b;
endmodule

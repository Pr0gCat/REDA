// The Verilog source for `reda::circuits::seven_segment`'s target function
// (a BCD-to-seven-segment decoder), run through the Yosys frontend
// (`reda::frontend::synthesize_verilog`) as a second, independent
// implementation of the same circuit -- see `tests/verilog_frontend.rs`,
// which checks it against the exact same truth table
// (`reda::circuits::seven_segment::TRUTH_TABLE`) the hand-written decoder is
// checked against. Segment bit order per digit is `a b c d e f g`
// (active-high), matching that table exactly; digits 10..15 have no defined
// segments and are left dark.
//
// Written the natural way a person would describe this decoder -- one
// `case` arm per digit -- rather than mirroring the hand-written netlist's
// sum-of-products structure, so this is an honest independent synthesis
// path rather than a transcription of the other one.
module bcd_seven_segment(
    input  d3,
    input  d2,
    input  d1,
    input  d0,
    output a,
    output b,
    output c,
    output d,
    output e,
    output f,
    output g
);
    reg [6:0] seg;

    always @* begin
        case ({d3, d2, d1, d0})
            4'd0: seg = 7'b1111110;
            4'd1: seg = 7'b0110000;
            4'd2: seg = 7'b1101101;
            4'd3: seg = 7'b1111001;
            4'd4: seg = 7'b0110011;
            4'd5: seg = 7'b1011011;
            4'd6: seg = 7'b1011111;
            4'd7: seg = 7'b1110000;
            4'd8: seg = 7'b1111111;
            4'd9: seg = 7'b1111011;
            default: seg = 7'b0000000;
        endcase
    end

    assign {a, b, c, d, e, f, g} = seg;
endmodule

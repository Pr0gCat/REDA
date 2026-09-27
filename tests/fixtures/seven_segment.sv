module seven_segment (
    input  logic d3,
    input  logic d2,
    input  logic d1,
    input  logic d0,
    output logic a,
    output logic b,
    output logic c,
    output logic d,
    output logic e,
    output logic f,
    output logic g
);
    logic [6:0] seg;

    always_comb begin
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

    assign a = seg[6];
    assign b = seg[5];
    assign c = seg[4];
    assign d = seg[3];
    assign e = seg[2];
    assign f = seg[1];
    assign g = seg[0];
endmodule

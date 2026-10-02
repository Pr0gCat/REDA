module seven_segment (
    input  wire d3,
    input  wire d2,
    input  wire d1,
    input  wire d0,
    output reg a,
    output reg b,
    output reg c,
    output reg d,
    output reg e,
    output reg f,
    output reg g
);
    reg [6:0] seg;

    always @(*) begin
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

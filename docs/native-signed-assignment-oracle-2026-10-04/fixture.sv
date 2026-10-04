module top(input wire signed [3:0] a, input wire [3:0] b, output wire [7:0] y0, output wire [7:0] y1, output wire [7:0] y2, output wire [7:0] y3, output wire [7:0] y4, output wire [7:0] y5, output wire [7:0] y6, output wire [7:0] y7, output wire [7:0] y8, output wire [7:0] y9);
assign y0 = a;
assign y1 = ~a;
assign y2 = a ^ 4'shf;
assign y3 = a ^ b;
assign y4 = a[0] ? a : 4'sh8;
assign y5 = {a};
assign y6 = a == 4'shf;
assign y7 = !a;
assign y8 = a & b;
assign y9 = a | b;
endmodule

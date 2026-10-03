module matrix_all_low(input logic a,b,c,d, output logic y0,y1,y2,y3,y4,y5,y6);
assign y0 = 1'b0;
assign y1 = 1'b0;
assign y2 = 1'b0;
assign y3 = 1'b0;
assign y4 = 1'b0;
assign y5 = 1'b0;
assign y6 = 1'b0;
endmodule

module matrix_all_high(input logic a,b,c,d, output logic y0,y1,y2,y3,y4,y5,y6);
assign y0 = 1'b1;
assign y1 = 1'b1;
assign y2 = 1'b1;
assign y3 = 1'b1;
assign y4 = 1'b1;
assign y5 = 1'b1;
assign y6 = 1'b1;
endmodule

module matrix_mixed_constants(input logic a,b,c,d, output logic y0,y1,y2,y3,y4,y5,y6);
assign y0 = 1'b0;
assign y1 = 1'b1;
assign y2 = a;
assign y3 = d;
assign y4 = 1'b0;
assign y5 = 1'b1;
assign y6 = a ^ d;
endmodule

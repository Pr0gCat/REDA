module rom16x7(input logic d0,input logic d1,input logic d2,input logic d3,output logic y0,output logic y1,output logic y2,output logic y3,output logic y4,output logic y5,output logic y6);
logic [6:0] word;
always_comb begin
case ({d3,d2,d1,d0})
4'd0: word = 7'd21;
4'd1: word = 7'd98;
4'd2: word = 7'd58;
4'd3: word = 7'd65;
4'd4: word = 7'd12;
4'd5: word = 7'd113;
4'd6: word = 7'd38;
4'd7: word = 7'd91;
4'd8: word = 7'd100;
4'd9: word = 7'd25;
4'd10: word = 7'd79;
4'd11: word = 7'd50;
4'd12: word = 7'd117;
4'd13: word = 7'd10;
4'd14: word = 7'd83;
4'd15: word = 7'd45;
default: word = 7'd0;
endcase
end
assign y0=word[0];
assign y1=word[1];
assign y2=word[2];
assign y3=word[3];
assign y4=word[4];
assign y5=word[5];
assign y6=word[6];
endmodule

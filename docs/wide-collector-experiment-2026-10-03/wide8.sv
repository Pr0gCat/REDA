module wide8(input [3:0] a, output logic [7:0] q);
always_comb begin
case(a)
4'd0:q=8'b01010011;
4'd1:q=8'b11010100;
4'd2:q=8'b00111000;
4'd3:q=8'b11100001;
4'd4:q=8'b00101001;
4'd5:q=8'b10010010;
4'd6:q=8'b01111111;
4'd7:q=8'b01100100;
4'd8:q=8'b10000011;
4'd9:q=8'b01001010;
4'd10:q=8'b10111100;
4'd11:q=8'b11010111;
4'd12:q=8'b11101000;
4'd13:q=8'b00100110;
4'd14:q=8'b11111011;
4'd15:q=8'b01000101;
default:q=8'b0;
endcase
end
endmodule

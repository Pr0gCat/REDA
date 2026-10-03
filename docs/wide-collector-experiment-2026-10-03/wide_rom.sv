module wide_rom(input [3:0] a, output logic [15:0] q);
always_comb begin
 case(a)
4'd0: q=16'h9653;
4'd1: q=16'h2ad4;
4'd2: q=16'hc138;
4'd3: q=16'h73e1;
4'd4: q=16'h4b29;
4'd5: q=16'he592;
4'd6: q=16'h187f;
4'd7: q=16'hab64;
4'd8: q=16'h6d83;
4'd9: q=16'hf04a;
4'd10: q=16'h35bc;
4'd11: q=16'h82d7;
4'd12: q=16'h59e8;
4'd13: q=16'hd126;
4'd14: q=16'h07fb;
4'd15: q=16'hbe45;
default: q=16'h0000;
endcase
end
endmodule

module rom32(input logic [4:0] address, output logic [15:0] q);
always_comb begin
case (address)
5'd0: q = 16'h9653;
5'd1: q = 16'h2ad4;
5'd2: q = 16'hc138;
5'd3: q = 16'h73e1;
5'd4: q = 16'h4b29;
5'd5: q = 16'he592;
5'd6: q = 16'h187f;
5'd7: q = 16'hab64;
5'd8: q = 16'h6d83;
5'd9: q = 16'hf04a;
5'd10: q = 16'h35bc;
5'd11: q = 16'h82d7;
5'd12: q = 16'h59e8;
5'd13: q = 16'hd126;
5'd14: q = 16'h07fb;
5'd15: q = 16'hbe45;
5'd16: q = 16'h13a7;
5'd17: q = 16'heca1;
5'd18: q = 16'h4d86;
5'd19: q = 16'hb05f;
5'd20: q = 16'h7832;
5'd21: q = 16'h9a4d;
5'd22: q = 16'h027e;
5'd23: q = 16'hfd19;
5'd24: q = 16'h65b4;
5'd25: q = 16'h3c92;
5'd26: q = 16'ha871;
5'd27: q = 16'h571c;
5'd28: q = 16'hd2e6;
5'd29: q = 16'h0b43;
5'd30: q = 16'h8f25;
5'd31: q = 16'hc69a;
default: q = 16'h0000;
endcase
end
endmodule

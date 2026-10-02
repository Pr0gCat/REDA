module xor_word #(parameter W=1, MASK=0)(input [W-1:0] a, output [W-1:0] q);
  localparam LAST=W-1;
  assign q=a ^ MASK[LAST:0];
endmodule
module pair #(parameter W=2)(input [W-1:0] a, output [W-1:0] q);
  xor_word #(.W(W), .MASK(1)) core(.q(q), .a(a));
endmodule
module hierarchy_bits(input [3:0] a, output [3:0] q);
  pair #(2) low(a[1:0], q[1:0]), high(a[3:2], q[3:2]);
endmodule

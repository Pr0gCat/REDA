// A two-level bit-slice ripple adder: four 4-bit adders, each four full
// adders, so one cell type repeats sixteen times and one block four times.
module full_adder(input a, input b, input cin, output s, output cout);
  assign s = a ^ b ^ cin;
  assign cout = (a & b) | (cin & (a ^ b));
endmodule

module adder4(input [3:0] a, input [3:0] b, input cin, output [3:0] s, output cout);
  wire c1, c2, c3;
  full_adder bit0(.a(a[0]), .b(b[0]), .cin(cin), .s(s[0]), .cout(c1));
  full_adder bit1(.a(a[1]), .b(b[1]), .cin(c1), .s(s[1]), .cout(c2));
  full_adder bit2(.a(a[2]), .b(b[2]), .cin(c2), .s(s[2]), .cout(c3));
  full_adder bit3(.a(a[3]), .b(b[3]), .cin(c3), .s(s[3]), .cout(cout));
endmodule

module hierarchy_adder16(input [15:0] a, input [15:0] b, output [15:0] s, output cout);
  wire c4, c8, c12;
  adder4 nibble0(.a(a[3:0]), .b(b[3:0]), .cin(1'b0), .s(s[3:0]), .cout(c4));
  adder4 nibble1(.a(a[7:4]), .b(b[7:4]), .cin(c4), .s(s[7:4]), .cout(c8));
  adder4 nibble2(.a(a[11:8]), .b(b[11:8]), .cin(c8), .s(s[11:8]), .cout(c12));
  adder4 nibble3(.a(a[15:12]), .b(b[15:12]), .cin(c12), .s(s[15:12]), .cout(cout));
endmodule

// A 32-bit carry-lookahead adder in two levels: 4-bit lookahead groups,
// a lookahead unit over four groups making a 16-bit block, and the top
// joining two blocks. The carry crosses each level once instead of
// rippling through every bit.

// Carries into groups 1..3 (c[0..2]) from four groups' propagate and
// generate, and the four together as one bigger group.
module lookahead4(input [3:0] p, input [3:0] g, input cin, output [2:0] c, output pg, output gg);
  assign c[0] = g[0] | (p[0] & cin);
  assign c[1] = g[1] | (p[1] & g[0]) | (p[1] & p[0] & cin);
  assign c[2] = g[2] | (p[2] & g[1]) | (p[2] & p[1] & g[0]) | (p[2] & p[1] & p[0] & cin);
  assign pg = p[3] & p[2] & p[1] & p[0];
  assign gg = g[3] | (p[3] & g[2]) | (p[3] & p[2] & g[1]) | (p[3] & p[2] & p[1] & g[0]);
endmodule

module cla4(input [3:0] a, input [3:0] b, input cin, output [3:0] s, output pg, output gg);
  wire [3:0] p, g;
  wire [2:0] c;
  assign p = a ^ b;
  assign g = a & b;
  lookahead4 carry(.p(p), .g(g), .cin(cin), .c(c), .pg(pg), .gg(gg));
  assign s = p ^ {c, cin};
endmodule

module cla16(input [15:0] a, input [15:0] b, input cin, output [15:0] s, output pg, output gg);
  wire [3:0] p, g;
  wire [2:0] c;
  cla4 group0(.a(a[3:0]), .b(b[3:0]), .cin(cin), .s(s[3:0]), .pg(p[0]), .gg(g[0]));
  cla4 group1(.a(a[7:4]), .b(b[7:4]), .cin(c[0]), .s(s[7:4]), .pg(p[1]), .gg(g[1]));
  cla4 group2(.a(a[11:8]), .b(b[11:8]), .cin(c[1]), .s(s[11:8]), .pg(p[2]), .gg(g[2]));
  cla4 group3(.a(a[15:12]), .b(b[15:12]), .cin(c[2]), .s(s[15:12]), .pg(p[3]), .gg(g[3]));
  lookahead4 carry(.p(p), .g(g), .cin(cin), .c(c), .pg(pg), .gg(gg));
endmodule

module cla_adder32(input [31:0] a, input [31:0] b, output [31:0] s, output cout);
  wire p0, g0, p1, g1;
  cla16 lo(.a(a[15:0]), .b(b[15:0]), .cin(1'b0), .s(s[15:0]), .pg(p0), .gg(g0));
  cla16 hi(.a(a[31:16]), .b(b[31:16]), .cin(g0), .s(s[31:16]), .pg(p1), .gg(g1));
  assign cout = g1 | (p1 & g0);
endmodule

module register_bit(input d,clk,en,output reg q);
  always @(posedge clk) if(en) q<=d;
endmodule
module register_wrapper(input d,clk,en,output q);
  register_bit r(d,clk,en,q);
endmodule
module hierarchy_dff(input d,clk,en,output q);
  register_wrapper w(.clk(clk),.en(en),.q(q),.d(d));
endmodule

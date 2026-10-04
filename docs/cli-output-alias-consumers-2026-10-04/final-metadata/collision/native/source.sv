module m(input [1:0] a, output g1,other,alias);
assign g1=~a[1]; assign other=~a[0]; assign alias=~a[1];
endmodule

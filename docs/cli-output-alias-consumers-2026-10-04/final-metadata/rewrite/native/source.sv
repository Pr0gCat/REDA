module m(input [1:0] a, output [1:0] y, output z);
assign y[0]=~a[1]; assign y[1]=~a[0]; assign z=~a[1];
endmodule

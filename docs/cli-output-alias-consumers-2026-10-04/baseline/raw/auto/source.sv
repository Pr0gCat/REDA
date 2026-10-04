module m(input [5:0] a, output [7:0] y, output z);
assign y[0]=a[5]|a[4]; assign y[1]=1'b0; assign y[2]=1'b1;
assign y[3]=a[3]&a[2]; assign y[4]=a[3]&a[2]; assign y[5]=a[3];
assign y[6]=a[1]^a[0]; assign y[7]=~(a[1]|a[0]); assign z=a[0];
endmodule

module wide_mixed(input [3:0] a, output q00, output q01, output q02, output q03, output q04, output q05, output q06, output q07, output q08, output q09, output q10, output q11, output q12, output q13, output q14, output q15);
assign q00=1'b0;
assign q01=1'b1;
assign q02=a[0];
assign q03=a[3];
assign q04=a[0]^a[1];
assign q05=1'b0;
assign q06=1'b1;
assign q07=a[2];
assign q08=1'b0;
assign q09=1'b1;
assign q10=a[0];
assign q11=a[3];
assign q12=a[0]^a[1];
assign q13=1'b0;
assign q14=1'b1;
assign q15=a[2];
endmodule

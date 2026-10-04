module child #(parameter W=8, parameter logic [W-1:0] P=4'hf+4'h1)(input [3:0] a,output [7:0] y); assign y={4'b0,a} ^ P; endmodule
module m(input [3:0] a,output [7:0] y); child #(.W(8)) u(.a(a),.y(y)); endmodule

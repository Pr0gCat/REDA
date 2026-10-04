module leaf #(parameter integer W=4,parameter logic [W-1:0] P=4'hf+4'h1)(output [W-1:0] y);assign y=P;endmodule module m(output [7:0] y);leaf #(.W(8)) u(.y(y));endmodule

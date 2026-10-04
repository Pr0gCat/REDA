module leaf #(parameter logic [7:0] P=0)(output [7:0] y);assign y=P;endmodule module m(output [7:0] y);leaf #(.P(4'hf+4'h1)) u(.y(y));endmodule

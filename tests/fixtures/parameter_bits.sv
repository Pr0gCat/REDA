module parameter_bits #(
    parameter int BASE = 2,
    parameter integer WIDTH = BASE * 3 - 2
)(input [WIDTH-1:0] a, output [WIDTH-1:0] q);
    localparam MASK = 5;
    wire [WIDTH-1:0] value;
    assign value = a ^ MASK[WIDTH-1:0];
    assign q[WIDTH-1:0] = value;
endmodule

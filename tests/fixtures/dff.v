module dff_example(
    input  wire d,
    input  wire clk,
    output reg  q
);
always @(posedge clk)
    q <= d;
endmodule

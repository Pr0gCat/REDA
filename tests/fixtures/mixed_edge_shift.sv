module mixed_edge_shift(input logic clk, d, output logic [1:0] q);
    always_ff @(posedge clk) q[0] <= d;
    always_ff @(negedge clk) q[1] <= q[0];
endmodule

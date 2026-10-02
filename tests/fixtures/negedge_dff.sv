module negedge_dff(input logic d, clk, output logic q);
    always_ff @(negedge clk) q <= d;
endmodule

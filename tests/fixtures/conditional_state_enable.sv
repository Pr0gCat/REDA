module conditional_state_enable(input logic clk, d, en, output logic q);
    always_ff @(posedge clk) if (q || en) q <= d; else q <= q;
endmodule

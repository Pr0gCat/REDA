module multiclock_shift(
    input logic d_a, d_b, clk_a, clk_b,
    output logic q_a, q_b
);
    always_ff @(posedge clk_a) q_a <= d_a;
    always_ff @(posedge clk_b) q_b <= q_a;
endmodule

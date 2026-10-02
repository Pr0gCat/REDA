module domain_register(input d, clk, output reg q);
    always @(posedge clk) q <= d;
endmodule
module hierarchy_multiclock_dff(
    input d_a, d_b, clk_a, clk_b,
    output q_a, q_b
);
    domain_register a(.d(d_a), .clk(clk_a), .q(q_a));
    domain_register b(.d(d_b), .clk(clk_b), .q(q_b));
endmodule

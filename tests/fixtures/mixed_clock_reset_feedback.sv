module mixed_clock_reset_feedback(
    input logic clk_a, clk_b, reset_a, reset_b, en,
    output logic q_a, q_b
);
    always_ff @(posedge clk_a) begin
        if (reset_a) q_a <= 1'b0;
        else if (en) q_a <= ~q_a;
    end
    always_ff @(negedge clk_b) begin
        if (reset_b) q_b <= 1'b0;
        else q_b <= q_a;
    end
endmodule

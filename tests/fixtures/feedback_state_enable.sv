module feedback_state_enable(input logic clk, d, en, output logic q);
    always_ff @(posedge clk) if (q || en) q <= d;
endmodule

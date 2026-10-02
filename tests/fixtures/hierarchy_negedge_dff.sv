module falling_leaf(input logic data, clock, output logic state);
    always_ff @(negedge clock) state <= data;
endmodule
module falling_middle(input logic x, c, output logic y);
    falling_leaf leaf(.data(x), .clock(c), .state(y));
endmodule
module hierarchy_negedge_dff(input logic d, clk, output logic q);
    falling_middle child(.x(d), .c(clk), .y(q));
endmodule

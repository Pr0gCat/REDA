module reset_leaf(input logic leaf_clk, reset, en, output logic [1:0] q);
    always_ff @(posedge leaf_clk) begin
        if (reset) begin q <= 2'b00; end
        else if (en) q <= {q[1] ^ q[0], ~q[0]};
    end
endmodule

module hierarchy_reset_counter(input logic clk,reset,en,output logic [1:0] q);
    reset_leaf leaf(.leaf_clk(clk),.reset(reset),.en(en),.q(q));
endmodule

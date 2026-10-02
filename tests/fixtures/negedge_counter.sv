module negedge_counter(input logic clk, reset, en, output logic [1:0] q);
    always_ff @(negedge clk)
        q <= reset ? 2'b00 : (en ? {q[1] ^ q[0], ~q[0]} : q);
endmodule

module arithmetic_counter_subtract(input logic clk,reset,en,output logic [3:0] q);
    always_ff @(posedge clk) begin
        if (reset) q <= 4'b0000;
        else if (en) q <= q - 4'b0001;
    end
endmodule

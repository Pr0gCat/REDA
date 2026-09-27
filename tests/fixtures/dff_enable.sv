module dff_enable(
    input logic d,
    input logic clk,
    input logic en,
    output logic q
);
    always_ff @(posedge clk) begin
        if (en) q <= d;
    end
endmodule

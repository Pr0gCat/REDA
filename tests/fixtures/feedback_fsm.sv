module feedback_fsm(
    input logic clk, reset, advance,
    output logic [1:0] q
);
    logic [1:0] next;
    always_comb begin
        case (q)
            2'b00: next = advance ? 2'b01 : 2'b00;
            2'b01: next = advance ? 2'b10 : 2'b01;
            2'b10: next = advance ? 2'b00 : 2'b10;
            default: next = 2'b00;
        endcase
    end
    always_ff @(posedge clk) q <= reset ? 2'b00 : next;
endmodule

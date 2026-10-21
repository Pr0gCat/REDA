module arithmetic_pair(input logic [1:0] a,b,output logic [1:0] sum,difference);
    assign sum = a + b;
    assign difference = a - b;
endmodule

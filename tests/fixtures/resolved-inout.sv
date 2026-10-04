module writer(input wire en, data, inout wire bus);
  assign bus = en ? data : 1'bz;
endmodule
module top(input wire e0,d0,e1,d1,inout wire bus,output wire y);
  writer a(.en(e0),.data(d0),.bus(bus));
  writer b(.en(e1),.data(d1),.bus(bus));
  assign y = bus;
endmodule

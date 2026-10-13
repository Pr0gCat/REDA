// Second shared-inout fixture: a DIFFERENT driver/contention structure from
// the first (docs/minecraft-native-inout-2026-10-04/fixture.sv). Here one of
// the two internal writers drives the INVERTED data, so the contention and
// floating distribution over the 64-state space differs, and the generated
// netlist contains an inverter the first fixture does not. Same public contract:
// top-level inout bus, four-state resolution, public readback y.
module writer(input wire en, data, inout wire bus);
  assign bus = en ? data : 1'bz;
endmodule
module writer_inv(input wire en, data, inout wire bus);
  assign bus = en ? ~data : 1'bz;
endmodule
module top(input wire e0,d0,e1,d1,inout wire bus,output wire y);
  writer a(.en(e0),.data(d0),.bus(bus));
  writer_inv b(.en(e1),.data(d1),.bus(bus));
  assign y = bus;
endmodule

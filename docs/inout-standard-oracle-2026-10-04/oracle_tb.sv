module oracle_tb;
  reg e0,d0,e1,d1,ext_present,ext_value;
  wire bus,y;
  integer from_state,to_state,phase;
  reg [5:0] packed_state;
  top dut(.e0(e0),.d0(d0),.e1(e1),.d1(d1),.bus(bus),.y(y));
  assign bus = ext_present ? ext_value : 1'bz;
  task apply_state(input integer state);
    begin
      packed_state=state;
      {ext_value,ext_present,d1,e1,d0,e0}=packed_state;
      #1;
    end
  endtask
  task emit(input integer f,input integer t,input integer p);
    reg [7:0] cls;
    begin
      if (bus === 1'b0) cls="0";
      else if (bus === 1'b1) cls="1";
      else if (bus === 1'bx) cls="X";
      else if (bus === 1'bz) cls="Z";
      else $fatal(1,"unclassifiable bus");
      if (!(y === bus)) $fatal(1,"public readback differs");
      $display("%0d,%0d,%0d,%c",f,t,p,cls);
    end
  endtask
  initial begin
    for(from_state=0;from_state<64;from_state=from_state+1)
      for(to_state=0;to_state<64;to_state=to_state+1) begin
        apply_state(from_state); emit(from_state,to_state,0);
        apply_state(to_state); emit(from_state,to_state,1);
      end
    $finish;
  end
endmodule

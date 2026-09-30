# Logical masking of backfeed (rejected)

Control: `5a96dae`. Prior experiments show that internal dust fanout can transmit a multi-input support's power into an inverter branch. Isolating all fanout fixes the ROM family but violates accepted XOR timing; isolating only fanout between multiple multi-input supports misses words `[4,1,0,2]`.

This experiment evaluated logical masking before routing. For each shared net feeding a multi-input non-merge support, enumerate primary input assignments (bounded at eight inputs). When the net is low and another input to one of those supports is high, force the shared net high in a second combinational evaluation. Request diode terminals if any declared output changes. Larger or sequential graphs conservatively require isolation. This is a logical hazard heuristic, not a physical proof: it does not know which support landings the router will actually realize as diodes.

The complete 4096-table stock scan passes output driver/lamp truth for 65536 target transitions, with 65536 setup phases and deterministic recompilation (141.36 seconds). It does not certify every internal driver or Minecraft execution.

The fixed production quality matrix rejects `xor2/native` mean settling of 7.875 ticks. No cap changes are permitted. This rules out the implementation regardless of family truth results. The helper, routing predicate change, and temporary family probe are withdrawn; production code is restored exactly to the control. No accepted tests are skipped or weakened.

The next physical isolation hypothesis must combine actual terminal directionality and reverse-power reach with the existing timing controls. Logical susceptibility alone overconstrains routing and does not establish an acceptable improvement. Automatic stock/torch-row selection remains disabled.

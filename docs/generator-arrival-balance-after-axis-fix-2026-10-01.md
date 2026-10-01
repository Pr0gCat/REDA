# Arrival balancing after the refresh-axis fix

Reference c90698b. Replay the archived required-refresh weak-endpoint patch
and the existing generic nominal arrival balancer. This is a diagnostic world
mutation, not a certified production route or torch-row implementation.

With the sideways forced refresh removed, b->n2 is correctly estimated as
zero-delay, so the balancer inserts three private straight-cell relays instead
of two. All 16 setup and 16 target phases pass internal gate truth and an
independent XOR lamp expectation with retained physical history. Two complete
runs have byte-identical stdout; each run also checks deterministic base
compilation. No simulator, hold interval, truth expectation or cap changes.

Both driver and lamp have zero extra transitions and zero wrong pulses.
For 3->0, b->n2 now falls at tick 2, simultaneous with n0 rising; the former
n2/output pulse disappears. This supports the endpoint-skew diagnosis rather
than treating a repeater merely present in a recorded path as a delay.

The candidate still fails the frozen checkpoint: target settling totals 108
ticks over 16 transitions (mean 6.75), above the unchanged 6.375 cap. Worst
settling is 12, last driver/lamp changes are 8/12. Improved glitches cannot
compensate for slower settling. No block count contributes to acceptance.
The complete trace and declared mutation/ledger mismatch are retained in the
adjacent JSON. No global quality pass or Minecraft certification is claimed.

The three temporary production files were restored exactly from c90698b.
The existing diagnostic executable was replayed before rebuilding, while it
still contained the archived candidate. Only this evidence is committed.
Next investigate reducing original branch skew (including redundant terminal
delay and logical depth), then qualify any generic generator change against
independent controls; adding delays alone is not accepted.

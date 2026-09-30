# Function recognition and certified torch-row generation

compile_torch_row is now a production library entry accepting the caller's
actual netlist. It validates the full source using the existing exhaustive
rewrite checker before evaluating its four input vectors, derives three
output bits per vector, builds the qualified normalized graph and physical
candidate, and returns the sealed source-boundary/physical rewrite result.
Names, modules and case identifiers do not participate in recognition.

The qualified scope remains exactly two inputs and three declared output
occurrences. Other boundaries are refused explicitly. Sequential logic,
malformed graphs and proof-budget exhaustion fail closed. This entry reuses
the frontend's existing deterministic cover implementation and the certified
rewrite API; it introduces no dependency or alternative physical verifier.
Each proof separately consumes its stated work cap. The bounded truth-table
recognition prevents arbitrary HDL width from expanding this constructor.

The actual constructor and explicit matrix library were promoted from the
qualified fixture. The former fixture is retained independently in tests,
alongside the unchanged original aligned-refresh world control and frozen
39257ff nonconstant per-table caps. The complete 4096-table test now obtains
its worlds and observation data through production function recognition,
compares the exact implemented graph/candidate identity/emitted bytes against
the prior fixture, and retains all internal truth, topology, deterministic
reconstruction and nine-metric physical checks. No cap is regenerated.

## Physical results

All 4096 native-source tables pass, with 65536 measured target transitions per
arm, 611056 control / 608752 candidate aggregate settling, 120 improved tables,
and 16 worst ticks. The proof-set fingerprint is unchanged. This promotion
introduces no new prototype timing gain. Renamed gate/signal names and reversed
input/output declarations pass deterministic recognition; invalid arity,
undriven pins, stateful gates, insufficient proof work and unsupported boundary
shapes are refused. Two fresh Yosys sources also use this production entry.

A separate six-case experiment now compares actual stock compile() layouts
against this entry (96 target transitions per arm, plus setup history).
Shipping public aliases are measured through the normal frontend physical
netlist with unique driver declarations, retaining all three public readings.
Raw metric values and traces are in the adjacent shipping-comparison JSON.

For [1,6,3,5], worst settling is 22 to 14 ticks, aggregate 217 to 162,
driver/lamp glitches 4/2 to 0/0 and wrong-driver pulse ticks 12 to 0.
For [3,6,6,1], worst is 22 to 16, aggregate 232 to 181, glitches 5/3
to 0/0 and pulse ticks 12 to 0. All nine metrics improve or tie in both.
But [0,1,2,3] regresses from 8 to 14 worst ticks and 81 to 153 aggregate.
[0,0,0,7] improves worst/glitches/pulses but regresses aggregate 75 to 111.
All-zero/all-one cases tie. Therefore unconditional replacement is rejected;
those regressions are not excused by the improvements or block counts.

Stock compile() and CLI selection remain unchanged. Next work must choose
between actual certified candidates using all nine per-boundary metrics,
retain the stock layout on any regression, and expose the explicit rewritten
netlist to timing/viewer callers. The shipping experiment is exploratory;
it does not prove every ROM improves over shipping. Simulator qualification
is not Minecraft certification or a clock-rate guarantee.

## Validation

Complete production-entry family passes in 111.85s. Recognition/refusal and
fresh-Yosys tests pass; current production quality suites preserve all limits
over 1312 measured transitions and four constant-driver physical tests pass.
The initial rustc release build crashed with SIGSEGV; retry succeeded without
changing the design or omitting tests. git diff --check passes. Unrelated
existing packing warnings remain; this is not a globally green-suite claim.

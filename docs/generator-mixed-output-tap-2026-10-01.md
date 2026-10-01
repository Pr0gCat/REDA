# Mixed comparator output taps: rejected physical prototypes

Reference `b4daaa4`. Three independent hand-built probes attempt outputs
`a AND b`, `NOT a`, `NOT b` from two actual primary levers. The AND uses the
existing subtract/inverter geometry; a second comparator attempts to share
the rear primary support, and the internal inverter's strong side handover
support supplies a public passive branch. No duplicate primary levers,
compiler selection, simulator edits or certification exemptions are used.

All variants run both input permutations, every from/to pair and its setup
phase in one retained simulator history (64 phases per variant). Each checks
independent Boolean driver and lamp expectations. They finish the histories
and exit 1 on incorrect truth. These are rejected local experiments; no
full-family acceptance is attempted for physically incorrect constructions.

Upper extraction fails target 0->1 with `NOT a` remaining high. Lower-north
extraction fails 0->2 with `NOT b` remaining high. Lower-south extraction
already drives AND high at address zero. The actual first incorrect outputs
and complete histories are in the adjacent JSON artifacts. These observations
prove the attempted layouts invalid, but do not yet isolate every causal
power path; a coordinate-based suspicion is not a certified diagnosis.

Sources for each variant are archived alongside results. To reproduce, unpack
one source into `examples/comparator_mixed_outputs_probe.rs`, source
`tools/env.sh`, and run `RUST_MIN_STACK=67108864 cargo run --release --example
comparator_mixed_outputs_probe`. Remove this temporary example afterwards.
The failed prototype is not installed in production source.

Next isolate the individual tap geometries with causal cuts and direct
comparators/support observations. In particular, a support that is safe for
the internal side input does not automatically authorize a public branch.
Develop an electrically isolated distribution layout rather than assembling
previously qualified cells without verifying their new neighbouring domains.

# Retain a strong pinned-fanout negative control

The pinned `g4` fanout test needs another input to strongly power the shared `g26` support. Its original layout happened to supply that through the `g1` terminal repeater. Qualified arrival-runway selection now gives that sibling a dust terminal; the existing prerequisite assertion correctly reports that the world no longer exercises the backfeed defect. Accepting a dust sibling or dropping that assertion would make the truth sweep decorative.

The test now explicitly supplies a repeater at the existing collinear `g1` terminal, updates the candidate's exact terminal/block metadata, and runs the complete existing physical verification before using the world. Pins, shared support, eight-vector truth expectation and sibling-repeater assertion remain unchanged. The public planner request and generated routing policy do not change.

The verified world must report `g4`'s own function on all eight vectors. A separate negative control removes only the `g4` gate-input diode and must produce a truth mismatch over the same eight vectors. Setup/settling errors do not satisfy that negative control. Thus the test proves the strong sibling still exposes missing isolation instead of relying on whichever terminal a later placement happens to choose. The existing test passes in 0.17 seconds.

The before artifact records the actual prerequisite failure from the still-running root snapshot; it is not a completed full-root report. The repaired fixture is compiled and tested afterwards. No updated full-root count, arithmetic counter success, simulator change or verifier relaxation is claimed. The overall eight-item goal remains incomplete.

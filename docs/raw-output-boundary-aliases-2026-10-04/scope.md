# Explicit raw output boundary aliases

Fixes actual raw compilation of repeated source output signals using explicit PortId-to-canonical-PortId ownership. Every source occurrence retains its typed assignment and observation; exactly one physical boundary and terminal own the shared lamp or pinned external port.

The alias relation is fingerprinted. Shape and physical ownership validate the same typed source driver, identical observation location/state/logical owner, no self references or chains, zero alias-owned physical blocks and a real canonical block or pin contract. Declared output route checks retain exactly one physical route for the canonical owner and require every occurrence to resolve to that route. The adapter skips only exact duplicate pinned terminals with an explicit shared-owner relation; other malformed duplicates retain the original refusal.

Timing retains each declared-output node with a certified zero-delay binding from its canonical output landing, so aliases have identical static arrival/tail values. No physical geometry or redstone simulation rules change. The compatibility map remains one position per signal; output_occurrences retains source order and repeated entries.

Controls cover three raw compiler APIs, reverse occurrence order, duplicate lamps at their real shared location, pinned external ports against the original planner emission, unchanged rewrite distinct lamps, and eight independent ownership corruptions. All four setup/target truth histories are tested at the unchanged 2000 settling cap.

An incremental rustc ICE occurred while prototype edits overlapped a prior compiler process; final qualification uses CARGO_INCREMENTAL=0 and serial builds. This is not a circuit-budget or correctness waiver.

Exact frozen 6-input, 9-output default entry now compiles successfully and records every source occurrence, including v/w sharing the same real output position. Original failure and exact source are retained here. This compile acceptance does not replace the separate full 8192-phase default-versus-candidate quality audit.

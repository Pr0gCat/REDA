# NOR and independent negative outputs on shared primary supports

Independent prototype after `8740ad4`; frozen control `541eb4b`. A two-side
NOR comparator and two unary negative comparators can read the same two
primary strong supports. The NOR has its own rear reference; both unary
comparators have separate references and public rails. The centre NOR rail
extends west into an output domain separated from the outer negative rails.
No source levers are duplicated and no simulator rules change.

Both input permutations pass all 32 continuous setup/target phases. Against
their independently frozen certified controls, worst settling improves 8 to
6 ticks, total target settling 81 to 57, driver last change 4 to 2 and lamp
last change 8 to 6. All driver/lamp glitch and pulse metrics remain zero.
All twelve metrics are independently nonregressing; complete repeat is exact.

Reproduce with `source tools/env.sh` and
`RUST_MIN_STACK=67108864 cargo run --release --example comparator_mixed_nor_probe`.
Raw records and comparisons are archived beside this report. Optional cuts
are diagnostics, not production selection rules. This is not compiler
certification or a complete family qualification. Next compose selected
Nor(2) and Nor(1) comparator contracts, certify every shared support, and
generalize aliases and constants before the fixed 4096 comparison.

# Direct subtraction mixed with independent negative outputs

Independent physical prototype after `c7381eb`; not compiler certification.
The frozen comparator AND mixtures motivate applying direct subtraction to
`a AND NOT b`, `NOT a`, `NOT b` with the same two primary levers.

The raw negative input now directly strongly powers the target side support.
A public negative comparator reads the opposite side of that same support,
with a private reference and output rail. The positive primary likewise feeds
the target rear and the other negative comparator. Neither public negative
rail exports the target's side support. Outputs occupy isolated conductor
domains; no duplicate input levers or simulator edits are used.

Both input permutations pass all 32 retained-history setup/target phases.
Against their independently frozen certified records, worst settling is
10 to 6 ticks, total target settling 108 to 60, driver last change 6 to 2,
and lamp last change 10 to 6. All driver/lamp glitch and pulse metrics remain
zero; twelve metrics individually have no regression. Complete repeat output
is exact. Raw records and comparisons are in the adjacent JSON archive.

Reproduce using `source tools/env.sh` and
`RUST_MIN_STACK=67108864 cargo run --release --example comparator_mixed_subtract_probe`.
The optional coordinate cut is a diagnostic, not a generation selection rule.
These local results do not establish source rewrite equivalence, strict
certification, the 4096 family, or default compiler integration. Next generalize
the product polarity structurally and certify every shared boundary and output
occurrence before adopting this construction.

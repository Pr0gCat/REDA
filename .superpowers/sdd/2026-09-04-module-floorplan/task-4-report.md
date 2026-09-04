# Task 4 report: hierarchical netlist builder for tests

## Files

- Created `src/circuits/hierarchical_builder.rs`.
- Modified `src/circuits/mod.rs` (added `pub(crate) mod hierarchical_builder;`).

## What was built

`HierarchicalNetlistBuilder`/`ModuleBuilder` exactly as specified in the brief, plus a
`#[cfg(test)] pub(crate) mod circuits` with `full_adder_module`, `ripple_adder(bits)`,
`alu4_full()`, `multiplier4()`, `alu8()`.

Strict RED/GREEN was followed: the two brief-mandated tests were written first
(referencing not-yet-existing `circuits::ripple_adder`/`circuits::alu8`), confirmed to
fail with a compile error (`cannot find module or crate 'circuits'`), then the full
implementation was added. `cargo test --lib circuits::hierarchical_builder` and
`cargo test --lib compile::hierarchy` both pass (5 and 13 tests respectively).

## The literal-port-name convention (why `expose` exists)

`HierarchicalNetlist::flatten` aliases a child module's signals by exact string match:
wherever a module's own gates reference the literal string of one of its declared ports
(as a gate input, or as a gate's own `output` field), that string is what the parent's
`instance.ports` binding rewrites during flattening (confirmed against
`compile::hierarchy`'s own `two_level`/`a_gate_named_after_a_port_is_not_aliased` tests,
where e.g. `gate("g0", &["a"], "y", ...)` has `output: "y"` literally, matching the
module's declared output port `"y"`).

`NetlistBuilder`'s reduction helpers (`and_reduce`, `or_reduce`, `nor`, `not`, ...)
always invent their own generated name (`g0`, `g1`, ...) for whatever they build. So
whenever a value a module computes with its *own* gates must become one of that
module's declared **output** ports, something has to force the final gate to carry the
literal port name instead of a generated one. There is no "identity"/buffer primitive
in this project (a wire doesn't need a gate to relay itself), so the file defines one
helper, `expose(gates, value, name)`, that does `NOT(NOT(value))` — two ordinary
single-input NOR gates (exactly what every other `not()` call in the codebase already
is), with the second one built via `nor_named(name, name, ...)` so its `output` field is
literally `name`.

`expose` is only needed when a module's own glue computes the final value of one of its
declared outputs. A value that simply passes through from a child instance's own output
binding (e.g. a slice's `r` bound straight to the parent's own declared output wire)
never needs it — this is why `alu4` (the module built from 4 `slice` instances) and
`adder_row` (built from 4 `full_adder` instances) have zero `expose` calls: every one of
their declared outputs is a pure instance pass-through. `full_adder`, `slice` (both
variants), and the glue-computing parts of `alu4_full`'s `top` and `multiplier4`'s `top`
do need it.

## Module port lists

### `full_adder` (used by `ripple_adder` and `multiplier4`'s `adder_row`)

- Inputs: `a, b, cin`.
- Outputs: `sum, cout`.
- Same gate structure as `fragment_synth::seed`'s `extra_circuits::full_adder`/`xor`
  (ported, not reused — the flat helpers stay untouched per the task constraints), with
  `sum`/`cout` forced to their literal names via `expose`.

### `ripple_adder(bits)`

- `top` inputs: `a0..a{bits-1}, b0..b{bits-1}, cin`; outputs: `s0..s{bits-1}, cout`.
- `bits` instances of `full_adder`, `cout` chained bit to bit; bit 0's `cin` is bound to
  `top`'s own `cin` input directly, so this design never uses a constant.

### `slice` inside `alu4_full()` — one bit of `extra_circuits::alu4_full`

- Inputs: `a, b, sel0..sel7, sub, cin, shift_in` (12).
- Outputs: `r, cout`.
- `sel0..sel7` are the pre-decoded one-hot opcode selects and `sub` is the shared
  subtract-mode signal — both computed once in `top`'s own glue (the "opcode decode"
  the brief calls out) and passed in as ordinary ports, matching `alu4_full`'s
  "decode once, reuse per bit" structure instead of re-decoding the opcode in every
  slice. `nsub` is *not* a separate port — it's `not(sub)` computed locally inside the
  slice, since `sub` is always an ordinary `Signal` binding (never a constant), so
  inverting it locally carries no fold risk.
- `shift_in` is the SHL chain input (the previous bit's `a`, or 0 for bit 0). Bit 0's
  `shift_in` is tied to `PortBinding::Zero` per the brief, and `cin` is tied to the
  `sub` signal — the only constant used anywhere in this task's four circuits.

**Why `shift_in` needed a specific gate shape to fold.** `specialise_constants` only
folds a constant into a `Nor`/`Or` gate whose *other* inputs stay non-empty afterward —
folding a lone-input `Nor`'s only input away leaves arity 0, i.e. a hard-wired constant
output, which `specialise_module` refuses by name (`UnfoldableConstant`). Every
`NetlistBuilder` reduction helper computes `AND`/`OR` via De Morgan, which means the
*first* thing done to any raw operand is `not()` — a solitary single-input `Nor`. If
`shift_in` were inverted that way, the fold would always fail (arity 1 → 0). To avoid
that, the slice computes `NOT(shift_in)` as a 2-input `Nor` with a filler operand
instead of a 1-input one:

```
zero          = AND(a, NOT(a))            -- a real, always-0 signal (never a constant)
not_shift_in  = NOR(shift_in, zero)       -- 2 inputs; folds cleanly when shift_in=0
shifted_term  = NOR(not_shift_in, NOT(sel6))   -- = AND(shift_in, sel6)
```

When `shift_in` is folded away (bit 0), `NOR(shift_in, zero)` correctly reduces to
`NOR(zero)` = `NOT(zero)` = 1, matching `NOT(0)` = 1. When `shift_in` is an ordinary
signal (bits 1–3), the gate is unaffected and computes the same `NOT(shift_in)` it
always would. This is the concrete answer to the brief's warning about designing slice
modules "so any constant you use lands on a Nor or Or input."

### `alu4_full()` top module

- Inputs: `a0..a3, b0..b3, s0, s1, s2` (11).
- Outputs: `r0..r3, cout, zero` (6).
- Glue: the 8-way opcode decode (`sel0..sel7`) and `sub`, plus the zero-flag
  (`OR-reduce(r0..r3)` then `NOT`, exposed as `zero`). `cout` and `r0..r3` are pure
  pass-throughs from the four `slice` instances (`cout` from `slice3`, `r{i}` from
  `slice{i}`) — no `expose` needed for those.
- Module order: `slice` before `top` (2 levels — this circuit does not need 3-level
  nesting; that's `alu8`'s job).

### `adder_row` (used by `multiplier4`)

- Inputs: `x0..x3` (the shifted accumulator from the previous row, or a row's own
  partial products for row 0's glue), `y0..y3` (this row's partial products).
- Outputs: `s0..s3, cout`.
- Four `full_adder` instances, ripple-chained; `fa0`'s `cin` is tied to a real
  always-0 signal (`AND(x0, NOT(x0))`) computed inside `adder_row` itself — an ordinary
  wire, not a `PortBinding` constant, exactly per the brief's "avoid constants entirely"
  option, since a constant `cin` on `full_adder` would hit the same "lone `not()` on a
  constant" problem `shift_in` has, and `full_adder`'s internal gate shape wasn't
  redesigned to guard against it (only `slice` was, since only `alu4_full` needed a
  constant at all).

### `multiplier4()` top module

- Inputs: `a0..a3, b0..b3` (8). Outputs: `o0..o7` (8, the product bits).
- Glue computes all 16 partial products (`AND(a_i, b_j)`); `o0` is `pp(0,0)` exposed
  directly (the only top-level `expose` in this circuit). `row1`/`row2`/`row3` are three
  `adder_row` instances summing successive rows of partial products, chained exactly as
  `extra_circuits::multiplier4` does; `o1..o7` are all pure pass-throughs from those
  instances' `s0`/`cout` bindings.
- Module order: `full_adder` → `adder_row` → `top`.

### `slice` inside `alu8()` — one bit of `extra_circuits::alu4` (distinct module, distinct `HierarchicalNetlist`)

- Inputs: `a, b, sel0..sel3, cin` (7). Outputs: `r, cout`.
- AND/OR/XOR/ADD 4-way one-hot mux, matching flat `alu4`'s per-bit body. Named `slice`
  in its own builder instance, same name as `alu4_full`'s `slice` but in a completely
  separate `HierarchicalNetlist` (each top-level circuit function starts a fresh
  `HierarchicalNetlistBuilder`), so there's no collision.

### `alu4` (used twice by `alu8`'s top)

- Inputs: `a0..a3, b0..b3, s1, s0, cin` (11). Outputs: `r0..r3, cout` (5).
- Glue: only the 2-bit opcode decode (`sel0..sel3`); every declared output is a pure
  pass-through from the four `slice` instances (`cout` from `slice3`).

### `alu8()` top module

- Inputs: `a0..a7, b0..b7, s1, s0, cin` (19). Outputs: `r0..r7, cout` (9).
- Two `alu4` instances (`lo` for bits 0–3, `hi` for bits 4–7), opcode (`s1, s0`) shared
  between both, `lo.cout` chained into `hi.cin`. `top` has **zero** gates of its own —
  everything is instance wiring, which is enough (`Module.gates: vec![]` is a normal,
  already-tested shape — see `compile::hierarchy`'s own `two_level` test).
- This is the milestone three-level nesting case: `top` → `alu4` → `slice`. The brief's
  own `alu8_nests_three_levels` test confirms `module_order` puts `slice` before
  `alu4`, and that `flatten` produces a `GatePath` with `path.len() == 2` (a slice
  gate's path is `["lo"|"hi", "slice{i}"]`).

## Tests added beyond the brief's two

- `ripple_adder_validates_and_orders_children_before_parents` — `validate()`,
  `module_order() == ["full_adder", "top"]`, `flatten()` + `combinational_order()`.
- `alu4_full_validates_orders_and_flattens_after_specialising_its_constant` — `validate()`,
  `module_order()` puts `slice` first and `top` last, confirms raw `flatten()` is
  refused (unspecialised constant), then `specialise_constants().flatten()` succeeds
  with a valid `combinational_order()`.
- `multiplier4_validates_orders_and_flattens` — `validate()`, `module_order()` orders
  `full_adder` < `adder_row` < `top`, `flatten()` + `combinational_order()`, plus input/
  output counts (8 in, 8 out).

Combined with the brief's `ripple_adder8_...` and `alu8_nests_three_levels`, every one
of the four circuit constructors (`ripple_adder`, `alu4_full`, `multiplier4`, `alu8`)
now has a dedicated test proving it validates, orders children before parents, and
flattens to a netlist with a valid `combinational_order()`.

## Verification

- `cargo test --lib circuits::hierarchical_builder` — 5 passed, 0 failed.
- `cargo test --lib compile::hierarchy` — 13 passed, 0 failed (untouched, confirms no
  regression in the interfaces this task consumes).
- No other test suites were run per the task instructions (they take minutes).

## Notes / deviations from the brief's literal wording

- The brief's task-4-brief.md commit message example uses the trailer
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; the orchestrating task
  instructions for this specific run explicitly required
  `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` instead, which is what the
  actual commit uses.
- No other deviations: all ports, module names, and constant placement follow the
  brief's design notes (`slice`/`adder_row`/`alu4` names, `shift_in`/`cin` on
  `alu4_full`'s `slice`, `alu4` = 4 slices + glue for `alu8`, top = 2×`alu4` with carry
  chained).

## Fix round 1

Review finding: every test added for the four hierarchical circuits (`ripple_adder`,
`alu4_full`, `multiplier4`, `alu8`) only checked *structure* -- `validate()`,
`module_order()`, `flatten()` succeeding, port/instance counts, `combinational_order()`
existing. None of that proves a circuit computes what its name claims; a truth table
was hand-derived once by a reviewer, but a later edit (swapped opcode index, row 2
reusing row 1's zero-tie) would pass every existing test silently. This round adds a
Boolean evaluator for a flat `Netlist` and uses it to prove equivalence against an
independent reference for all four circuits.

### Where the evaluator lives, and why

`eval::evaluate` (new, `src/circuits/hierarchical_builder.rs`, right before `mod tests`)
is `#[cfg(test)] pub(crate) mod eval` inside the same file. It was kept local rather
than promoted to a shared location (e.g. `circuits::netlist_builder` or a new
`compile::eval`) because nothing outside this file's own tests needs to evaluate a
netlist yet -- a tiny, single-purpose evaluator is easier to audit sitting next to its
only callers than as a new module with one user. It is marked `pub(crate)` (not
private) specifically so a later task that also wants to evaluate a `Netlist` can reach
it (`crate::circuits::hierarchical_builder::eval::evaluate`) without duplicating it
first; if that happens, lifting it out to a shared spot at that point is the right
move, noted in its doc comment.

`evaluate(netlist, assignment)` walks `netlist.combinational_order()` (already a valid
topological order), maintaining a `BTreeMap<String, bool>` seeded with the given input
assignment. For each gate in order: `GateKind::Nor(_)` -> true iff no input is true;
`GateKind::Or(_)` -> true iff any input is true. Any other `GateKind` triggers a
`panic!` naming the offending gate and its kind -- per the task's explicit
instruction, so this evaluator can never silently return a wrong answer for a netlist
shape it doesn't understand. (In practice only `Nor`/`Or` ever appear: every hand-built
circuit here is constructed exclusively through `NetlistBuilder`'s
`and_reduce`/`or_reduce`/`not`/`nor` helpers, which only ever emit those two kinds.)

### Reaching `seed.rs`'s private flat helpers

`extra_circuits::{ripple_adder, alu4_full, multiplier4}` live inside
`fragment_synth::seed`'s `#[cfg(test)] mod tests { mod extra_circuits { ... } }`, both
private. Per the task's instruction not to move or modify their logic, the smallest
change that makes them reachable was widening three existing visibility annotations in
`src/compile/fragment_synth/seed.rs` (no other line in any of these three functions
touched):

- `mod tests {` -> `pub(crate) mod tests {`
- `mod extra_circuits {` -> `pub(crate) mod extra_circuits {`
- `fn ripple_adder(bits: usize) -> Netlist {` -> `pub(crate) fn ripple_adder(...)`
- `fn alu4_full() -> Netlist {` -> `pub(crate) fn alu4_full() -> Netlist {`
- `fn multiplier4() -> Netlist {` -> `pub(crate) fn multiplier4() -> Netlist {`

`extra_circuits::alu4` (used to build `alu8`'s per-nibble ALU, but not itself the flat
counterpart being compared against -- `alu8` has no flat counterpart) was left private;
only the three functions actually consumed by the new equivalence tests were widened.
Both `mod tests` and `mod extra_circuits` stay `#[cfg(test)]`-gated as before, so this
changes nothing about non-test builds. The new tests import them as
`crate::compile::fragment_synth::seed::tests::extra_circuits as flat`.

### How ports were matched between the hierarchical and flat netlists

**Inputs** are matched *by name*, not position: every input name is the literal same
string on both sides (`a0..`, `b0..`, `cin`, `s0`/`s1`/`s2`). The one wrinkle is
`alu4_full`: the flat helper declares its opcode inputs as `s2, s1, s0` (that literal
order) while the hierarchical `top` declares `s0, s1, s2`. Since `evaluate` takes a
name-keyed `BTreeMap<String, bool>` assignment rather than a positional vector, this
reordering is irrelevant -- one assignment map, built from either side's own input
list, drives both netlists identically.

**Outputs** are never literally equal by name and so are matched *by position*: the
hierarchical side exposes its declared port names (`s0`, `cout`, `r0`, `zero`, ...)
while the flat side's outputs are whatever generated gate name (`g12`, ...) each
`NetlistBuilder` reduction happened to produce. Both sides were confirmed (by reading
the construction code in `hierarchical_builder.rs` and `seed.rs` side by side) to build
their `outputs` vector in the same semantic order for all three exhaustively-compared
circuits -- e.g. `ripple_adder`: sum bit 0, sum bit 1, ..., final carry, on both sides;
`alu4_full`: `r0, r1, r2, r3, cout, zero` on both sides; `multiplier4`: `o0..o7` in the
same partial-product-row order on both sides. `assert_same_outputs` zips
`hier.outputs`/`flat.outputs` positionally and evaluates each pair.

### Tests added and case counts

All in `src/circuits/hierarchical_builder.rs`, new `#[cfg(test)] mod equivalence`:

- `ripple_adder4_matches_the_flat_builder_exhaustively` -- 9 inputs, all 512 cases,
  against `flat::ripple_adder(4)`.
- `alu4_full_matches_the_flat_builder_exhaustively` -- `specialise_constants()` then
  `flatten()`, 11 inputs, all 2048 cases, against `flat::alu4_full()`.
- `multiplier4_matches_the_flat_builder_exhaustively` -- 8 inputs, all 256 cases,
  against `flat::multiplier4()`.
- `alu8_add_opcode_matches_8_bit_arithmetic_over_a_deterministic_sample` -- `alu8()` has
  no flat counterpart, so opcode is fixed to ADD (`s1=1, s0=1`, the same encoding
  `extra_circuits::alu4`'s `sel[3]` and `hierarchical_builder`'s `alu4_module` both
  use) and 256 deterministic samples are drawn over the 17 remaining inputs
  (`a0..a7, b0..b7, cin`, a 2^17 space), each checked against `(a + b + cin) & 0xFF`
  for `r0..r7` and `(a + b + cin) >> 8` for `cout`.
- `ripple_adder8_matches_8_bit_arithmetic_over_a_deterministic_sample` -- same
  arithmetic-direct approach (not the flat comparison, and not exhaustive: 2^17 cases
  is too slow for a debug build), 256 deterministic samples over `a0..a7, b0..b7, cin`,
  checked against `(a + b + cin) & 0xFF` / carry.

The sample is generated by `deterministic_sample`: `mask = (index * 0x9E3779B1) &
(space - 1)`. Since `0x9E3779B1` is odd and `space` is a power of two, multiplying by
it is a bijection mod `space` (the standard Fibonacci-hashing trick), so this scatters
the first 256 indices across the whole input space as a fixed, reproducible sequence --
never a seeded or random number generator, satisfying the project's determinism rule.

No discrepancy was found: all four circuits agree with their reference on every case
run.

### Commands run

```
cargo test --lib circuits
```
19 passed, 0 failed, 7 ignored (release-only/measurement harnesses, pre-existing and
unaffected), ~29s -- includes all five new `equivalence` tests plus the four existing
`circuits::hierarchical_builder` structural tests.

```
cargo test --lib compile::fragment_synth::seed
```
20 passed, 0 failed, 2 ignored (release-only, pre-existing), ~58s -- confirms the
`pub(crate)` visibility widening in `seed.rs` did not disturb any of its own tests.

Also ran `cargo build --lib` and `cargo test --lib --no-run` to check for new warnings:
the test build compiles clean; the plain (non-test) library build shows pre-existing
dead-code warnings for `xor`/`full_adder_gates`/`full_adder_module` in
`hierarchical_builder.rs` (they're only reachable from `#[cfg(test)]` code, unchanged by
this round) and unrelated warnings elsewhere in the crate -- nothing introduced by this
fix.

### Deferred by ruling

Per the task instructions, the locally-computed always-zero signals (`AND(x, NOT(x))`
inside `slice`/`adder_row`/`multiplier4`'s `top`) were left as-is: two real NOR gates
each, not optimised away.

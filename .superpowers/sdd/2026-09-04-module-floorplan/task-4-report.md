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

# Native + WASM SystemVerilog Compiler Plan

Date: 2026-09-21

## Goal

Replace the Yosys-dependent frontend with a REDA-owned compiler that:

- runs from the same Rust source on native targets and `wasm32-unknown-unknown`;
- compiles a synthesizable IEEE 1800-2023 SystemVerilog subset into REDA's existing gate-level `Netlist`;
- reports source-located diagnostics suitable for an online editor;
- supports hot reload without reloading the page or discarding the last valid circuit;
- preserves enough provenance to trace source expressions and signals through
  logical gates, physical cells, routes, and Minecraft blocks;
- initially compiles SystemVerilog versions of the `and4`, `seven_segment`,
  and positive-edge DFF fixtures without Yosys;
- leaves placement, routing, simulation, and rendering unchanged.

There is one language mode: SystemVerilog. There is no separate Verilog-2005
mode, version switch, or second parser. Legacy syntax that remains valid
SystemVerilog may work, but REDA documents and tests the modern syntax only.
The target is a useful synthesizable subset, not full IEEE 1800-2023 compatibility.
The long-term language target is synthesizable SystemVerilog, not the complete
simulation, verification, class, DPI, or testbench language.

## Project status and integration rule

As of 2026-09-21, the positive-edge DFF path is implemented end to end:

```text
Yosys $_DFF_P_
  -> GateKind::DffPosedge with [D, C] pins
  -> lowering and feedback boundary
  -> primitive graph and Design H macro
  -> placement, verification, and simulation
```

The generator is still being completed. Compiler development therefore uses
the existing `Netlist` as a hard isolation boundary:

- frontend milestones may add source parsing, elaboration, logic optimisation,
  netlist emission, and tests now;
- they must not change `Netlist`, `GateKind`, lowering, placement, routing,
  simulator, baked netlists, or physical baselines;
- Yosys remains the production frontend while the generator is changing;
- production cutover and artifact regeneration wait until the generator is
  declared stable.
- the existing `synthesize_verilog` API and all of its callers remain unchanged
  until production cutover; the new API is added beside it.

This lets compiler and generator work proceed independently. If a compiler
feature appears to require a downstream representation change, stop and make
that contract change explicit instead of silently coupling the two projects.

### Implementation checkpoint: 2026-09-21

Milestones 0 through 4 are implemented behind the additive
`compile_systemverilog` API, all still beside the unchanged
`synthesize_verilog` Yosys path:

- **M0** (contract): `SourceInput`, `CompileOptions`, `CompileArtifact`,
  typed IDs, `DebugDatabase`, the canonical Netlist fingerprint, and the
  logical evaluator all exist and are exercised in
  `tests/systemverilog_compiler.rs` and `tests/logical_evaluator.rs`.
- **M1** (`and4.sv`): compiles; Tier A/B pass exhaustively; forward and
  reverse source-to-gate mapping is proven, including the comment-span-move
  case (`tests/systemverilog_compiler.rs`).
- **M2** (`seven_segment.sv`): packed vectors, `always_comb`, `case`/
  `default`, width checking, and the rejection matrix are implemented and
  tested (`tests/systemverilog_seven_segment.rs`); Tier B against the baked
  Yosys netlist is in `tests/m4_shadow_validation.rs`.
- **M3** (positive-edge state): implemented, including the enable form. The
  shipped `always_ff` slice accepts one unconditional nonblocking assignment
  (`q <= d;`) or one enable-guarded assignment (`if (en) q <= d;`) per
  clocked variable, and rejects everything else (reset, `else`, `case`,
  multiple writes, feedback in the enable condition). The enable form lowers
  to a hold mux exactly as this document's "Scope of version 1" section
  describes: `elaborate::always_ff_assignment` builds `en ? d : q` and tags
  it `debug::SyntheticOrigin::HoldMux`; `logic::LogicGraph::reserve_dff` /
  `finalize_dff` reserve the register's own node identity before its `d`
  operand is blasted, so the mux's self-read of `q` resolves to the DFF
  instead of tripping the combinational-loop check; `netlist::resolve`
  recurses on demand to emit the DFF's gate around that one back-reference.
  `tests/systemverilog_dff.rs` covers both forms: gate shape, step
  semantics across the enable truth table, debug-database provenance for
  the hold mux, determinism, and the narrowed rejection matrix (`else`,
  feedback in the assignment, feedback in the enable condition).
- **M4** (shadow validation): implemented in `tests/m4_shadow_validation.rs`,
  `tests/systemverilog_fuzz.rs` (10,000+ generated expressions plus
  arbitrary-byte fuzzing), and `tests/netlist_fingerprint_independence.rs`
  (two independent fingerprint oracles). All three corpus fixtures are
  semantically equivalent to their Tier A/B oracles; the consolidated
  size/cost report (`target/m4_shadow_report.txt`) is generated but asserts
  nothing about the numbers, per the plan. Measured 2026-09-21: `and4` is
  gate-for-gate identical to Yosys (3 gates both frontend and lowered
  stages); `seven_segment` is not -- REDA emits 82 frontend gates lowering to
  94, against Yosys/ABC's 31 lowering to 47 (+51 frontend, +47 lowered). This
  gap is exactly the plan's stated "optimisation gap" risk, reported here and
  in the harness's own output, not yet reviewed or bounded by the generator
  owner (M5 exit criteria 5 and 6, below, are unmet).
- **M4 timing evidence (local sample, 2026-09-22)**: the ignored
  `tests/compile_timing_harness.rs` target now measures the REDA frontend,
  `compile::lowering::lower`, and the existing physical `compile` path for
  all four checked-in REDA fixtures (`and4`, `seven_segment`, `dff`, and
  `dff_enable`). It prints an explicit `ok`/`FAIL`/`SKIP` status beside each
  duration and fails the harness if any stage fails; it compares no duration
  against a threshold. This is a local wall-clock sample, not a CI gate:
  host load, allocator state, compiler profile, and placement/routing
  heuristics make the durations variable. The exact command and one native
  debug-profile sample (arm64, rustc 1.98.1) were:

  ```text
  cargo test --test compile_timing_harness -- --ignored --nocapture

  fixture=and4.sv top=and4 gates=3 frontend=ok (5.240667ms) lowering=ok (271.583µs) physical=ok (85.512ms)
  fixture=seven_segment.sv top=seven_segment gates=82 frontend=ok (701.333µs) lowering=ok (216.625µs) physical=ok (4.083566709s)
  fixture=dff.sv top=dff gates=1 frontend=ok (86.166µs) lowering=ok (11.25µs) physical=ok (1.384167ms)
  fixture=dff_enable.sv top=dff_enable gates=2 frontend=ok (45.334µs) lowering=ok (14.916µs) physical=ok (29.435792ms)
  test result: ok. 1 passed; 0 failed
  ```

  A later run is expected to produce different durations; the durable evidence
  in this target is the fixed fixture order and explicit stage outcome. The
  `m4_shadow_validation` cost report remains a frontend/lowering report and
  does not substitute a zero-duration baked-Yosys placeholder for physical
  timing.
- **Tier C physical bridge**: `tests/m5_tier_c_bridge.rs` now feeds the REDA
  frontend's own `Netlist` through lowering, placement, routing, and the real
  redstone simulator. Its four tests pass locally for `and4`,
  `seven_segment`, unconditional `dff`, and enable-gated `dff_enable`.
  The local acceptance matrix below also exercises the Yosys-backed
  `tests/verilog_frontend.rs` path using
  `REDA_PYTHON=/private/tmp/reda-uv/bin/python3`; it passed 5/5 in both
  recorded passes. These are local runs and are not evidence of two
  consecutive green CI runs.
  The physical DFF currently powers on with `q=1`, while the logical evaluator
  initializes `q=0`; this semantic difference remains unresolved. The Tier C
  DFF tests use a priming rising edge with `d=0` before checking the trace, so
  the test setup makes the trace deterministic but does not claim to fix or
  waive the power-on mismatch.

The crate passes `cargo test --release` on native. `cargo check --target
wasm32-unknown-unknown` for the root `reda` crate now **passes**:
`compile::fragment_synth::benchmark::atomic_publish` previously had no
wasm32 arm (`#[cfg(windows)]` and `#[cfg(unix)]` only), which had left the
whole crate failing to typecheck for that target
(`error[E0425]: cannot find function atomic_publish`) and the same break
reaching `viewer`'s `wasm-pack build`; that gap has since been closed by a
`#[cfg(not(any(unix, windows)))]` fallback arm added in unrelated
fragment-synthesis work, not by this packet. `check.sh` now runs `cargo
check -p reda --target wasm32-unknown-unknown` directly (see below) so a
regression here stops being visible only by chance, through whichever
viewer code path happens to reach it.

No downstream `Netlist`, `GateKind`, lowering, placement, routing, simulator,
generator, viewer session, or baked artifact has moved. Production callers
still use `synthesize_verilog` exclusively.

#### Milestone 5 readiness

Milestone 5 (switch production callers) has **not** started. Checking this
document's own cutover checklist against the current state:

1. Generator revision fingerprint / physical baseline stability across two
   consecutive merged generator changes -- **not evaluated by this packet**;
   unrelated to the frontend work above.
2. Generator, DFF, timing, routing, and physical verification suites passing
   on the cutover revision -- **not evaluated by this packet**.
3. Tiers A, B, and C passing for all three fixtures on two consecutive CI
   runs -- Tiers A and B pass locally for all three fixtures (above), and the
   REDA frontend's Tier C bridge passes locally for `and4`, `seven_segment`,
   `dff`, and `dff_enable`. The fresh Yosys-backed
   `tests/verilog_frontend.rs` and the fresh-Yosys DFF differential each pass
   in both local acceptance passes with
   `REDA_PYTHON=/private/tmp/reda-uv/bin/python3`. The timing harness also
   passes all four fixtures in both passes. These are local results, not two
   consecutive CI runs; that CI-process criterion remains unmet. The DFF
   power-on difference (`q=1` in the physical simulator versus `q=0` in the
   logical evaluator) is also unresolved; priming-edge setup in the Tier C
   tests does not change that conclusion.
4. Milestone 4 completing with >=10,000 expression cases, zero semantic
   differences, zero panics -- **met**, see M4 above.
5. Lowered seven-segment gate count within an explicitly accepted bound of
   the Yosys result -- **unmet**: no bound has been proposed or accepted; the
   measured gap is reported above and in
   `tests/m4_shadow_validation.rs::m4_consolidated_size_and_cost_report`.
6. Generator owner recording the accepted revision and fingerprints in this
   document -- **unmet**; no such record exists yet.
7. A bridge test compiling `and4`, joining every gate-owned primitive back
   through `(gate index, output)` to a source span, joining ports by name,
   and rejecting a deliberately mismatched Netlist fingerprint -- **met** by
   `tests/m5_and4_bridge.rs`, added in this packet.

Criteria 1, 2, 3 (the "two consecutive runs" half), 5, and 6 remain unmet and
are outside this packet's scope (no `synthesize_verilog` caller changes, no
Yosys removal, no baked-netlist regeneration, no feature cutover). This
packet's own mandate is preparation only: it adds the provenance M5 bridge and
the REDA-frontend Tier C physical bridge, records the M3/M5 gaps above
honestly, and adds the missing wasm check to `check.sh`. It does not attempt,
and this document does not claim, that Milestone 5 may begin.

#### Local acceptance record: 2026-09-22 (latest working-tree revision)

The following exact four-command acceptance matrix completed successfully
twice consecutively in the same local worktree on 2026-09-22. Both passes had
zero failures. In each pass, command A ran 62 selected native compiler,
evaluator, shadow, provenance, fuzz, rejection-matrix, and Tier C bridge tests;
command B ran the fresh-Yosys DFF differential (1 test) plus the Yosys-backed
frontend tests (5 tests); command C ran the ignored timing harness (1 test,
with `and4`, `seven_segment`, `dff`, and `dff_enable` frontend/lowering/
physical stages all `ok`); and command D checked the native crate for
`wasm32-unknown-unknown`:

```sh
cargo test --test systemverilog_compiler --test logical_evaluator --test systemverilog_seven_segment --test systemverilog_dff --test systemverilog_fuzz --test m4_shadow_validation --test netlist_fingerprint_independence --test m5_and4_bridge --test m5_tier_c_bridge
REDA_PYTHON=/private/tmp/reda-uv/bin/python3 cargo test --test m3_m4_dff_yosys_differential --test verilog_frontend
cargo test --test compile_timing_harness -- --ignored --nocapture
cargo check -p reda --target wasm32-unknown-unknown
```

Pass 1 and pass 2 were both local executions, not CI jobs. They therefore
provide useful local acceptance evidence but cannot satisfy M5 exit criterion
3, which explicitly requires Tiers A, B, and C to pass on two consecutive CI
runs. The M5 two-consecutive-CI requirement remains unmet.

## Current boundary

The existing pipeline is:

```text
SystemVerilog source
  -> Python + yowasp-yosys
  -> Yosys JSON
  -> frontend/yosys_json.rs
  -> compile::Netlist using GateKind
  -> lowering
  -> placement/routing
  -> simulator
  -> viewer
```

Only the first three steps need replacement. `Netlist` is already the correct
boundary: the rest of REDA consumes it on both native and WASM builds.

The replacement is a traceable multi-pass pipeline:

```text
source set
  -> preprocessing seam
  -> lexer
  -> parser AST
  -> typed hierarchical IR
  -> transient RTL expressions
  -> bit-level logic graph
  -> local optimisation
  -> GateKind mapping
  -> compile::Netlist
  -> primitive graph
  -> placed cells and routes
  -> Minecraft world

each transformed node retains one parent origin; merges, folds, and removals emit edges
  -> DebugDatabase / PhysicalDebugArtifact
```

Compiler stages through `Netlist` are pure Rust functions over owned data. They
must not read files, spawn processes, inspect environment variables, or use
browser APIs. Version 1 does not implement preprocessing, hierarchy, or a rich
word-level RTL vocabulary, but the IDs and provenance contract must not assume
one file, one instance, or a direct AST-to-gate relationship.

The preprocessing seam later owns macros, includes, and expansion origins.
Typed hierarchical IR later owns module instances, parameters, generates,
types, and instance paths. RTL IR later preserves registers, memories,
arithmetic, and muxes before bit-level lowering. Do not prematurely bit-blast
future memories or wide arithmetic merely to reuse the version 1 graph.

## Pass model

This is not a two-pass compiler. Name resolution uses two traversals -- first
collect declarations and module signatures, then resolve bodies -- but the
complete compiler has multiple explicit passes:

1. preprocess and retain expansion origins;
2. lex and parse;
3. collect declarations;
4. resolve names and types;
5. elaborate parameters, hierarchy, and generate constructs;
6. lower procedural code to RTL;
7. optimise word-level RTL;
8. bit-blast supported operations;
9. optimise logic;
10. emit and validate `Netlist`;
11. select physical implementations, place, and route.

Version 1 actually performs lexing, parsing, declaration collection,
name/width resolution, symbolic execution, bit-level construction with
interning, liveness marking, and Netlist emission. Preprocessing, hierarchy,
word-level optimisation, and physical generation remain explicit future or
existing downstream passes rather than pretend version 1 implementations.

## Debug and provenance contract

Source-to-cell debugging is a first-class compiler output, not something to
reconstruct from the final Netlist. Transformations are many-to-many: one
expression may split into many gates, several expressions may share one gate,
and a folded expression may produce no gate at all.

Every persisted compiler entity receives a layer-specific, deterministic,
artifact-local ID. Transient RTL expressions do not. IDs and gate names must
not depend on whitespace or comments. They are stable for identical syntax and
options, but are not promised to survive arbitrary source edits.

```rust
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
    pub expansion: Option<ExpansionId>,
}

pub struct SourceNodeId(pub u32);
pub struct ElabNodeId(pub u32);
pub struct LogicNodeId(pub u32);

pub struct GateRef {
    pub index: u32,
    pub output: String,
}

pub struct LogicOrigin {
    pub elab: ElabNodeId,
    pub bit: u32,
    pub synthetic: Option<SyntheticOrigin>,
}

pub enum SyntheticOrigin {
    HoldMux,
    BranchJoin,
    CaseFold,
    Passthrough,
}
```

Each elaborated-node record contains its `SourceNodeId`, kind, width, and
instance path. Version 1 stores one root instance without building a path
interner. Later, an elaborated identity becomes source node plus instance path;
parameter values live on that path. `Span::expansion` is `None` in version 1;
the future expansion table records invocation and spelling spans.

The compiler emits a sidecar `DebugDatabase` rather than adding source fields
to `Netlist`:

```rust
pub enum BitBinding {
    Logic(LogicNodeId),
    Const(bool),
    Input(String),
}

pub struct SignalBinding {
    pub elab: ElabNodeId,
    pub bits: Vec<BitBinding>,
}

pub enum Realisation {
    Gate(GateRef),
    Input(String),
    Dead,
    Folded(LogicNodeId),
    Const(bool),
}

pub enum DebugNode {
    Source(SourceNodeId),
    Elab(ElabNodeId),
    Logic(LogicNodeId),
    Gate(u32),
}

pub enum TransformReason {
    Fold,
    Cse,
    Dead,
    Map,
}

pub struct Transformation {
    pub reason: TransformReason,
    pub inputs: Vec<DebugNode>,
    pub outputs: Vec<DebugNode>,
}

pub struct DebugDatabase {
    pub files: Vec<SourceFileInfo>,
    pub source_nodes: Vec<SourceNode>,
    pub elab_nodes: Vec<ElabNode>,
    pub logic_origins: Vec<LogicOrigin>,
    pub extra_origins: Vec<(LogicNodeId, LogicOrigin)>,
    pub realisations: Vec<Realisation>,
    pub transformations: Vec<Transformation>,
    pub signals: Vec<SignalBinding>,
    pub netlist_fingerprint: Fingerprint,
}
```

Each persisted transformed node embeds exactly one origin pointing to its
parent layer. Only many-to-many events use side tables: an interning hit
appends an extra origin; folding records the surviving constant or operand;
dead logic stays in the arena with a liveness mark and a `Dead` record while
the emitter skips it. Origins never participate in the structural-hashing key,
so identical logic from different spans can share one node without losing
either source.

Synthetic RTL or logic created for a hold mux, branch join, case fold, or
passthrough carries a compact `SyntheticOrigin` plus its owning elaborated
node. Bit expansion is represented directly by `LogicOrigin::bit`, not by a
separate transformation row.

The minimum supported queries are bidirectional:

- source span or elaborated signal bit -> logic nodes -> Netlist gates;
- Netlist gate or signal -> every direct and extra source origin;
- eliminated source node -> reason and surviving realization, if any;
- later, gate -> primitive instance -> placed cell -> blocks and routes;
- later, placed cell or block -> gate, signal, instance path, and source.

The compiler-generator bridge is `GateRef { index, output }`, not a name alone.
Both `DebugDatabase` and the future `PhysicalDebugArtifact` carry the same
canonical Netlist fingerprint and reject a mismatched pairing. This matches
REDA's existing downstream identity chain: lowering retains source-gate
provenance, and primitive generation, placement, routes, and observations
already use gate, port, primitive, and route IDs. `Netlist` therefore needs no
new ID field while generator work continues.

After the generator freezes, `PhysicalDebugArtifact` serialises those existing
maps and adds physical-only reasons such as polarity selection, cell expansion,
timing-repeater insertion, and routing repair. Synthetic physical components
record a reason instead of inventing a source span. Optional full IR snapshots
may be added later; compact origins, signal bindings, transformations, and the
fingerprint are always produced from version 1 onward.

## Scope of version 1

Version 1 must compile migrated `and4.sv`, `seven_segment.sv`, and `dff.sv`
fixtures.

Supported syntax:

- one selected top module;
- ANSI-style module ports;
- `input`, `output`, `wire`, and `logic` declarations;
- scalar signals and packed vectors with constant ranges;
- `assign` statements;
- `always_comb` combinational blocks;
- `always_ff @(posedge clock)` with nonblocking assignment;
- `begin`/`end`, `if`/`else`, and `case`/`default`;
- blocking assignment inside combinational blocks;
- identifiers, bit selection, constant part selection, and concatenation;
- binary and decimal integer literals with explicit width;
- `~`, `!`, `&`, `|`, `^`, `&&`, `||`, `==`, `!=`, and ternary expressions;
- parentheses and normal SystemVerilog operator precedence.

Version 1 is unsigned and deliberately stricter than general SystemVerilog:

- every vector declaration must use descending `[N-1:0]` form;
- literals must have an explicit width and fit without truncation;
- all assignment, bitwise, equality, ternary-branch, and `case` widths must
  match exactly;
- no implicit extension, truncation, or signed conversion is performed;
- the clock of an `always_ff` must be a top-level input port.

Explicitly rejected in version 1:

- module instances and parameters;
- macros, includes, and compiler directives;
- signed arithmetic and four-state `x`/`z` semantics;
- memories, arrays, generate blocks, tasks, and functions;
- delays and simulation-only constructs;
- inferred latches;
- latches, asynchronous resets, negative-edge clocks, multiple clocks, and
  sequential processes other than the supported positive-edge `always_ff`.
- legacy `reg`, `always @*`, and `always @(posedge ...)` syntax; checked-in
  SystemVerilog fixtures use `logic`, `always_comb`, and `always_ff`.

The initial sequential subset is deliberately narrow: one positive-edge clock,
one writer per variable, no reset, and no mixed blocking/nonblocking writes.
Each assigned bit becomes one `GateKind::DffPosedge` whose inputs are ordered
`[D, C]`. Wider state is only a vector of independent DFF bits; it does not
need a second state representation.

An incomplete assignment in `always_ff` means hold: `if (en) q <= d` becomes
`D = en ? d : q`. An incomplete assignment or read-before-write in
`always_comb` is an error because it would infer storage.

## Source layout

Keep this inside the existing `reda` crate until the API stabilises:

```text
src/frontend/
  mod.rs              public API and shared diagnostics
  source.rs           source files, FileId, Span, and expansion origins
  lexer.rs            tokens and source spans
  parser.rs           SystemVerilog subset to AST
  ast.rs              syntax-only data
  elaborate.rs        declarations, types, hierarchy, and instance paths
  rtl.rs              transient word-level expressions from symbolic execution
  logic.rs            canonical bit-level graph and optimisation
  netlist.rs          logic graph to compile::Netlist
  debug.rs            provenance graph and bidirectional indices
  evaluate.rs         pure logical Netlist evaluator used by verification
  yosys_json.rs       temporary native differential oracle
  synth.py            temporary native differential oracle
```

Do not create `reda-ir`, `reda-verilog`, or a Cargo workspace yet. `Netlist`
and `GateKind` currently live in `reda`; extracting them solely to make a new
frontend crate would create a large move without improving the first compiler.
Split only if independent reuse or compile-time measurements later justify it.

Add `and4.sv`, `seven_segment.sv`, and `dff.sv` beside the existing `.v`
fixtures. Do not rename or remove the `.v` files, change the Verilog catalog,
or regenerate baked files before production cutover: their paths are part of
the current artifact headers and tests.

## Public API

The cross-platform entry point should contain no target-specific types:

```rust
pub struct CompileOptions {
    pub top: String,
}

pub struct SourceInput<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

pub struct CompileArtifact {
    pub netlist: Netlist,
    pub ports: Vec<PortBinding>,
    pub debug: DebugDatabase,
}

pub fn compile_systemverilog(
    sources: &[SourceInput<'_>],
    options: &CompileOptions,
) -> Result<CompileArtifact, Vec<Diagnostic>>;
```

Diagnostics need stable source positions from the first commit:

```rust
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
    pub expansion: Option<ExpansionId>,
}

pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
}
```

Byte offsets are the canonical representation. Native and browser hosts can
derive line and column without storing two coordinate systems in every token.
`CompileArtifact::ports` is sorted by port name for deterministic transport.
Version 1 normally receives one `SourceInput`; accepting a slice now avoids
changing every diagnostic and debug identifier when includes and multi-file
projects arrive.

Keep the current `synthesize_verilog` Yosys path and its callers unchanged.
Tests use it as an oracle; production callers switch to
`compile_systemverilog` only after parity and generator sign-off.

## Compiler stages

### 1. Lexer

Write a small hand-rolled lexer. The required token set is limited and no
existing dependency solves only this problem more cheaply.

Requirements:

- every token carries a `Span`;
- whitespace, line comments, and block comments are skipped;
- malformed literals and unterminated comments produce diagnostics;
- keywords remain distinct tokens;
- the lexer never panics on arbitrary UTF-8 input.

First acceptance test: tokenise `and4.sv` and preserve exact spans for `module`,
the four inputs, the output, and the assignment expression.

### 2. Parser

Use recursive descent for declarations and statements, plus a Pratt parser for
expressions. Version 1 stops at the first lexer or parser error and returns a
single diagnostic inside `Vec<Diagnostic>`; do not build syntax recovery yet.

The AST preserves syntax, `SourceNodeId`, and spans. It does not contain REDA
gates.
Elaboration may accumulate independent item-level errors, then sorts all
diagnostics by starting byte offset for deterministic output. Version 1 emits
errors only; keep `Severity` for the future without inventing warnings.

First acceptance test: parse the three `.sv` fixtures, then reject a missing
semicolon with one diagnostic pointing at the following token.

### 3. Elaboration

Elaboration selects the top module and converts syntax into typed hierarchical
IR. Version 1 has only the root instance, but still assigns its explicit
`InstancePathId`:

- collect declarations and module signatures before resolving any body;
- resolve names and reject duplicates or unknown identifiers;
- calculate packed-vector widths;
- validate assignment widths;
- create an `ElabNode` for every typed declaration and expression, recording
  its `SourceNodeId`, width, kind, and root instance path.

Use LSB-first bit vectors internally because the current Yosys bridge and
`Netlist` port map already use that convention.

Width checking is a deliberately strict bottom-up pass. It implements only
the following unsigned rules and does not claim general IEEE context sizing:

| Expression | Result width | Version 1 rule |
| --- | ---: | --- |
| identifier | declared width | declaration must use `[N-1:0]` |
| `N'b...`, `N'd...` | `N` | value must fit; `_` allowed; `x`, `z`, `?` rejected |
| `a[i]` | 1 | constant index in range |
| `a[h:l]` | `h-l+1` | constant descending range in bounds |
| `{a, b, ...}` | sum of operands | no replication |
| `~a` | width of `a` | unchanged |
| `a & b`, `a \| b`, `a ^ b` | operand width | operand widths must match |
| `a == b`, `a != b` | 1 | operand widths must match |
| `!a`, `a && b`, `a \|\| b` | 1 | each operand is OR-reduced to truth |
| `s ? a : b` | branch width | branch widths must match; `s` is reduced to truth |
| assignment | LHS width | RHS width must match exactly |
| `case` item | selector width | item width must match exactly |

This rejects some legal SystemVerilog that Yosys extends or truncates. Such a
rejection is intentional and belongs in the documented rejection matrix, not
in the equivalence-failure count. Add context propagation later only when a
real supported design requires it.

### 4. RTL lowering

Procedural lowering uses symbolic execution. An environment maps each variable
to an RTL value; sequential statements overwrite the environment; `if` and
`case` clone it and join changed values with muxes. `always_comb` requires every
written value to be defined on every path. All nonblocking RHS values in
`always_ff` read the pre-edge environment, then commit simultaneously, so
`a <= b; b <= a;` swaps state correctly. Version 1 RTL is a transient
`RtlExpr` vocabulary containing signals, constants, bitwise operations, muxes,
and registers. It carries `ElabNodeId` plus optional `SyntheticOrigin`, but has
no separate RTL ID arena until word-level optimisation or memories require it.

`case` lowering starts with the `default` value and folds arms into muxes in
reverse source order. This gives deterministic priority semantics and handles
the seven-segment fixture without a separate process engine. Version 1 requires
`default` and rejects `casez`, `casex`, `unique`, `priority`, and multi-label
arms.

Hold muxes, branch joins, and case folds carry an embedded `SyntheticOrigin`;
they do not need separate transformation rows.

### 5. Bit-level logic graph

Use a compact graph with stable integer node IDs:

```text
Input(name)
Const(false | true)
Not(a)
And(a, b)
Or(a, b)
Xor(a, b)
Mux(select, when_false, when_true)
Dff(data, clock)
```

Intern nodes while constructing them. Each constructor performs cheap local
simplification:

- constant folding;
- `x & x`, `x | x`, and `x ^ x`;
- double negation;
- commutative operand ordering;
- muxes with equal branches or constant selectors.

After output roots are known, mark unreachable nodes dead but retain them in
the arena for debugger queries; Netlist emission skips them. Do not build an
ABC replacement in version 1. Structural hashing plus local identities are
enough to establish the cross-platform compiler and hot-reload loop.

Constants are internal graph values. They should disappear through folding in
the initial supported designs. A design whose final output is constant must
receive an unsupported diagnostic until REDA has an explicit physical constant
driver in `Netlist`.

Every constructor and rewrite also updates provenance. Structural interning
keeps the first origin and appends later ones to `extra_origins`; constant
folding and dead marking record reasons; bit expansion stores the source bit in
`LogicOrigin`.

### 6. Gate mapping

Map graph nodes directly to existing `GateKind` values:

- `Not` to `GateKind::Nor(1)`;
- `And` to `GateKind::And`;
- `Or` to `GateKind::Or(2)`;
- `Xor` to `GateKind::Xor`;
- `Mux` to `GateKind::Mux`;
- passthrough outputs to `GateKind::Buf` when REDA requires a gate-produced
  output signal;
- `Dff` to `GateKind::DffPosedge` with inputs `[D, C]`.

Do not emit `Nand`, `Xnor`, `AndNot`, `OrNot`, `Nmux`, `Aoi*`, `Oai*`, or
`Nor(2)` in version 1. Downstream polarity assignment and optimised lowering
already own those choices.

Use the existing `NetlistBuilder` for deterministic gate/output naming rather
than inventing a second naming policy. Preserve top-level input declaration
order. If multiple output ports reference one graph node, emit that node once
and give the sorted output map multiple port-name entries. Validate the
resulting `Netlist`: every input exists, every output has one producer, gate
outputs are unique, and combinational dependencies are acyclic when DFFs are
treated as state boundaries.

Do not lower to NOR here. `compile::lowering` already owns that decision and
its redstone cost model.

Each emitted node records a `Realisation`, and every gate is referenced by
both its Netlist index and output name. The completed canonical Netlist
fingerprint is copied into the debug sidecar before returning the artifact.

### 7. Logical evaluator

Add a pure Netlist-level verification utility in `frontend/evaluate.rs`. It
uses `Netlist::combinational_order` and `GateKind::evaluate`; it is not part of
the compiler pipeline and does not modify `src/compile`.

The evaluator:

- evaluates combinational outputs without placement or routing;
- initializes every DFF Q to zero;
- on `step`, settles logic using the old Q values, detects each clock's 0-to-1
  transition, and commits every triggered DFF simultaneously;
- rejects undriven nets, illegal combinational cycles, and malformed DFFs;
- supports exhaustive truth-table and bounded sequential-trace comparison.

Use this same machine for REDA-versus-independent-spec and
REDA-Netlist-versus-Yosys-Netlist tests. Do not add a second evaluator for the
temporary bit-level graph.

## Native and WASM hosts

The compiler core stays identical on both targets. Hosts only provide source
text and consume the result.

### Native

- CLI reads an `.sv` file and calls `compile_systemverilog`.
- Existing build tools migrate from `VerilogCircuit::synthesize` to the new
  function after differential tests pass.
- Yosys remains optional and test-only during migration.

### WASM

- `viewer` exports a wrapper accepting source and top-module strings.
- The wrapper converts `CompileArtifact` and diagnostics to JavaScript values.
- No virtual filesystem, subprocess shim, or Yosys WASM bundle is needed.
- Full REDA placement and simulation continue through the existing Rust/WASM
  code.

Target-specific `cfg` blocks belong only around the temporary Yosys oracle and
host wrappers, never inside lexer, parser, elaboration, or logic passes.

## Hot reload design

Hot reload is a later host feature, not a compiler pass. The compiler API stays
pure and synchronous so a browser can wrap it in a worker and a native host can
wrap it in a watcher. Revision handling, cancellation, session replacement,
camera preservation, and performance targets belong in a separate Milestone 6
document after the compiler and generator are stable; version 1 does not build
an incremental compiler.

## Verification

Use three oracle tiers, all comparing named output ports rather than textual
gate graphs:

| Tier | Oracle | Environment | Purpose |
| --- | --- | --- | --- |
| A | independent specifications: `and4` predicate, seven-segment truth table, documented DFF trace | native and WASM; no Yosys | REDA compiler correctness |
| B | REDA Netlist versus Yosys Netlist through the same logical evaluator | native CI with Yosys | frontend equivalence |
| C | existing physical simulator tests | native; slow | end-to-end sanity |

Tier B exhaustively tests combinational inputs up to 16 bits. DFF comparison
starts from all-zero state and uses bounded sequences that explicitly toggle
input clocks. Designs depending on `x` propagation are outside the subset.

The rejection matrix covers at least: width mismatch, missing `default`,
read-before-write in `always_comb`, incomplete combinational assignment, dual
writer, `negedge`, asynchronous reset, unsized literal, ascending range, and
derived clock. Each case asserts one stable diagnostic and source span.

Add arbitrary-byte no-panic fuzzing and generated small-expression Tier B
comparison. Check determinism by rendering the same output twice and comparing
bytes.

The compatibility contract is semantic, not structural. The REDA compiler is
allowed to emit a different gate graph from Yosys when all combinational truth
tables and sequential traces agree. Determinism is checked separately: the
same source and options must always produce byte-for-byte identical serialized
netlist data.

Cross-platform checks before the editor are `cargo test` on native and
`cargo check --target wasm32-unknown-unknown`. Viewer wrapper and browser tests
wait for Milestone 6.

Provenance tests are behavioral API tests, not snapshots of internal tables:

- `gates_for_span` maps source expressions forward;
- `origins_for_gate` maps shared gates back to every origin;
- `signal_bits` distinguishes logic, constant, and primary-input bits;
- `why_missing` explains folded and dead source nodes;
- inserting comments may move byte spans but does not change semantic IDs,
  Netlist gate names, or the Netlist fingerprint.

## Delivery order

### Milestone 0: Freeze the boundary and debug contract

- Add `SourceInput`, `CompileOptions`, `CompileArtifact`, `Span`, and
  `Diagnostic`.
- Add the typed compiler IDs, compact `DebugDatabase`, four query methods, and
  canonical Netlist fingerprint. No physical debug artifact yet.
- Add `compile_systemverilog` as the new pure entry point.
- Leave `synthesize_verilog` and all existing callers unchanged.
- Add the pure logical Netlist evaluator and DFF stepper.
- Add native and WASM compile checks before language work begins.

Exit: empty/minimal module returns a typed unsupported diagnostic on both
targets; the evaluator reproduces the independent seven-segment truth table
from the existing baked Netlist; the old Yosys path still passes existing
tests; an empty debug database serializes identically twice.

### Milestone 1: `and4.sv`

- Lexer, parser, `logic` declarations, continuous assignment, and bitwise AND.
- Logic graph, deterministic gate mapping, Buf passthrough, and output map.
- Exhaustive 16-row Tier A and Tier B comparison.
- Verify forward/reverse mapping for the complete expression and one nested
  subexpression; verify output `y` resolves to the root gate.
- Insert a comment and verify only spans move: semantic IDs, gate names, and
  Netlist fingerprint remain unchanged.

Exit: `and4.sv` compiles and its logical truth table passes without Python or
Yosys; the compiler still builds for WASM.

### Milestone 2: `seven_segment.sv`

- Packed vectors, concatenation, literals, `always_comb`, `case`, and default.
- Strict width checking, symbolic execution, latch analysis, and mux lowering.
- Exhaustive 16-row, seven-output Tier A and Tier B comparison.
- Complete version 1 rejection matrix with stable spans.
- Verify all seven output signal-bit bindings.
- Verify CSE gives one gate multiple origins, folding remains explainable, and
  dead source logic remains queryable although no gate is emitted.

Exit: both checked-in SystemVerilog circuits compile through the REDA frontend.

### Milestone 3: Positive-edge state

- Parse `always_ff @(posedge clk)` and nonblocking assignment.
- Enforce one top-level input clock, one writer, no reset, and complete width
  agreement; lower incomplete assignment to a hold mux.
- Add `Dff` nodes to the bit-level graph and emit `GateKind::DffPosedge` with
  `[D, C]` inputs.
- Add `dff.sv` beside the unchanged `dff.v` fixture.
- Compare logical state traces against the independent specification and
  Yosys. Run the existing physical DFF simulator test as a shadow check, not a
  compiler milestone gate while generator code is changing.
- Verify `q` resolves to the `DffPosedge` gate and an enable-style incomplete
  assignment records a `HoldMux` origin at its `if` statement.

Exit: `dff.sv` passes Tier A and Tier B trace equivalence from all-zero state;
the compiler still builds for WASM.

### Milestone 4: Shadow validation

- Keep all production callers on Yosys.
- Run the REDA compiler beside Yosys for the checked-in corpus and report
  semantic, size, and lowering-cost differences.
- Fuzz at least 10,000 small valid expressions and compare exhaustive truth
  tables; fuzz arbitrary bytes for panics.
- Check deterministic diagnostics and serialized netlists.
- Verify the debug sidecar fingerprint against an independent canonical render
  of every emitted Netlist.
- Record frontend time beside physical compile time without changing baked
  netlists or generator baselines.

Exit: every supported checked-in design is semantically equivalent; no
semantic difference or frontend panic is found by the required fuzz runs.

### Milestone 5: Switch production callers

- Make `VerilogCircuit::synthesize` call the REDA SystemVerilog compiler.
- Keep the Yosys path only as a native differential oracle.
- Regenerate baked netlists and review size/latency changes explicitly.
- Update user-facing errors to use source diagnostics.

Production cutover is a no-go until all of these are true:

1. Generator revision fingerprints and physical baseline files remain stable
   across two consecutive merged generator changes.
2. Generator, DFF, timing, routing, and physical verification suites pass on
   the cutover revision.
3. Tiers A, B, and C pass for all three fixtures on two consecutive CI runs.
4. Milestone 4 completes with at least 10,000 expression cases, zero semantic
   differences, and zero panics.
5. The lowered seven-segment gate count is within an explicitly accepted bound
   of the Yosys result.
6. The generator owner records the accepted revision and relevant descriptor
   fingerprints in this document.
7. A bridge test compiles `and4`, joins every gate-owned primitive back through
   `(gate index, output)` to a source span, joins ports by name, and rejects a
   deliberately mismatched Netlist fingerprint.

Cutover must explicitly review every affected consumer: baked Netlists,
review fingerprints, delay-model reconciliation, strength differential,
polarity tests, fragment-synthesis benchmarks, legacy benchmark evaluation,
CLI tools, and viewer fixtures. Input ordering also needs review because the
old JSON bridge may expose alphabetical order while the new compiler preserves
declaration order.

Exit: ordinary builds and the viewer no longer require Yosys.

### Milestone 6: Online editor, hot reload, and source-to-cell debugging

Write a separate implementation plan after Milestone 5, then add the source
editor, worker/watcher host integration, atomic session replacement, diagnostic
display, and view/camera preservation. Serialize the generator's existing
gate-to-primitive, placement, route, observation, and block identity maps as
`PhysicalDebugArtifact`, then join it to `DebugDatabase` by Netlist fingerprint
and `GateRef`.

Exit: editing `and4.sv` updates the running circuit without page reload; syntax
errors leave the previous circuit usable; selecting source, signal, gate,
primitive/cell, route, or block highlights every connected debug entity in
both directions.

### Milestone 7: Language growth

Add only features demanded by real examples, in this order:

1. unary reductions and comparisons;
2. addition/subtraction and shifts;
3. module instances;
4. constant parameters and generate loops;
5. resettable and negative-edge state, only after matching physical cells
   exist.

Each feature needs one syntax test, one diagnostic test, and one simulated
behaviour test. Do not promise complete IEEE 1800-2023 compatibility.

## Main risks

- **Optimisation gap:** Yosys/ABC reduces the decoder substantially. The first
  REDA compiler may produce a larger netlist. Correctness and portability come
  first; use measured physical cost to select later rewrites.
- **Language ambiguity:** SystemVerilog sizing and signedness rules are subtle.
  Version 1 rejects unclear or unsupported cases instead of guessing.
- **Long physical compile:** hot reload can finish frontend work quickly but
  still wait on placement/routing. Keep it off the main thread and expose the
  active stage.
- **Sequential mismatch:** SystemVerilog scheduling rules are larger than the
  supported physical state model. Accept only the documented `always_ff`
  subset and reject the rest at elaboration with source spans.
- **Cutover fingerprint churn:** even equivalent logic can change placement
  fingerprints because the current JSON bridge and the new compiler may order
  ports differently. Treat every regenerated baseline as a reviewed cutover
  artifact, never incidental compiler work.
- **Stale results:** asynchronous builds can finish out of order. Revision
  checks and atomic session replacement are mandatory.

## First implementation slice

Start with Milestone 0 and the smallest vertical slice of Milestone 1:

```text
module top(input logic a, input logic b, output logic y);
  assign y = a & b;
endmodule
```

Compile it to one `GateKind::And`, evaluate all four input rows with the logical
machine, compare them with the independent predicate and Yosys Netlist, and
confirm the compiler builds for WASM. Query the artifact in both directions:
the assignment expression must find the AND gate, and that gate must return the
assignment's source origin. This proves compilation and debug lineage before
adding vectors, procedural blocks, hierarchy, or incremental compilation.

## Work packets while the generator finishes

Use small vertical changes. Each packet must leave the old Yosys path working.

1. **Contract:** add file-aware spans, typed compiler IDs, diagnostics,
   `CompileArtifact`, compact `DebugDatabase`, a stub pure-Rust entry point,
   Netlist fingerprinting, and the logical evaluator; verify native and WASM
   compilation.
2. **AND slice:** implement only enough lexer, parser, elaboration, graph, and
   emitter code for the two-input example; prove its four rows and bidirectional
   source-to-gate mapping.
3. **Combinational fixture:** extend only as required by `and4.sv`, then
   `seven_segment.sv`; add features in response to a failing fixture.
4. **DFF slice:** add the narrow `always_ff` form and reuse the completed
   `DffPosedge` downstream contract.
5. **Shadow run:** measure parity while production still uses Yosys.

The new `.sv` fixtures are additive during these packets. Existing `.v`
fixtures, catalog entries, baked artifacts, and current production callers do
not move until Milestone 5.

Packets 1-4 may proceed before generator completion because they stop at the
existing boundary. Packets that switch callers, regenerate artifacts, compare
physical fingerprints, or change viewer sessions remain blocked until the
generator is stable.

Do not add parser generators, a second IR crate, an incremental compiler,
filesystem abstraction, or cancellation framework during these packets. Add
one only when a measured failure shows the simpler design cannot meet its
contract.

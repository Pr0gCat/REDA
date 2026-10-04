# REDA inout support audit

Read-only investigation of the shared source on 2026-10-04. Existing internal alias evidence is preserved; no production or test source was changed.

## Supported today

Parser recognizes ANSI `inout` and inherited packed ranges/signs (`src/frontend/parser.rs:235`). Hierarchy elaboration aliases each inout bit to a parent net bit, preserving concat/slice order (`src/frontend/elaborate.rs`, instance Inout branch). `AliasClasses` unions all hierarchy aliases (`src/frontend/logic.rs:587`); graph construction requires exactly one primary-input or assignment driver per class. This is genuine static single-driver connectivity, not dynamic tri-state bus resolution. Existing `tests/systemverilog_internal_inout.rs` covers multiple reader scopes, reordered concatenations, one driver, deterministic netlist/debug, physical switching and native/auto CLI. Its 512-phase physical evidence must remain.

## Current unsupported semantic path

Top-level inout is rejected before elaboration (`src/frontend/elaborate.rs:212`). Four-state Z literals are rejected in the parser (`src/frontend/parser.rs:1083,1124`); `ExprKind`/RTL/Logic represent Boolean values, without drive presence. Multiple assignments to an aliased class are rejected in `src/frontend/logic.rs:524,533`; no resolver exists. `CompileOptions` contains only top (`src/frontend/mod.rs:211`); `CompileArtifact` has ordinary bindings and source-ordered outputs, without inout driver/presence/readback roles. `Netlist` has inputs/outputs/gates (`src/compile/mod.rs:206`). `SelectedFrontend` transports outputs but not complete source port identity (`src/frontend/selection.rs:42`).

The physical contract is explicitly single-direction: `PortRole` only Input/Output, one handover neighbor per `PortPin` (`src/compile/planner.rs:3034`). Input rear-reading and output delivery repeaters are distinguished by facing (`src/compile/mod.rs:7288,7309`). One shared caller cell currently cannot satisfy both existing exclusive-neighbor contracts. `build_circuit` writes two maps and output labels (`src/bin/build_circuit.rs:938`); viewer `BakedPinout` has two maps (`viewer/src/lib.rs:161`) and physical port-flow is one direction (`viewer/src/lib.rs:598`).

Additional fallback hazard: Yosys JSON transport traverses only direction=input and direction=output (`src/frontend/yosys_json.rs:451,550`); inout is skipped by both loops. Sources with internally driven inout and ordinary outputs may therefore lose a public boundary without a diagnostic. This observation is from current code, not a newly executed end-to-end fixture. Preventing omission alone does not count as support.

## Binary restriction that preserves actual HDL semantics

Preserve each driver as `(value,present)`, where Z means absent. For resolved bits, let `present_any = OR(present_i)` and `conflict = OR(present_i & present_j & (value_i XOR value_j))`. Under `present_any && !conflict`, resolved Boolean value is `OR(present_i & value_i)` and every source readback uses this resolved value, including a writer reading its own bus. Multiple active equal values are valid; different active values produce HDL X. No active driver produces HDL Z. Both invalid cases are outside a two-value observation domain, rather than silently becoming zero. Pull-up/pull-down nets can supply an explicit always-present driver only where source/environment really declares that behavior.

A tri-state assignment such as `assign bus = enable ? data : 1'bz` is lowered to drive value/data and drive presence/enable. Z recognition must be contextual: it denotes absent drive at a resolved net boundary, not a general Boolean constant in arithmetic. Hierarchical inout connections join both driver sets and readback identities. External driver presence is necessary: a low redstone signal cannot distinguish absent external drive from an actively driven HDL zero.

Split read/drive/enable pins can transport this internal representation, but they are only an adapter interface. They do not establish the requested single physical bidirectional bus. To claim genuine physical support, define and build a shared bus boundary with explicit ownership/presence sideband and prove readback, outward drive and absence isolation in the actual game. Within the valid binary domain, enabled-value contributions and a redstone shared channel can implement the resolved Boolean function; the physical ownership/presence convention still needs an explicit public contract. Unrestricted four-state Verilog and analog contention are not achieved by a wired-OR channel.

## Contract choices still required

1. Is external drive presence supplied explicitly per bit, shared per vector, or guaranteed by an external ownership protocol? A read-value input alone is insufficient.
2. Is no-driver behavior observed as Z through a richer API, excluded by a valid-domain invariant, or resolved by an explicitly specified pull? Defaulting to zero is not source-equivalent.
3. Is active-value disagreement observable X through a richer API, excluded with a checked validity invariant, or prohibited by proven arbitration? Source drivers may overlap with equal values legally.
4. Does the public physical boundary require a single shared redstone channel plus presence/ownership sideband, or deliberately expose split interface adapters? The latter must not be reported as completing the former.

## Genuine support acceptance

Freeze source and contract before tests. Execute standard-HDL independent event-simulation oracle cases over all permitted external/internal value/presence transitions. Check writer readback, enabled/disabled drive, external drive changes, equal overlapping values, explicit pull behavior, bit order, nested hierarchy, read-dependent drive feedback and legal clocked direction changes. Preserve invalid Z/X cases as such in the oracle; demonstrate domain validation or richer observation, rather than assigning arbitrary zero. Include a positive original top-level inout source through native/auto CLI without Yosys, with complete original port identity in artifact, generated pinout and viewer. Verify source/logic/public-output provenance and deterministic generation. Build a real shared physical boundary, check isolation and drive/readback directions through all valid transitions in simulator and Minecraft, and restore caller cells/world fixtures afterward. Existing internal-alias regression and physical512-phase evidence remain controls. Diagnostic tests are negative controls, never completion evidence.

Recommended order: agree the binary-domain physical ownership contract, implement value/presence resolution and source boundary metadata in isolation, qualify against standard HDL, then implement the matching shared physical port and carry metadata through CLI/viewer. Static read-only or always-driven top inout can be useful positive subcases, but cannot close dynamic top-level inout by themselves.

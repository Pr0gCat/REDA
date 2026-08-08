//! Prices what each Yosys cell type actually costs in redstone, by measuring
//! rather than estimating.
//!
//! See `docs/superpowers/specs/2026-08-08-cell-type-costs.md` for the report
//! this harness produces the numbers for, and
//! `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md` for why the
//! question matters: `redstone_nor.genlib` only prices NOR/BUF, so ABC's
//! technology mapper has no honest number for any other gate shape and pays
//! two gates for every OR.
//!
//! Two questions per cell type:
//!
//! 1. What does it cost realised the way this compiler can build it today --
//!    a NOR network, run through the real placer/router (`compile`) and the
//!    real simulator? [`measure_nor_network`] answers this for every type by
//!    building the same kind of NOR tree `NetlistBuilder` would (De Morgan
//!    expansions with fan-in <= 3), compiling it, and reading gate count,
//!    non-air block count, and worst-case settle ticks off the result --
//!    exactly the method `tests/reference_circuits.rs` already uses for the
//!    project's reference circuits.
//! 2. Does a cheaper, non-NOR realisation exist? Two of them are probed by
//!    hand-building a `World` directly (bypassing `compile` and its NOR-only
//!    `Gate` type entirely) and running it through the real `Simulator`:
//!    `or_is_a_free_wire_merge_when_nothing_else_shares_the_branch` and its
//!    sibling below for the backflow case, and
//!    `andnot_is_one_comparator_in_subtract_mode`. Both are genuine claims
//!    about redstone behaviour, so both are checked against the simulator
//!    rather than taken on faith -- this project's record with unverified
//!    redstone claims is the reason that rule exists at all.
//!
//! This file only measures. It does not add a gate kind, a library entry, or
//! a genlib line -- see the report doc for why not yet.

use reda::compile::{compile, Gate, Netlist};
use reda::redstone::simulator::position::Position;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use reda::redstone::world::storage::World;

const MAX_TICKS: u64 = 2000;

// ---------------------------------------------------------------------
// A minimal NOR-tree builder, mirroring `circuits::netlist_builder::
// NetlistBuilder`'s two primitives (`nor`, `not`) closely enough to build
// the same *kind* of network it would -- but written locally because
// `NetlistBuilder` itself is `pub(crate)` to the `reda` crate and this file
// is a separate crate that can only see `reda`'s public API (`Gate` and
// `Netlist` are both plain public structs, so building a `Netlist` by hand
// needs nothing else).
// ---------------------------------------------------------------------

struct GateNet {
    gates: Vec<Gate>,
    counter: usize,
}

impl GateNet {
    fn new() -> Self {
        GateNet { gates: Vec::new(), counter: 0 }
    }

    fn fresh(&mut self) -> String {
        let name = format!("g{}", self.counter);
        self.counter += 1;
        name
    }

    /// A NOR gate with 1..=3 inputs -- `place_nor_gate`'s own hardware
    /// ceiling, the same one `NetlistBuilder::nor` enforces.
    fn nor(&mut self, inputs: &[&str]) -> String {
        assert!(!inputs.is_empty() && inputs.len() <= 3, "NOR fan-in must be 1..=3, got {}", inputs.len());
        let output = self.fresh();
        self.gates.push(Gate {
            name: output.clone(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            output: output.clone(),
            is_merge: false,
        });
        output
    }

    fn not(&mut self, x: &str) -> String {
        self.nor(&[x])
    }
}

// ---------------------------------------------------------------------
// One netlist per Yosys cell type this task was asked to price -- the seven
// `synth` (no `abc`) actually emitted (`$_OR_`, `$_NAND_`, `$_ANDNOT_`,
// `$_NOR_`, `$_AND_`, `$_ORNOT_`, `$_MUX_`), plus `$_NOT_`/BUF (already
// priced in `redstone_nor.genlib`, included here for a complete table) and
// `$_XOR_`/`$_XNOR_` (not seen on this project's own circuits, but standard
// Yosys output on other Verilog, so worth having a number for). Every
// construction below is the smallest NOR tree this task's author could find
// by hand (see the report doc for the derivation of each), and every one is
// checked against its truth table via the real compiled-and-simulated
// circuit below, not against the boolean algebra alone.
//
// Every builder returns `(Netlist, output_signal, input_names)`.
// ---------------------------------------------------------------------

fn build_not() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let y = net.not("a");
    (Netlist { inputs: vec!["a".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a"])
}

/// `BUF`: two chained NOR1 torches, `NOT(NOT(x)) == x` -- the same
/// realisation `topology::buf_entry` already documents and
/// `redstone_nor.genlib` already prices.
fn build_buf() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let n1 = net.not("a");
    let y = net.not(&n1);
    (Netlist { inputs: vec!["a".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a"])
}

fn build_nor2() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let y = net.nor(&["a", "b"]);
    (Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a", "b"])
}

/// **Does not compile** -- see `bare_nor3_from_three_raw_primary_inputs_hits_a_router_edge_case`
/// below. Kept as its own builder (rather than folded into that test) so the
/// discovered shape is named and reusable, the same way every other builder
/// in this file is.
fn build_nor3() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let y = net.nor(&["a", "b", "c"]);
    (
        Netlist { inputs: vec!["a".to_string(), "b".to_string(), "c".to_string()], outputs: vec![y.clone()], gates: net.gates },
        y,
        vec!["a", "b", "c"],
    )
}

/// The same function, `NOR3(a,b,c)`, built so it actually compiles: `c`'s
/// socket is fed by a `BUF(c)` (two chained NOR1 torches, `NOT(NOT(c)) ==
/// c`) instead of by `c`'s own lever directly. See
/// `bare_nor3_from_three_raw_primary_inputs_hits_a_router_edge_case` for what
/// this works around and how that was isolated to "all three of a gate's
/// inputs are primary-input levers", not to arity 3 or to the South socket
/// on their own. This is what `cost_of_nor3` actually measures -- 3 gates,
/// not 1, so its own numbers are NOR3's cost plus one BUF's, not NOR3's
/// alone (see that test's own comment for the practical reading).
fn build_nor3_measurable() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let c1 = net.not("c");
    let c2 = net.not(&c1);
    let y = net.nor(&["a", "b", &c2]);
    (
        Netlist { inputs: vec!["a".to_string(), "b".to_string(), "c".to_string()], outputs: vec![y.clone()], gates: net.gates },
        y,
        vec!["a", "b", "c"],
    )
}

/// `AND(a,b) = NOR(NOT a, NOT b)` -- De Morgan, 3 gates: both inputs need
/// negating before the final NOR, since neither enters the AND already
/// complemented.
fn build_and2() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let na = net.not("a");
    let nb = net.not("b");
    let y = net.nor(&[&na, &nb]);
    (Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a", "b"])
}

/// `OR(a,b) = NOT(NOR(a,b))` -- 2 gates: no input needs negating (both enter
/// the inner NOR raw), only the final inversion.
fn build_or2() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let n = net.nor(&["a", "b"]);
    let y = net.not(&n);
    (Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a", "b"])
}

/// `NAND(a,b) = NOT(AND(a,b))` -- `build_and2`'s 3 gates plus one more
/// inversion, 4 total. This is the general OR-shape penalty: an OR-shape
/// output out of NOR always costs one negation per input that enters
/// complemented, plus one for the final inversion, and NAND has no
/// complemented inputs to cancel that final inversion against (unlike
/// ANDNOT below).
fn build_nand2() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let na = net.not("a");
    let nb = net.not("b");
    let and = net.nor(&[&na, &nb]);
    let y = net.not(&and);
    (Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a", "b"])
}

/// `ANDNOT(a,b) = a & !b = NOR(NOT a, b)`.
///
/// This is the cheap case the spec flagged: `NOR(x,y) = !x & !y`, so setting
/// `x = NOT a` (making `!x = a`) and `y = b` (making `!y = NOT b`) gets
/// `a & !b` out of a single NOR gate plus a single negation -- 2 gates, one
/// less than plain AND, because the input that is supposed to enter negated
/// (`b`) already does, for free, as one of NOR's own two implicit negations.
fn build_andnot2() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let na = net.not("a");
    let y = net.nor(&[&na, "b"]);
    (Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a", "b"])
}

/// `ORNOT(a,b) = a | !b = NOT(NOR(a, NOT b))`.
///
/// Unlike ANDNOT, this is an OR-shape output, so it pays the input negation
/// (`NOT b`, since `b` enters the OR complemented) *and* the final
/// inversion the OR shape always costs -- 3 gates. Also verified below not
/// to be reducible to 2 by trying the AND-shape derivation
/// (`!(!a & b)` needs the same 3, just distributed differently) -- 3 seems
/// to be the honest floor for this shape from 2-input NOR alone.
fn build_ornot2() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let nb = net.not("b");
    let n = net.nor(&["a", &nb]);
    let y = net.not(&n);
    (Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a", "b"])
}

/// `XNOR(a,b)`, the classic 4-NOR-gate construction (verified by truth table
/// both algebraically in this file's own doc comment derivation and against
/// the real compiled circuit below): `n1 = NOR(a,b)`, then
/// `NOR(NOR(a,n1), NOR(b,n1))`.
fn build_xnor2() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let n1 = net.nor(&["a", "b"]);
    let n2 = net.nor(&["a", &n1]);
    let n3 = net.nor(&["b", &n1]);
    let y = net.nor(&[&n2, &n3]);
    (Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a", "b"])
}

/// `XOR(a,b) = NOT(XNOR(a,b))` -- one more gate than XNOR, 5 total. NOR
/// pairs naturally with XNOR (both are "agreement" shapes), the same way it
/// pairs with AND/ANDNOT rather than OR/ORNOT -- XOR pays an extra
/// inversion for the same reason NAND and ORNOT do.
fn build_xor2() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let n1 = net.nor(&["a", "b"]);
    let n2 = net.nor(&["a", &n1]);
    let n3 = net.nor(&["b", &n1]);
    let xnor = net.nor(&[&n2, &n3]);
    let y = net.not(&xnor);
    (Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y.clone()], gates: net.gates }, y, vec!["a", "b"])
}

/// `MUX(s,a,b) = s ? b : a = OR(AND(!s,a), AND(s,b))`, sum-of-products the
/// same way `NetlistBuilder::or_reduce`/`and_reduce` would, but hand-picked
/// to reuse the ANDNOT trick where it applies:
///
/// - `term1 = !s & a = NOR(s, NOT a)` -- 2 gates (ANDNOT shape: `s` enters
///   raw, `a` enters complemented via one NOT).
/// - `term2 = s & b = NOR(NOT s, NOT b)` -- 3 gates (plain AND shape:
///   neither input enters complemented, so both need their own NOT).
/// - `y = OR(term1, term2) = NOT(NOR(term1, term2))` -- 2 gates.
///
/// 7 gates total. Yosys's Verilog `? :` convention (`S` selects `B`) is
/// followed here, matching `$_MUX_`'s own `A`/`B`/`S`/`Y` port meaning.
fn build_mux() -> (Netlist, String, Vec<&'static str>) {
    let mut net = GateNet::new();
    let na = net.not("a");
    let term1 = net.nor(&["s", &na]);
    let ns = net.not("s");
    let nb = net.not("b");
    let term2 = net.nor(&[&ns, &nb]);
    let n = net.nor(&[&term1, &term2]);
    let y = net.not(&n);
    (
        Netlist { inputs: vec!["s".to_string(), "a".to_string(), "b".to_string()], outputs: vec![y.clone()], gates: net.gates },
        y,
        vec!["s", "a", "b"],
    )
}

// ---------------------------------------------------------------------
// Measurement: compile, verify the truth table against the real simulator,
// and read back gate count / non-air blocks / worst-case settle ticks.
// ---------------------------------------------------------------------

fn count_non_air(world: &World) -> usize {
    let (size_x, size_y, size_z) = world.size();
    let mut count = 0usize;
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if world.get(x, y, z).kind != BlockKind::Air {
                    count += 1;
                }
            }
        }
    }
    count
}

fn set_lever(simulator: &mut Simulator, position: (i32, i32, i32), on: bool) -> u64 {
    let start = simulator.current_tick();
    let mut state = simulator.world().get(position.0, position.1, position.2).clone();
    state.lit = on;
    simulator.world_mut().set(position.0, position.1, position.2, state);
    simulator.run_until_stable(MAX_TICKS).expect("a NOR network must settle after any single input change");
    simulator.current_tick() - start
}

#[derive(Debug, Clone, Copy)]
struct CellCost {
    gates: usize,
    blocks: usize,
    settle_game_ticks: u64,
}

/// Compiles `netlist`, checks its output against `truth` for every input
/// combination via the real simulator (not the boolean algebra this file's
/// comments derive it from), and returns the measured cost. Every input
/// combination is swept from the previous one input-at-a-time, exactly as
/// `tests/reference_circuits.rs`'s `the_compiled_and4_matches_its_truth_table`
/// does, so the settle figure is a genuine worst-case single-input-change
/// settle time, not a from-reset one.
fn measure_nor_network(
    label: &str,
    netlist: Netlist,
    output: &str,
    inputs: &[&str],
    truth: impl Fn(&[bool]) -> bool,
) -> CellCost {
    let gate_count = netlist.gates.len();
    let compiled = compile(&netlist).unwrap_or_else(|err| panic!("{label} failed to compile: {err}"));
    let blocks = count_non_air(&compiled.world);

    let lever_positions: Vec<(i32, i32, i32)> =
        inputs.iter().map(|name| compiled.input_positions[*name]).collect();
    let output_position = compiled.output_positions[output];

    let mut simulator = Simulator::new(compiled.world.clone());
    simulator
        .run_until_stable(MAX_TICKS)
        .unwrap_or_else(|err| panic!("{label} must settle before the first reading: {err:?}"));

    let mut worst_settle = 0u64;
    let n = inputs.len();
    for combo in 0u32..(1 << n) {
        let bits: Vec<bool> = (0..n).map(|i| (combo >> i) & 1 == 1).collect();
        for (position, &bit) in lever_positions.iter().zip(bits.iter()) {
            let ticks = set_lever(&mut simulator, *position, bit);
            worst_settle = worst_settle.max(ticks);
        }
        let actual = simulator.world().get(output_position.0, output_position.1, output_position.2).lit;
        let expected = truth(&bits);
        assert_eq!(
            actual, expected,
            "{label}: inputs {inputs:?}={bits:?} -> expected {expected}, compiled circuit gave {actual}"
        );
    }

    CellCost { gates: gate_count, blocks, settle_game_ticks: worst_settle }
}

// ---------------------------------------------------------------------
// One test per cell type: builds it, verifies its truth table against the
// real compiled-and-simulated circuit, and prints its measured cost. Run
// with `cargo test --test cell_type_costs -- --nocapture` to see every
// number; `cell_type_cost_table` below reruns all of them together for a
// single combined table.
// ---------------------------------------------------------------------

#[test]
fn cost_of_not() {
    let (n, y, i) = build_not();
    let cost = measure_nor_network("NOT", n, &y, &i, |b| !b[0]);
    eprintln!("NOT   : {cost:?}");
}

#[test]
fn cost_of_buf() {
    let (n, y, i) = build_buf();
    let cost = measure_nor_network("BUF", n, &y, &i, |b| b[0]);
    eprintln!("BUF   : {cost:?}");
}

#[test]
fn cost_of_nor2() {
    let (n, y, i) = build_nor2();
    let cost = measure_nor_network("NOR2", n, &y, &i, |b| !(b[0] || b[1]));
    eprintln!("NOR2  : {cost:?}");
}

/// **A discovered router edge case, not this task's to fix.** The obvious
/// way to price a bare `NOR3` -- one gate, three primary-input levers wired
/// straight to it, nothing else in the netlist -- does not compile at all:
/// `compile` returns `ConnectivityViolation` (two of the three input nets'
/// routed dust ends up electrically joined). This is not a NOR3-arity
/// problem in general: every reference circuit (`and4`, `full_adder`,
/// `segment_a`, `seven_segment`, all 273 passing tests) is full of NOR3
/// gates and none of them trips this, because `NetlistBuilder::and_reduce`
/// always interposes a `NOT` gate before feeding any multi-input NOR, and
/// `or_reduce` in practice is only ever called on already-derived signals,
/// never on three raw top-level inputs at once. Isolated by construction
/// (see the two probes that used to live here, folded into this comment
/// once the isolation was confirmed): a 3-input NOR with any *one* of its
/// three sockets fed by a gate output instead of a lever compiles and
/// simulates correctly regardless of which socket that is (West, East, or
/// the South one an arity-<=2 gate never even uses) -- only "all three
/// inputs are primary-input levers" reproduces the failure. That points at
/// the primary-input row / bypass-routing interaction specifically, not at
/// `place_nor_gate`'s own geometry.
///
/// Flagged as a follow-up (see this task's own report), not fixed here --
/// this task measures, it does not change the compiler.
#[test]
fn bare_nor3_from_three_raw_primary_inputs_hits_a_router_edge_case() {
    let (netlist, _y, _inputs) = build_nor3();
    match compile(&netlist) {
        Ok(_) => panic!(
            "if this starts compiling, the router edge case this test documents has been fixed (or moved) -- \
             update `docs/superpowers/specs/2026-08-08-cell-type-costs.md` and this comment to match"
        ),
        Err(err) => assert!(
            matches!(err, reda::compile::CompileError::ConnectivityViolation { .. }),
            "expected a ConnectivityViolation, got {err:?}"
        ),
    }
}

/// NOR3's practical cost, measured via `build_nor3_measurable` -- the same
/// function, `NOR3(a,b,c)`, worked around the edge case above by feeding
/// `c` through a `BUF` instead of straight off its own lever. This measures
/// **NOR3 plus one BUF** (4 gates would be wrong -- `build_nor3_measurable`
/// is 3: the 2-gate BUF plus the real NOR3), not NOR3 alone: subtract BUF's
/// own already-measured cost (`cost_of_buf`: 2 gates, 78 blocks, 11 ticks)
/// for a rough (not exact, routing interacts) estimate of the bare gate.
#[test]
fn cost_of_nor3() {
    let (n, y, i) = build_nor3_measurable();
    let cost = measure_nor_network("NOR3", n, &y, &i, |b| !(b[0] || b[1] || b[2]));
    eprintln!("NOR3 (measured as NOR3 + one BUF on the c input, see comment): {cost:?}");
}

#[test]
fn cost_of_and2() {
    let (n, y, i) = build_and2();
    let cost = measure_nor_network("AND", n, &y, &i, |b| b[0] && b[1]);
    eprintln!("AND   : {cost:?}");
}

#[test]
fn cost_of_or2() {
    let (n, y, i) = build_or2();
    let cost = measure_nor_network("OR", n, &y, &i, |b| b[0] || b[1]);
    eprintln!("OR    : {cost:?}");
}

#[test]
fn cost_of_nand2() {
    let (n, y, i) = build_nand2();
    let cost = measure_nor_network("NAND", n, &y, &i, |b| !(b[0] && b[1]));
    eprintln!("NAND  : {cost:?}");
}

#[test]
fn cost_of_andnot2() {
    let (n, y, i) = build_andnot2();
    let cost = measure_nor_network("ANDNOT", n, &y, &i, |b| b[0] && !b[1]);
    eprintln!("ANDNOT: {cost:?}");
}

#[test]
fn cost_of_ornot2() {
    let (n, y, i) = build_ornot2();
    let cost = measure_nor_network("ORNOT", n, &y, &i, |b| b[0] || !b[1]);
    eprintln!("ORNOT : {cost:?}");
}

#[test]
fn cost_of_xor2() {
    let (n, y, i) = build_xor2();
    let cost = measure_nor_network("XOR", n, &y, &i, |b| b[0] != b[1]);
    eprintln!("XOR   : {cost:?}");
}

#[test]
fn cost_of_xnor2() {
    let (n, y, i) = build_xnor2();
    let cost = measure_nor_network("XNOR", n, &y, &i, |b| b[0] == b[1]);
    eprintln!("XNOR  : {cost:?}");
}

#[test]
fn cost_of_mux() {
    let (n, y, i) = build_mux();
    // Yosys `$_MUX_` convention: Y = S ? B : A.
    let cost = measure_nor_network("MUX", n, &y, &i, |b| if b[0] { b[2] } else { b[1] });
    eprintln!("MUX   : {cost:?}");
}

#[test]
fn cell_type_cost_table() {
    let rows: Vec<(&str, CellCost)> = vec![
        {
            let (n, y, i) = build_not();
            ("NOT", measure_nor_network("NOT", n, &y, &i, |b| !b[0]))
        },
        {
            let (n, y, i) = build_buf();
            ("BUF", measure_nor_network("BUF", n, &y, &i, |b| b[0]))
        },
        {
            let (n, y, i) = build_nor2();
            ("NOR2", measure_nor_network("NOR2", n, &y, &i, |b| !(b[0] || b[1])))
        },
        {
            // See `cost_of_nor3`: measured as NOR3 + one BUF, not NOR3 alone
            // (the bare form does not compile -- see
            // `bare_nor3_from_three_raw_primary_inputs_hits_a_router_edge_case`).
            let (n, y, i) = build_nor3_measurable();
            ("NOR3*", measure_nor_network("NOR3", n, &y, &i, |b| !(b[0] || b[1] || b[2])))
        },
        {
            let (n, y, i) = build_and2();
            ("AND", measure_nor_network("AND", n, &y, &i, |b| b[0] && b[1]))
        },
        {
            let (n, y, i) = build_or2();
            ("OR", measure_nor_network("OR", n, &y, &i, |b| b[0] || b[1]))
        },
        {
            let (n, y, i) = build_nand2();
            ("NAND", measure_nor_network("NAND", n, &y, &i, |b| !(b[0] && b[1])))
        },
        {
            let (n, y, i) = build_andnot2();
            ("ANDNOT", measure_nor_network("ANDNOT", n, &y, &i, |b| b[0] && !b[1]))
        },
        {
            let (n, y, i) = build_ornot2();
            ("ORNOT", measure_nor_network("ORNOT", n, &y, &i, |b| b[0] || !b[1]))
        },
        {
            let (n, y, i) = build_xor2();
            ("XOR", measure_nor_network("XOR", n, &y, &i, |b| b[0] != b[1]))
        },
        {
            let (n, y, i) = build_xnor2();
            ("XNOR", measure_nor_network("XNOR", n, &y, &i, |b| b[0] == b[1]))
        },
        {
            let (n, y, i) = build_mux();
            ("MUX", measure_nor_network("MUX", n, &y, &i, |b| if b[0] { b[2] } else { b[1] }))
        },
    ];

    eprintln!();
    eprintln!("{:<8} {:>6} {:>8} {:>14} {:>12}", "cell", "gates", "blocks", "settle(ticks)", "blocks/gate");
    for (name, cost) in &rows {
        eprintln!(
            "{:<8} {:>6} {:>8} {:>14} {:>12.1}",
            name,
            cost.gates,
            cost.blocks,
            cost.settle_game_ticks,
            cost.blocks as f64 / cost.gates as f64
        );
    }
}

// ---------------------------------------------------------------------
// Non-NOR realisations: hand-built `World`s, bypassing `compile` and its
// NOR-only `Gate` type entirely, run through the real `Simulator` to check
// a specific claim about redstone physics before it goes in the report.
// ---------------------------------------------------------------------

fn stone() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Solid;
    state.name = "minecraft:stone".to_string();
    state
}

fn dust() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::RedstoneWire;
    state.name = "minecraft:redstone_wire".to_string();
    state
}

fn raw_lever(on: bool) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Lever;
    state.name = "minecraft:lever".to_string();
    state.lit = on;
    state.face = Some(Face::Floor);
    state.facing = Some(Facing::North);
    state
}

fn repeater(direction: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Repeater;
    state.name = "minecraft:repeater".to_string();
    state.facing = Some(direction.opposite());
    state.delay = 1;
    state.lit = true;
    state
}

fn comparator_subtract(facing: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Comparator;
    state.name = "minecraft:comparator".to_string();
    state.facing = Some(facing);
    state.extra_properties.insert("mode".to_string(), "subtract".to_string());
    state
}

fn set_raw_lever(simulator: &mut Simulator, pos: Position, on: bool) -> u64 {
    let start = simulator.current_tick();
    simulator.world_mut().set(pos.x, pos.y, pos.z, raw_lever(on));
    simulator.run_until_stable(MAX_TICKS).expect("hand-built probe circuit must settle");
    simulator.current_tick() - start
}

/// Every dust cell in these hand-built probes sits on its own floor, so a
/// wire has somewhere to be -- matches `compile::ensure_floor`'s own
/// convention, even though nothing in this simulator's power model actually
/// requires it structurally.
fn floor_under(world: &mut World, pos: Position) {
    world.set(pos.x, pos.y - 1, pos.z, stone());
}

/// **Claim under test**: two redstone dust lines merging into one network is
/// an OR with zero primitives -- the merged wire's strength is the maximum
/// of its sources', which is exactly the OR operation, per
/// `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`.
///
/// A straight run of plain dust, a lever directly against each end, nothing
/// else -- no torch, no repeater. `power_emitted_toward`'s catch-all arm
/// makes a lever isotropic (it strongly powers every neighbour, not just
/// the block it is nominally "attached to" -- see that function's own `_ =>
/// full` arm), so each lever charges the dust cell next to it directly, and
/// `recompute_dust_strengths`'s BFS carries that the rest of the way down
/// the run. If the claim is right, the middle cell reads a nonzero (in fact
/// full, since it is only one hop from either end here) strength whenever
/// *either* lever is on, with no active component anywhere in the circuit.
#[test]
fn or_is_a_free_wire_merge_when_nothing_else_shares_the_branch() {
    let mut world = World::new(8, 3, 3);

    let lever_a = Position::new(0, 1, 1);
    let wire: Vec<Position> = (1..=5).map(|x| Position::new(x, 1, 1)).collect();
    let lever_b = Position::new(6, 1, 1);
    let middle = wire[2]; // three hops from either end -- still well inside MAX_DUST_RUN.

    world.set(lever_a.x, lever_a.y, lever_a.z, raw_lever(false));
    for &cell in &wire {
        floor_under(&mut world, cell);
        world.set(cell.x, cell.y, cell.z, dust());
    }
    world.set(lever_b.x, lever_b.y, lever_b.z, raw_lever(false));

    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    let blocks = count_non_air(&simulator.world().clone());
    let mut worst_ticks = 0u64;

    for (a, b) in [(false, false), (false, true), (true, false), (true, true)] {
        let t1 = set_raw_lever(&mut simulator, lever_a, a);
        let t2 = set_raw_lever(&mut simulator, lever_b, b);
        worst_ticks = worst_ticks.max(t1).max(t2);
        let power = simulator.world().get(middle.x, middle.y, middle.z).power;
        let expected = a || b;
        assert_eq!(
            power > 0,
            expected,
            "merge({a}, {b}): expected power>0 == {expected}, got power={power}"
        );
    }

    eprintln!("OR (wire merge): {blocks} blocks, 0 gates, 0 torches, 0 repeaters, worst settle {worst_ticks} ticks");
}

/// **Claim under test, half 2**: the bare merge above is only correct when
/// the branch it's built from drives nothing else. If a lever's own wire
/// also feeds an unrelated consumer, and that wire is joined into a merge
/// with no isolation, the merge's own backflow corrupts the unrelated
/// consumer -- dust is bidirectional, so a signal arriving at the merge from
/// the *other* branch runs back up the shared wire just as readily as the
/// wire's own source's signal runs forward.
///
/// Layout: lever A drives a fork cell directly. From the fork, one branch
/// goes through a repeater into a support block with a standing torch on it
/// (`consumer_torch`, meant to read `NOT(A)` and nothing else); the other
/// branch is two more plain dust cells running straight into a second merge
/// point that lever B also drives directly, unisolated. Since dust cells are
/// bidirectionally connected, that second branch is electrically the *same
/// network* as the fork cell itself, so B's signal reaches the fork (and
/// therefore the repeater's input) exactly as A's own signal does.
#[test]
fn or_merge_without_isolation_corrupts_a_branch_that_fans_out_elsewhere() {
    let mut world = World::new(10, 4, 10);

    let lever_a = Position::new(1, 1, 5);
    let fork = Position::new(2, 1, 5);
    // Branch 1: fork -> repeater -> consumer support -> standing torch.
    let repeater_pos = Position::new(3, 1, 5);
    let consumer_support = Position::new(4, 1, 5);
    let consumer_torch = Position::new(4, 2, 5);
    // Branch 2 (unisolated): fork -> plain dust -> merge point, driven by B.
    let branch2 = Position::new(2, 1, 6);
    let merge = Position::new(2, 1, 7);
    let lever_b = Position::new(3, 1, 7);

    world.set(lever_a.x, lever_a.y, lever_a.z, raw_lever(false));
    floor_under(&mut world, fork);
    world.set(fork.x, fork.y, fork.z, dust());
    floor_under(&mut world, repeater_pos);
    world.set(repeater_pos.x, repeater_pos.y, repeater_pos.z, repeater(Facing::East));
    world.set(consumer_support.x, consumer_support.y, consumer_support.z, stone());
    world.set(consumer_torch.x, consumer_torch.y, consumer_torch.z, {
        let mut t = BlockState::air();
        t.kind = BlockKind::Torch;
        t.name = "minecraft:redstone_torch".to_string();
        t.lit = true;
        t
    });
    floor_under(&mut world, branch2);
    world.set(branch2.x, branch2.y, branch2.z, dust());
    floor_under(&mut world, merge);
    world.set(merge.x, merge.y, merge.z, dust());
    world.set(lever_b.x, lever_b.y, lever_b.z, raw_lever(false));

    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    // A off, B on: the merge is correctly powered (it is still a correct OR),
    // but the unrelated consumer torch -- which should only ever answer
    // NOT(A) -- goes dark too, because B's signal ran backward through the
    // unisolated shared branch and reached the repeater's input.
    set_raw_lever(&mut simulator, lever_a, false);
    set_raw_lever(&mut simulator, lever_b, true);
    let torch_lit = simulator.world().get(consumer_torch.x, consumer_torch.y, consumer_torch.z).lit;
    assert!(
        !torch_lit,
        "backflow claim not reproduced: with A=0, B=1, the unisolated consumer torch should have been \
         corrupted dark (reading B instead of NOT(A)=1), but it is lit"
    );
}

/// **Claim under test, half 3**: inserting a repeater on the shared branch --
/// exactly at the point the fanout rule says to (`docs/superpowers/specs/
/// 2026-08-08-gate-types-and-wired-or.md`, "Isolate a branch when its source
/// fans out to anything besides this merge") -- fixes the corruption above,
/// because a repeater only ever forwards, never backward. Identical layout
/// to the corruption test except `branch2` is a repeater instead of plain
/// dust.
#[test]
fn or_merge_with_isolation_protects_a_branch_that_fans_out_elsewhere() {
    let mut world = World::new(10, 4, 10);

    let lever_a = Position::new(1, 1, 5);
    let fork = Position::new(2, 1, 5);
    let repeater_pos = Position::new(3, 1, 5);
    let consumer_support = Position::new(4, 1, 5);
    let consumer_torch = Position::new(4, 2, 5);
    // Branch 2 (isolated): fork -> repeater (one-way!) -> merge, driven by B.
    let isolating_repeater = Position::new(2, 1, 6);
    let merge = Position::new(2, 1, 7);
    let lever_b = Position::new(3, 1, 7);

    world.set(lever_a.x, lever_a.y, lever_a.z, raw_lever(false));
    floor_under(&mut world, fork);
    world.set(fork.x, fork.y, fork.z, dust());
    floor_under(&mut world, repeater_pos);
    world.set(repeater_pos.x, repeater_pos.y, repeater_pos.z, repeater(Facing::East));
    world.set(consumer_support.x, consumer_support.y, consumer_support.z, stone());
    world.set(consumer_torch.x, consumer_torch.y, consumer_torch.z, {
        let mut t = BlockState::air();
        t.kind = BlockKind::Torch;
        t.name = "minecraft:redstone_torch".to_string();
        t.lit = true;
        t
    });
    floor_under(&mut world, isolating_repeater);
    world.set(isolating_repeater.x, isolating_repeater.y, isolating_repeater.z, repeater(Facing::South));
    floor_under(&mut world, merge);
    world.set(merge.x, merge.y, merge.z, dust());
    world.set(lever_b.x, lever_b.y, lever_b.z, raw_lever(false));

    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    // A off, B on: the merge is still correctly powered (B's signal still
    // reaches it, forward through the isolating repeater), and this time the
    // unrelated consumer torch correctly reads NOT(A) = lit, unaffected by B.
    set_raw_lever(&mut simulator, lever_a, false);
    set_raw_lever(&mut simulator, lever_b, true);

    let merge_power = simulator.world().get(merge.x, merge.y, merge.z).power;
    assert!(merge_power > 0, "the merge itself must still see B's signal through the isolating repeater");

    let torch_lit = simulator.world().get(consumer_torch.x, consumer_torch.y, consumer_torch.z).lit;
    assert!(torch_lit, "isolation should have protected the consumer torch: NOT(A=0) = 1, but it reads dark");

    // And with A on, the torch correctly goes dark regardless of B.
    set_raw_lever(&mut simulator, lever_a, true);
    set_raw_lever(&mut simulator, lever_b, false);
    let torch_lit = simulator.world().get(consumer_torch.x, consumer_torch.y, consumer_torch.z).lit;
    assert!(!torch_lit, "NOT(A=1) should be 0");
}

/// **Claim under test**: `$_ANDNOT_` (`a & !b`) has a one-primitive redstone
/// realisation: a comparator in subtract mode, rear input `a`, side input
/// `b`. For boolean (0 or 15) inputs, `max(0, a - b)` is exactly `a & !b`.
///
/// Wiring follows `comparator_rear_position`/`comparator_side_positions`'s
/// own documented geometry: with `facing = West`, the rear (main) input is
/// the cell to the comparator's west, and the two side inputs are north and
/// south. The rear input is fed the ordinary way (a lever directly against
/// a dust cell, matching every other test in this file); the side input is
/// fed the way `comparator_side_input`'s own doc comment insists real
/// Minecraft requires and this simulator enforces -- **strong** power on the
/// side block itself, not weak power off a dust line sitting on top of it --
/// so lever B stands directly on top of the side block rather than feeding
/// it through dust.
#[test]
fn andnot_is_one_comparator_in_subtract_mode() {
    let mut world = World::new(10, 4, 10);

    let comparator_pos = Position::new(5, 1, 5);
    let rear_dust = Position::new(4, 1, 5); // west of the comparator: rear/main input
    let lever_a = Position::new(3, 1, 5); // west of rear_dust, charges it to 15
    let side_block = Position::new(5, 1, 6); // south of the comparator: one side input
    let lever_b = Position::new(5, 2, 6); // standing on top of side_block: strong power
    let output_dust = Position::new(6, 1, 5); // east of the comparator: front/output

    world.set(comparator_pos.x, comparator_pos.y, comparator_pos.z, comparator_subtract(Facing::West));
    floor_under(&mut world, comparator_pos);

    floor_under(&mut world, rear_dust);
    world.set(rear_dust.x, rear_dust.y, rear_dust.z, dust());
    world.set(lever_a.x, lever_a.y, lever_a.z, raw_lever(false));

    world.set(side_block.x, side_block.y, side_block.z, stone());
    world.set(lever_b.x, lever_b.y, lever_b.z, raw_lever(false));

    floor_under(&mut world, output_dust);
    world.set(output_dust.x, output_dust.y, output_dust.z, dust());

    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    let blocks = count_non_air(&simulator.world().clone());
    let mut worst_ticks = 0u64;

    for (a, b) in [(false, false), (false, true), (true, false), (true, true)] {
        let t1 = set_raw_lever(&mut simulator, lever_a, a);
        let t2 = set_raw_lever(&mut simulator, lever_b, b);
        worst_ticks = worst_ticks.max(t1).max(t2);
        let output = simulator.world().get(output_dust.x, output_dust.y, output_dust.z).power > 0;
        let expected = a && !b;
        assert_eq!(output, expected, "ANDNOT({a}, {b}): expected {expected}, comparator circuit gave {output}");
    }

    eprintln!(
        "ANDNOT (comparator, subtract mode): {blocks} blocks, 1 primitive (comparator), worst settle {worst_ticks} ticks"
    );
}

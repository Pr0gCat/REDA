//! Netlist construction, in the two things redstone actually builds: NOR
//! gates (fan-in 1..=3) and wire merges (fan-in 2..=3).
//!
//! Shared by every reference circuit under `circuits/` -- the seven-segment
//! decoder, and the smaller circuits alongside it -- so the NOR-tree
//! expansion logic (`not`, `and_reduce`, `or_reduce`) lives in exactly one
//! place instead of being copied per circuit. `compile::lowering` uses the
//! same builder for the same reason: expanding a gate-level `$_XOR_` into
//! torches and merges is the same job as expanding a hand-written
//! `or_reduce`, and doing it through a second, parallel gate constructor is
//! exactly the drift this type exists to prevent.

use std::collections::HashMap;

use crate::compile::topology::GateKind;
use crate::compile::Gate;

/// Builds a netlist one gate at a time.
///
/// - `not`: repeated calls for the same signal return the same shared
///   inverter (remembered in `not_cache`). This is the mechanism behind
///   "build the inverter of each of the four inputs once, then let every
///   minterm share it" -- and, in `compile::lowering`, behind one `!a`
///   serving every gate-level cell that wants one.
/// - `nor`: the primitive operation -- one new NOR gate, at most 3 inputs,
///   matching `place_nor_gate`'s hardware limit.
/// - `merge`: a free wire-merge OR (`GateKind::Or`), 2 or 3 inputs.
/// - `and_reduce` / `or_reduce`: fold a signal list of any length into a
///   tree of fan-in <= 3 gates computing their AND / OR.
///
/// The four hand-written reference circuits deliberately use `or_reduce`
/// rather than `merge`, as the control group the synthesised circuits are
/// measured against -- see `merge`'s own doc comment.
pub(crate) struct NetlistBuilder {
    gates: Vec<Gate>,
    not_cache: HashMap<String, String>,
    prefix: String,
    counter: usize,
}

impl NetlistBuilder {
    pub(crate) fn new() -> Self {
        NetlistBuilder::with_prefix("g".to_string())
    }

    /// A builder whose generated gate names start with `prefix` instead of
    /// `"g"`. `compile::lowering` uses this to guarantee that the names it
    /// invents cannot collide with the names already in the netlist it is
    /// lowering (see `lowering::fresh_prefix`).
    pub(crate) fn with_prefix(prefix: String) -> Self {
        NetlistBuilder { gates: Vec::new(), not_cache: HashMap::new(), prefix, counter: 0 }
    }

    /// Every gate built so far, in construction order.
    pub(crate) fn into_gates(self) -> Vec<Gate> {
        self.gates
    }

    /// How many gates have been built so far. `compile::lowering` uses this
    /// to record which source gate each new gate came from, without this
    /// type having to know what provenance is.
    pub(crate) fn len(&self) -> usize {
        self.gates.len()
    }

    fn fresh_name(&mut self) -> String {
        let name = format!("{}{}", self.prefix, self.counter);
        self.counter += 1;
        name
    }

    /// Take an already-built gate over verbatim -- name, output and kind
    /// untouched. `compile::lowering` uses this for a gate that is already
    /// realisable (a NOR or a merge), which is what makes lowering the
    /// identity on the hand-written circuits.
    ///
    /// A 1-input NOR adopted this way also seeds `not_cache`, so a `$_NOT_`
    /// the netlist already contained is reused as *the* inverter of its
    /// input rather than being duplicated beside a freshly built one.
    pub(crate) fn adopt(&mut self, gate: Gate) {
        if gate.kind == GateKind::Nor(1) {
            self.not_cache.entry(gate.inputs[0].clone()).or_insert_with(|| gate.output.clone());
        }
        self.gates.push(gate);
    }

    /// A new gate of any [`GateKind`], with a generated name -- what the
    /// Verilog frontend builds a gate-level cell with. `inputs` must be in
    /// the kind's own pin order, and as many as the kind declares.
    ///
    /// Use [`NetlistBuilder::nor`] / [`NetlistBuilder::merge`] for the two
    /// realisable kinds instead: their arity comes from the input list
    /// rather than from the caller, which is what constant folding needs.
    pub(crate) fn cell(&mut self, kind: GateKind, inputs: &[String]) -> String {
        assert!(!kind.is_realisable(), "use nor()/merge() for a realisable kind, not cell()");
        assert_eq!(inputs.len(), kind.arity(), "{kind:?} takes {} input(s)", kind.arity());
        let output = self.fresh_name();
        self.gates.push(Gate { name: output.clone(), inputs: inputs.to_vec(), output: output.clone(), kind });
        output
    }

    /// A new NOR gate; `inputs.len()` must be in 1..=3.
    pub(crate) fn nor(&mut self, inputs: &[String]) -> String {
        let output = self.fresh_name();
        self.nor_named(&output.clone(), &output, inputs)
    }

    /// A new NOR gate carrying a caller-chosen `name` and `output` instead
    /// of a generated one -- what `compile::lowering` uses for the last step
    /// of an expansion, so the gate's own declared output name survives.
    pub(crate) fn nor_named(&mut self, name: &str, output: &str, inputs: &[String]) -> String {
        assert!(
            !inputs.is_empty() && inputs.len() <= 3,
            "place_nor_gate takes at most 3 inputs, got {}",
            inputs.len()
        );
        self.gates.push(Gate {
            name: name.to_string(),
            inputs: inputs.to_vec(),
            output: output.to_string(),
            kind: GateKind::Nor(inputs.len()),
        });
        output.to_string()
    }

    /// Build a **declared wire merge** -- the free OR realisation from
    /// `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md`: no
    /// torch, no support, just the point where `inputs`' own routes are
    /// allowed to touch (`compile::place_merge_gate`). `inputs.len()` must
    /// be 2..=3, the same hardware ceiling `nor` enforces (a merge shares
    /// `place_nor_gate`'s three input faces -- see `place_merge_gate`'s own
    /// doc comment).
    ///
    /// Nothing under `circuits/` calls this, and nothing should:
    /// `and4`/`full_adder`/`segment_a`/`seven_segment` (via `or_reduce`,
    /// below) stay built the expensive NOR-decomposed way deliberately, as
    /// the control group the synthesised decoder is measured against. The
    /// synthesised side reaches it through `compile::lowering`, which is
    /// where every `$_OR_`, `$_NAND_`, `$_XOR_` and `$_MUX_` finds its
    /// merge.
    pub(crate) fn merge(&mut self, inputs: &[String]) -> String {
        let output = self.fresh_name();
        self.merge_named(&output.clone(), &output, inputs)
    }

    /// A wire merge carrying a caller-chosen `name` and `output`, for the
    /// same reason [`NetlistBuilder::nor_named`] exists.
    pub(crate) fn merge_named(&mut self, name: &str, output: &str, inputs: &[String]) -> String {
        assert!(
            (2..=3).contains(&inputs.len()),
            "place_merge_gate takes 2 or 3 inputs, got {}",
            inputs.len()
        );
        self.gates.push(Gate {
            name: name.to_string(),
            inputs: inputs.to_vec(),
            output: output.to_string(),
            kind: GateKind::Or(inputs.len()),
        });
        output.to_string()
    }

    /// `NOT x`. One gate per distinct `x`: repeated calls return the cached
    /// output name.
    pub(crate) fn not(&mut self, x: &str) -> String {
        if let Some(cached) = self.not_cache.get(x) {
            return cached.clone();
        }
        let output = self.nor(&[x.to_string()]);
        self.not_cache.insert(x.to_string(), output.clone());
        output
    }

    /// The AND of a signal list of any length, folded into a tree of
    /// fan-in <= 3.
    ///
    /// Each level groups the signals in threes: every signal in a group is
    /// inverted first (hitting the cache if it is a primary input, or an
    /// inversion another minterm already needed), then one NOR gate
    /// computes that group's AND (De Morgan:
    /// `AND(a,b,c) = NOR(NOT a, NOT b, NOT c)`). A signal left over on its
    /// own is promoted to the next level without building a gate.
    pub(crate) fn and_reduce(&mut self, signals: Vec<String>) -> String {
        let mut level = signals;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(3));
            for chunk in level.chunks(3) {
                if chunk.len() == 1 {
                    next.push(chunk[0].clone());
                } else {
                    let nots: Vec<String> = chunk.iter().map(|s| self.not(s)).collect();
                    next.push(self.nor(&nots));
                }
            }
            level = next;
        }
        level.into_iter().next().expect("and_reduce called with an empty signal list")
    }

    /// The OR of a signal list of any length, folded into a tree of
    /// fan-in <= 3.
    ///
    /// `OR(a,b,c) = NOT(NOR(a,b,c))`: each group takes a NOR first, then one
    /// inversion to recover the real OR value, so the next level up can OR
    /// it with the other groups'. A signal left over on its own is promoted
    /// without building a gate.
    pub(crate) fn or_reduce(&mut self, signals: Vec<String>) -> String {
        let mut level = signals;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(3));
            for chunk in level.chunks(3) {
                if chunk.len() == 1 {
                    next.push(chunk[0].clone());
                } else {
                    let nor_out = self.nor(chunk);
                    next.push(self.not(&nor_out));
                }
            }
            level = next;
        }
        level.into_iter().next().expect("or_reduce called with an empty signal list")
    }
}

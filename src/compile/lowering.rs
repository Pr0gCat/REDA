//! Lower a gate-level [`Netlist`] into the two things redstone actually
//! builds: NOR gates and wire merges.
//!
//! This is the pass that took over from `abc -genlib redstone_nor.genlib`.
//! Yosys still runs, and ABC still does the half of its job that is
//! genuinely valuable here -- logic optimisation, which takes the
//! hand-written seven-segment decoder's 84 gates to 31. What it no longer
//! does is *choose the redstone realisation*, because it has no idea what
//! redstone costs and this project's topology library is the thing whose
//! entire job that is. So the frontend reads Yosys's gate-level netlist as
//! it stands (`$_AND_`, `$_NAND_`, `$_XOR_`, `$_MUX_`, ...), and [`lower`]
//! applies `topology::expansion_for` to every gate in it.
//!
//! ```text
//!   Verilog
//!     -> yosys + abc            logic optimisation
//!   gate-level Netlist          $_AND_ $_OR_ $_NAND_ $_XOR_ $_MUX_ ...
//!     -> lowering::lower        THIS MODULE. NOR and torches appear here
//!   realisable Netlist          Nor(1..=3) and Or(2..=3), nothing else
//!     -> compile / primitive_graph::expand
//! ```
//!
//! # Two properties this pass owes its callers
//!
//! - **It is the identity on an already-realisable netlist.** Every
//!   hand-written circuit under `circuits/` is NOR-only, so `lower` returns
//!   them gate-for-gate, name-for-name, in the same order -- which is what
//!   keeps `and4`/`full_adder`/`segment_a`/`seven_segment` at exactly the
//!   block and tick counts they have always had even though every caller now
//!   runs this pass over them. `lowering_is_the_identity_on_a_realisable_netlist`
//!   holds it to that.
//! - **It is idempotent.** `lower(lower(n)) == lower(n)`, which follows
//!   from the first property. `compile` does not lower for its caller (see
//!   its own doc comment for why), so a caller that needs to name the gates
//!   `compile` places -- `mc_dump` and the viewer both do -- lowers once and
//!   passes the same netlist to both, and nothing breaks if it lowers twice.
//!
//! # Names
//!
//! A gate's declared output name survives lowering: the **last** step of an
//! expansion drives it, so anything downstream -- another gate's input, a
//! `Netlist::outputs` entry, an `output_labels` mapping from a Verilog port
//! -- still resolves. Only the intermediate steps need names of their own,
//! and those come from a prefix chosen (in [`fresh_prefix`]) so that no
//! generated name can collide with one already in the netlist.
//!
//! # Inverter sharing
//!
//! Most expansions start by inverting their inputs, because `NOR(x, y)`
//! *is* `!x & !y` -- so `!a` tends to be wanted by many gates at once. Every
//! single-input `Nor` step therefore goes through `NetlistBuilder::not`,
//! whose cache is what makes one torch serve all of them; and [`lower`]
//! seeds that cache with the netlist's *own* pre-existing 1-input NOR gates
//! first, so a `$_NOT_` Yosys already emitted is reused rather than
//! duplicated. This is the same trick `NetlistBuilder::and_reduce` has
//! always used for the hand-written circuits, applied one level up.

use std::collections::HashSet;

use crate::circuits::netlist_builder::NetlistBuilder;
use crate::compile::topology::{self, GateKind, Operand, SignalPolarity, Step};
use crate::compile::Netlist;

/// Why a netlist could not be lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerError {
    /// A gate's declared [`GateKind`] wants a different number of inputs
    /// than the gate actually has. Nothing here can guess which of the two
    /// is right -- a `$_MUX_` with two inputs is a malformed netlist, not a
    /// mux with a missing wire -- so it is a hard error naming both.
    ArityMismatch { gate: String, kind: GateKind, declared: usize, actual: usize },
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::ArityMismatch { gate, kind, declared, actual } => write!(
                f,
                "gate `{gate}` is a {kind:?}, which takes {declared} input(s), but it has {actual}"
            ),
        }
    }
}

impl std::error::Error for LowerError {}

/// Rewrite every gate of `netlist` into NOR gates and wire merges, leaving
/// the circuit's inputs, outputs and every declared output *name* exactly
/// where they were.
///
/// See this module's own doc comment for the two properties this owes its
/// callers, and `topology::expansion_for` for the recipes it applies.
pub fn lower(netlist: &Netlist) -> Result<Netlist, LowerError> {
    Ok(lower_with_provenance(netlist)?.0)
}

/// [`lower`], plus the map that says where each lowered gate came from:
/// element `i` is the index, in `netlist.gates`, of the gate whose expansion
/// produced lowered gate `i`.
///
/// Without this, the two levels are two unrelated graphs the moment lowering
/// runs -- and the consumer that most needs them related is the viewer's
/// topology tab, whose whole job is showing what a circuit really is. With
/// it, a torch can say which `$_MUX_` it is one fifth of.
///
/// One gate can appear under more than one provenance in spirit: an inverter
/// is shared, so the gate that *built* `!a` is credited with it and every
/// later gate that reuses it is not. That is the honest attribution -- the
/// torch exists once -- and it is why this is a map to one source gate
/// rather than to a set.
pub fn lower_with_provenance(netlist: &Netlist) -> Result<(Netlist, Vec<usize>), LowerError> {
    for gate in &netlist.gates {
        if gate.inputs.len() != gate.kind.arity() {
            return Err(LowerError::ArityMismatch {
                gate: gate.output.clone(),
                kind: gate.kind,
                declared: gate.kind.arity(),
                actual: gate.inputs.len(),
            });
        }
    }

    let mut builder = NetlistBuilder::with_prefix(fresh_prefix(netlist));
    let mut provenance: Vec<usize> = Vec::with_capacity(netlist.gates.len());

    for (source, gate) in netlist.gates.iter().enumerate() {
        let expansion = topology::expansion_for(gate.kind);

        // A realisable gate is its own expansion -- one step, over its own
        // inputs -- so it passes through untouched, name included. This is
        // the branch that makes `lower` the identity on every hand-written
        // circuit in this project.
        if gate.kind.is_realisable() {
            debug_assert_eq!(expansion.steps.len(), 1, "a realisable kind expands to exactly itself");
            builder.adopt(gate.clone());
            provenance.push(source);
            continue;
        }

        // Everything else: walk the recipe, naming each intermediate step
        // as the builder sees fit and the final one after the gate itself.
        let output_step = expansion.output_step();
        let mut built: Vec<String> = Vec::with_capacity(expansion.steps.len());

        // Most signed input rails are resolved at their first use. A recipe
        // can explicitly preserve an earlier established inverter prefix,
        // such as XOR/XNOR's `!a`, `!b` pair before either product term.
        let before = builder.len();
        for &pin in &expansion.pre_materialize_negative_inputs {
            builder.not(&gate.inputs[pin]);
        }
        for _ in before..builder.len() {
            provenance.push(source);
        }

        for (index, step) in expansion.steps.iter().enumerate() {
            // Resolving a negative external rail can create its shared
            // inverter, so capture provenance before reading operands.
            let before = builder.len();
            let operands: Vec<String> = step_operands(step)
                .iter()
                .map(|operand| match *operand {
                    Operand::Input { pin, polarity: SignalPolarity::Positive } => gate.inputs[pin].clone(),
                    Operand::Input { pin, polarity: SignalPolarity::Negative } => builder.not(&gate.inputs[pin]),
                    Operand::Step(s) => built[s].clone(),
                })
                .collect();

            let last = index == output_step;
            built.push(match step {
                // A one-input NOR is an inverter, and inverters are shared
                // circuit-wide -- except when this step *is* the gate's
                // output, which has to carry the gate's own declared name
                // and so cannot be somebody else's torch.
                Step::Nor(_) if !last && operands.len() == 1 => builder.not(&operands[0]),
                Step::Nor(_) if last => builder.nor_named(&gate.name, &gate.output, &operands),
                Step::Nor(_) => builder.nor(&operands),
                Step::Merge(_) if last => builder.merge_named(&gate.name, &gate.output, &operands),
                Step::Merge(_) => builder.merge(&operands),
            });
            // A cached inverter builds nothing, so it adds no provenance
            // entry -- the gate that first built it keeps the credit.
            for _ in before..builder.len() {
                provenance.push(source);
            }
        }
    }

    let gates = builder.into_gates();
    debug_assert_eq!(gates.len(), provenance.len(), "one provenance entry per lowered gate");
    Ok((Netlist { inputs: netlist.inputs.clone(), outputs: netlist.outputs.clone(), gates }, provenance))
}

fn step_operands(step: &Step) -> &[Operand] {
    match step {
        Step::Nor(operands) | Step::Merge(operands) => operands,
    }
}

/// A name prefix no generated name can collide with: the first of `"n"`,
/// `"n_"`, `"n__"`, ... that no signal name already in `netlist` starts
/// with. Since every generated name is that prefix followed by decimal
/// digits, "nothing starts with it" is more than enough to guarantee
/// freshness, and it keeps the common case (`n0`, `n1`, ...) readable.
fn fresh_prefix(netlist: &Netlist) -> String {
    let mut taken: HashSet<&str> = HashSet::new();
    for name in &netlist.inputs {
        taken.insert(name.as_str());
    }
    for gate in &netlist.gates {
        taken.insert(gate.output.as_str());
        for input in &gate.inputs {
            taken.insert(input.as_str());
        }
    }

    let mut prefix = String::from("n");
    while taken.iter().any(|name| name.starts_with(&prefix)) {
        prefix.push('_');
    }
    prefix
}

/// How many gates of each [`GateKind`] a netlist has, sorted by kind -- the
/// histogram that answers "what is this circuit made of", which is a
/// question worth being able to ask now that the answer is not "NOR, by
/// construction". Used by `bake_verilog` and `mc_dump` to report a circuit,
/// and by tests to pin one.
pub fn cell_type_histogram(netlist: &Netlist) -> Vec<(GateKind, usize)> {
    let mut counts: std::collections::BTreeMap<GateKind, usize> = std::collections::BTreeMap::new();
    for gate in &netlist.gates {
        *counts.entry(gate.kind).or_insert(0) += 1;
    }
    counts.into_iter().collect()
}

/// Render [`cell_type_histogram`] as `"and:5 nand:9 nor:3 ..."` -- one line,
/// stable order, for logs and test failure messages.
pub fn format_histogram(netlist: &Netlist) -> String {
    cell_type_histogram(netlist)
        .into_iter()
        .map(|(kind, count)| {
            if kind.is_realisable() {
                format!("{}{}:{count}", kind.wire_name(), kind.arity())
            } else {
                format!("{}:{count}", kind.wire_name())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::Gate;

    fn netlist(inputs: &[&str], outputs: &[&str], gates: Vec<Gate>) -> Netlist {
        Netlist {
            inputs: inputs.iter().map(|s| (*s).to_string()).collect(),
            outputs: outputs.iter().map(|s| (*s).to_string()).collect(),
            gates,
        }
    }

    fn gate(kind: GateKind, output: &str, inputs: &[&str]) -> Gate {
        Gate {
            name: output.to_string(),
            inputs: inputs.iter().map(|s| (*s).to_string()).collect(),
            output: output.to_string(),
            kind,
        }
    }

    /// Evaluate a **realisable** netlist directly: a NOR gate NORs, a merge
    /// ORs. Written here rather than reused from anywhere, so the check
    /// below compares lowering's output against the meaning of redstone,
    /// not against lowering's own idea of it.
    fn evaluate(netlist: &Netlist, inputs: &[(&str, bool)]) -> std::collections::HashMap<String, bool> {
        let mut values: std::collections::HashMap<String, bool> =
            inputs.iter().map(|&(name, value)| (name.to_string(), value)).collect();
        for index in netlist.topological_order().expect("acyclic") {
            let gate = &netlist.gates[index];
            let read: Vec<bool> = gate.inputs.iter().map(|input| values[input]).collect();
            let value = match gate.kind {
                GateKind::Nor(_) => !read.iter().any(|&b| b),
                GateKind::Or(_) => read.iter().any(|&b| b),
                other => panic!("a lowered netlist has no {other:?} gate left in it"),
            };
            values.insert(gate.output.clone(), value);
        }
        values
    }

    #[test]
    fn lowering_is_the_identity_on_a_realisable_netlist() {
        // Exactly the shape every hand-written circuit under `circuits/`
        // has, including a merge and a gate whose `name` differs from its
        // `output` -- all of it must come back untouched, in order.
        let original = netlist(
            &["a", "b"],
            &["y"],
            vec![
                Gate { name: "not_a".to_string(), inputs: vec!["a".to_string()], output: "na".to_string(), kind: GateKind::Nor(1) },
                gate(GateKind::Nor(1), "nb", &["b"]),
                gate(GateKind::Or(2), "m", &["na", "nb"]),
                gate(GateKind::Nor(1), "y", &["m"]),
            ],
        );
        assert_eq!(lower(&original).expect("realisable"), original);
    }

    #[test]
    fn lowering_is_idempotent() {
        let original = netlist(
            &["a", "b", "s"],
            &["y"],
            vec![gate(GateKind::Mux, "y", &["a", "b", "s"])],
        );
        let once = lower(&original).expect("lowers");
        let twice = lower(&once).expect("lowers again");
        assert_eq!(once, twice);
    }

    #[test]
    fn lowering_xor_and_xnor_keep_the_baseline_inverter_sequence_and_names() {
        for (kind, output_kind) in [(GateKind::Xor, GateKind::Or(2)), (GateKind::Xnor, GateKind::Nor(2))] {
            let original = netlist(&["a", "b"], &["y"], vec![gate(kind, "y", &["a", "b"])]);
            let expected = netlist(
                &["a", "b"],
                &["y"],
                vec![
                    gate(GateKind::Nor(1), "n0", &["a"]),
                    gate(GateKind::Nor(1), "n1", &["b"]),
                    gate(GateKind::Nor(2), "n2", &["n0", "b"]),
                    gate(GateKind::Nor(2), "n3", &["n1", "a"]),
                    gate(output_kind, "y", &["n2", "n3"]),
                ],
            );
            assert_eq!(lower(&original).expect("lowers"), expected, "{kind:?}");
        }
    }

    /// Every gate-level kind, lowered on its own, must compute its own
    /// truth table when the resulting NOR-and-merge netlist is evaluated as
    /// redstone -- the end-to-end version of `topology`'s own recipe check,
    /// this time through the real `NetlistBuilder` and the real naming.
    #[test]
    fn every_lowered_gate_computes_its_own_truth_table() {
        let pins = ["p0", "p1", "p2", "p3"];
        for kind in [
            GateKind::Nor(1),
            GateKind::Nor(2),
            GateKind::Nor(3),
            GateKind::Or(2),
            GateKind::Or(3),
            GateKind::Buf,
            GateKind::And,
            GateKind::Nand,
            GateKind::Xor,
            GateKind::Xnor,
            GateKind::AndNot,
            GateKind::OrNot,
            GateKind::Aoi3,
            GateKind::Oai3,
            GateKind::Aoi4,
            GateKind::Oai4,
            GateKind::Mux,
            GateKind::Nmux,
        ] {
            let arity = kind.arity();
            let ports: Vec<&str> = pins[..arity].to_vec();
            let original = netlist(&ports, &["y"], vec![gate(kind, "y", &ports)]);
            let lowered = lower(&original).expect("every kind lowers");

            assert!(
                lowered.gates.iter().all(|g| g.kind.is_realisable()),
                "{kind:?} lowered to something that is not a torch or a merge"
            );

            for bits in 0..(1u32 << arity) {
                let assignment: Vec<(&str, bool)> =
                    ports.iter().enumerate().map(|(i, &p)| (p, (bits >> i) & 1 == 1)).collect();
                let values: Vec<bool> = assignment.iter().map(|&(_, v)| v).collect();
                let got = evaluate(&lowered, &assignment)["y"];
                assert_eq!(got, kind.evaluate(&values), "{kind:?} on {values:?}");
            }
        }
    }

    /// The gate's declared output name has to survive, or every consumer of
    /// it -- another gate, a `Netlist::outputs` entry, a Verilog port label
    /// -- silently stops resolving.
    #[test]
    fn a_lowered_gate_still_drives_its_own_declared_output_name() {
        let original = netlist(&["a", "b"], &["y"], vec![gate(GateKind::Nand, "y", &["a", "b"])]);
        let lowered = lower(&original).expect("lowers");
        let driver = lowered.gates.iter().find(|g| g.output == "y").expect("`y` must still be driven");
        assert!(driver.is_merge(), "NAND's output stage is a merge: `!(a & b) == !a | !b`");
    }

    /// Inverters are shared, both with each other and with a `$_NOT_` the
    /// netlist already contained -- otherwise every gate would drag its own
    /// copy of `!a` into the floorplan.
    #[test]
    fn inverters_are_shared_across_gates_and_with_existing_nor1_gates() {
        let original = netlist(
            &["a", "b"],
            &["y", "z", "na"],
            vec![
                gate(GateKind::Nor(1), "na", &["a"]),
                gate(GateKind::And, "y", &["a", "b"]),
                gate(GateKind::And, "z", &["a", "b"]),
            ],
        );
        let lowered = lower(&original).expect("lowers");

        // `!a` and `!b` once each (the pre-existing `na` serving as `!a`),
        // plus one output torch per AND: four gates, not six.
        assert_eq!(lowered.gates.len(), 4, "got: {}", format_histogram(&lowered));
        let inverters: Vec<&Gate> = lowered.gates.iter().filter(|g| g.inputs.len() == 1).collect();
        assert_eq!(inverters.len(), 2, "one inverter per primary input, shared by both ANDs");
        assert!(inverters.iter().any(|g| g.output == "na"), "the netlist's own `$_NOT_` must be the one reused");
    }

    #[test]
    fn a_generated_name_never_collides_with_one_the_netlist_already_uses() {
        // Every plausible generated name is already taken as a port.
        let ports: Vec<String> = (0..8).map(|i| format!("n{i}")).collect();
        let port_refs: Vec<&str> = ports.iter().map(String::as_str).collect();
        let original = netlist(
            &port_refs[..2],
            &["y"],
            vec![gate(GateKind::Xor, "y", &[port_refs[0], port_refs[1]])],
        );
        let lowered = lower(&original).expect("lowers");

        let mut seen: HashSet<&str> = port_refs.iter().copied().collect();
        for gate in &lowered.gates {
            assert!(seen.insert(gate.output.as_str()), "`{}` collides with a name already in use", gate.output);
        }
    }

    #[test]
    fn an_arity_mismatch_is_a_hard_error_naming_the_gate_and_the_kind() {
        let original = netlist(&["a", "b"], &["y"], vec![gate(GateKind::Mux, "y", &["a", "b"])]);
        let error = lower(&original).expect_err("a two-input mux is malformed");
        assert_eq!(
            error,
            LowerError::ArityMismatch { gate: "y".to_string(), kind: GateKind::Mux, declared: 3, actual: 2 }
        );
    }

    /// Provenance has to account for every lowered gate, and point back at a
    /// real source gate -- that is what lets a viewer say which `$_MUX_` a
    /// given torch is one fifth of.
    #[test]
    fn provenance_credits_every_lowered_gate_to_a_real_source_gate() {
        let original = netlist(
            &["a", "b", "s"],
            &["y", "z"],
            vec![gate(GateKind::Mux, "y", &["a", "b", "s"]), gate(GateKind::Nand, "z", &["a", "b"])],
        );
        let (lowered, provenance) = lower_with_provenance(&original).expect("lowers");

        assert_eq!(provenance.len(), lowered.gates.len(), "one entry per lowered gate");
        assert!(provenance.iter().all(|&i| i < original.gates.len()), "every entry names a real source gate");

        // Every source gate is credited with at least the gate carrying its
        // own declared output.
        for (source, gate) in original.gates.iter().enumerate() {
            let driver = lowered.gates.iter().position(|g| g.output == gate.output).expect("output survives");
            assert_eq!(provenance[driver], source, "`{}`'s own output gate belongs to it", gate.output);
        }

        // The NAND reuses both inverters the MUX already built, so it is
        // credited with one gate (its merge) and the MUX with the rest.
        let mux_owned = provenance.iter().filter(|&&i| i == 0).count();
        let nand_owned = provenance.iter().filter(|&&i| i == 1).count();
        assert_eq!(nand_owned, 1, "a shared inverter is credited to whoever built it, not to every reuser");
        assert_eq!(mux_owned + nand_owned, lowered.gates.len());
    }

    #[test]
    fn the_histogram_names_every_kind_present() {
        let original = netlist(
            &["a", "b", "c"],
            &["y"],
            vec![
                gate(GateKind::Nand, "n0", &["a", "b"]),
                gate(GateKind::Nand, "n1", &["b", "c"]),
                gate(GateKind::Xor, "y", &["n0", "n1"]),
            ],
        );
        assert_eq!(format_histogram(&original), "nand:2 xor:1");
    }
}

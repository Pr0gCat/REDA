//! Gate mapping: the logic graph becomes a [`Netlist`].
//!
//! The mapping is deliberately direct and deliberately narrow. `Not` becomes
//! `Nor(1)`, `And`/`Xor`/`Mux` become their own gate-level kinds, `Or`
//! becomes a declared wire merge, and a passthrough output becomes `Buf`.
//! Nothing here emits `Nand`, `Xnor`, `AndNot`, `Aoi*`, or `Nor(2)`:
//! polarity assignment and optimised lowering are `compile::lowering`'s
//! decisions, made against its own redstone cost model, and making them here
//! would be the exact mistake the old `redstone_nor.genlib` made -- handing
//! the topology library a design whose realisation had already been chosen.
//!
//! Gate names come from the shared [`NetlistBuilder`], so this frontend and
//! the Yosys one cannot drift into two naming policies. Because those names
//! are generated in emission order, and emission order follows logic node
//! IDs, they depend only on the structure of the design -- not on where its
//! tokens happened to sit in the file.

use crate::circuits::netlist_builder::NetlistBuilder;
use crate::compile::topology::GateKind;
use crate::compile::Netlist;

use super::ast::PortDirection;
use super::debug::{
    BitBinding, DebugNode, GateRef, Realisation, SignalBinding, TransformReason, Transformation,
};
use super::elaborate::{bit_name, Design};
use super::logic::{Blasted, LogicGraph, LogicNode, LogicNodeId};
use super::{Diagnostic, PortBinding, Severity};

/// What gate mapping produced, beside the netlist itself.
pub struct Emitted {
    pub netlist: Netlist,
    /// Every emitted gate by netlist index.
    pub gates: Vec<GateRef>,
    pub ports: Vec<PortBinding>,
    /// Indexed by [`LogicNodeId`].
    pub realisations: Vec<Realisation>,
    pub transformations: Vec<Transformation>,
    pub signals: Vec<SignalBinding>,
}

pub fn emit(design: &Design, blasted: &Blasted) -> Result<Emitted, Vec<Diagnostic>> {
    let graph = &blasted.graph;
    let mut errors = Vec::new();
    let mut builder = NetlistBuilder::new();
    let mut transformations = Vec::new();

    // The signal each live node drives, once it has been emitted. A primary
    // input drives its own name and needs no gate.
    let mut signal_of: Vec<Option<String>> = vec![None; graph.nodes.len()];
    let mut realisations: Vec<Realisation> = vec![Realisation::Dead; graph.nodes.len()];

    let mut inputs: Vec<String> = Vec::new();
    for (_, signal) in design.inputs() {
        for bit in 0..signal.width {
            inputs.push(bit_name(&signal.name, signal.width, bit));
        }
    }

    // Leaves first, in either order: a primary input keeps its declared
    // name regardless of whether anything reads it, matching a DFF's
    // eventual `Realisation::Input` even when the design never uses it.
    for index in 0..graph.nodes.len() {
        match graph.node(LogicNodeId(index as u32)) {
            LogicNode::Input(name) => {
                signal_of[index] = Some(name.clone());
                realisations[index] = Realisation::Input(name.clone());
            }
            LogicNode::Const(value) => realisations[index] = Realisation::Const(*value),
            _ => {}
        }
    }

    // Node IDs are topological by construction -- interning builds every
    // operand before the operation that reads it -- with one deliberate
    // exception: a hold mux's `en ? d : q` reads the register's own D flip-
    // flop, whose node ID `reserve_dff` had to mint *before* `d` existed.
    // `resolve` recurses on demand so that one back-reference does not need
    // every node reachable from it to also be visited out of order; for
    // every other node it immediately hits the `signal_of` entry the linear
    // pass below already filled in, so gate emission order is unchanged
    // from a plain forward scan.
    for index in 0..graph.nodes.len() {
        let id = LogicNodeId(index as u32);
        if matches!(graph.node(id), LogicNode::Input(_) | LogicNode::Const(_)) {
            continue; // a leaf, already realised above regardless of liveness
        }
        if signal_of[index].is_some() || !graph.live.get(index).copied().unwrap_or(false) {
            continue;
        }
        resolve(id, graph, &mut builder, &mut signal_of, &mut realisations);
    }

    // Output ports. A port whose bit is a bare primary input (or a constant)
    // has no gate of its own, and `compile` requires every declared output to
    // be driven by a real gate -- so a passthrough gets the same `Buf` the
    // Yosys bridge synthesizes for `assign y = a;`.
    let mut outputs: Vec<String> = Vec::new();
    let mut ports: Vec<PortBinding> = Vec::new();
    let mut buffer_of: Vec<Option<String>> = vec![None; graph.nodes.len()];

    for (_, signal) in design.inputs() {
        for bit in 0..signal.width {
            ports.push(PortBinding {
                name: bit_name(&signal.name, signal.width, bit),
                direction: PortDirection::Input,
                signal: bit_name(&signal.name, signal.width, bit),
                elab: signal.elab,
                bit,
            });
        }
    }

    for (index, signal) in design.outputs() {
        for bit in 0..signal.width {
            let node = blasted.signal_bits[index][bit as usize];
            let name = bit_name(&signal.name, signal.width, bit);
            if let Some(value) = graph.const_value(node) {
                errors.push(Diagnostic {
                    severity: Severity::Error,
                    message: format!(
                        "unsupported: output `{name}` folds to the constant {}; REDA has no \
                         physical constant driver in `Netlist` yet",
                        u8::from(value)
                    ),
                    span: signal.span,
                });
                continue;
            }

            let driver = signal_of[node.0 as usize]
                .clone()
                .expect("every live node has a signal");
            let needs_buffer = matches!(graph.node(node), LogicNode::Input(_));
            let output = if needs_buffer {
                match &buffer_of[node.0 as usize] {
                    Some(existing) => existing.clone(),
                    None => {
                        let buffered = builder.cell(GateKind::Buf, &[driver]);
                        let gate_index = (builder.len() - 1) as u32;
                        // The buffered node already has a realisation of its
                        // own (it is a primary input), so this gate is
                        // recorded as the mapping event it is.
                        transformations.push(Transformation {
                            reason: TransformReason::Map,
                            inputs: vec![DebugNode::Logic(node)],
                            outputs: vec![DebugNode::Gate(gate_index)],
                        });
                        buffer_of[node.0 as usize] = Some(buffered.clone());
                        buffered
                    }
                }
            } else {
                driver
            };

            if !outputs.contains(&output) {
                outputs.push(output.clone());
            }
            ports.push(PortBinding {
                name,
                direction: PortDirection::Output,
                signal: output,
                elab: signal.elab,
                bit,
            });
        }
    }

    if !errors.is_empty() {
        errors.sort_by_key(|diagnostic| (diagnostic.span.file, diagnostic.span.start));
        return Err(errors);
    }

    // One statement-level mapping row per assignment: which gates this
    // `assign` ended up as. Spans already answer the same question through
    // the expression nodes inside it; this says it once at the granularity
    // a person actually edits, and is what an editor highlights when the
    // cursor is on the statement rather than on a subexpression.
    for assign in &design.assigns {
        let mut gates: Vec<DebugNode> = Vec::new();
        for bit in assign.lsb..assign.lsb + assign.width {
            let node = blasted.signal_bits[assign.signal][bit as usize];
            if let Realisation::Gate(gate) = &realisations[node.0 as usize] {
                let mapped = DebugNode::Gate(gate.index);
                if !gates.contains(&mapped) {
                    gates.push(mapped);
                }
            }
        }
        if !gates.is_empty() {
            transformations.push(Transformation {
                reason: TransformReason::Map,
                inputs: vec![DebugNode::Elab(assign.elab)],
                outputs: gates,
            });
        }
    }

    // Sorted by port name for deterministic transport, exactly as the API
    // contract promises.
    ports.sort_by(|a, b| a.name.cmp(&b.name));

    let signals = design
        .signals
        .iter()
        .enumerate()
        .map(|(index, signal)| SignalBinding {
            elab: signal.elab,
            bits: (0..signal.width)
                .map(|bit| {
                    let node = blasted.signal_bits[index][bit as usize];
                    match graph.node(node) {
                        LogicNode::Const(value) => BitBinding::Const(*value),
                        LogicNode::Input(name) => BitBinding::Input(name.clone()),
                        _ => BitBinding::Logic(node),
                    }
                })
                .collect(),
        })
        .collect();

    let netlist = Netlist {
        inputs,
        outputs,
        gates: builder.into_gates(),
    };
    let gates = netlist
        .gates
        .iter()
        .enumerate()
        .map(|(index, gate)| GateRef {
            index: index as u32,
            output: gate.output.clone(),
        })
        .collect();

    Ok(Emitted {
        netlist,
        gates,
        ports,
        realisations,
        transformations,
        signals,
    })
}

/// The signal name `id` drives, building whatever gates it and its
/// not-yet-resolved operands need, memoized through `signal_of`. Leaves
/// (`Input`/`Const`) are always pre-resolved by the caller.
///
/// A DFF resolves its `clock` first, then reserves its own output name and
/// records it in `signal_of` *before* resolving `data` -- so if `data`
/// reads the register's own current value (a hold mux), that recursive call
/// hits the reservation above instead of recursing forever.
fn resolve(
    id: LogicNodeId,
    graph: &LogicGraph,
    builder: &mut NetlistBuilder,
    signal_of: &mut [Option<String>],
    realisations: &mut [Realisation],
) -> String {
    let index = id.0 as usize;
    if let Some(name) = &signal_of[index] {
        return name.clone();
    }
    let output = match graph.node(id).clone() {
        LogicNode::Input(_) | LogicNode::Const(_) => {
            unreachable!("leaves are resolved before `resolve` is ever called")
        }
        LogicNode::Not(a) => {
            let a = resolve(a, graph, builder, signal_of, realisations);
            builder.nor(&[a])
        }
        LogicNode::And(a, b) => {
            let a = resolve(a, graph, builder, signal_of, realisations);
            let b = resolve(b, graph, builder, signal_of, realisations);
            builder.cell(GateKind::And, &[a, b])
        }
        LogicNode::Or(a, b) => {
            let a = resolve(a, graph, builder, signal_of, realisations);
            let b = resolve(b, graph, builder, signal_of, realisations);
            builder.merge(&[a, b])
        }
        LogicNode::Xor(a, b) => {
            let a = resolve(a, graph, builder, signal_of, realisations);
            let b = resolve(b, graph, builder, signal_of, realisations);
            builder.cell(GateKind::Xor, &[a, b])
        }
        LogicNode::Mux {
            select,
            when_false,
            when_true,
        } => {
            // `GateKind::Mux` is Yosys's `$_MUX_`: pins A, B, S with
            // `Y = S ? B : A`.
            let a = resolve(when_false, graph, builder, signal_of, realisations);
            let b = resolve(when_true, graph, builder, signal_of, realisations);
            let s = resolve(select, graph, builder, signal_of, realisations);
            builder.cell(GateKind::Mux, &[a, b, s])
        }
        LogicNode::Dff { clock, data, .. } => {
            let clock = resolve(clock, graph, builder, signal_of, realisations);
            let reserved = builder.reserve_name();
            signal_of[index] = Some(reserved.clone());
            let data = resolve(data, graph, builder, signal_of, realisations);
            // GateKind::DffPosedge's fixed pin order is [D, C].
            builder.cell_named(&reserved, GateKind::DffPosedge, &[data, clock])
        }
    };
    realisations[index] = Realisation::Gate(GateRef {
        index: (builder.len() - 1) as u32,
        output: output.clone(),
    });
    signal_of[index] = Some(output.clone());
    output
}

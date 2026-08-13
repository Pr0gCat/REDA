//! Turning a netlist and its primitive graph into the thing that relaxes.
//!
//! Placement's graph is not quite the primitive graph, and this module is
//! where they part. Two differences, both about wire merges:
//!
//! - a **bare** merge contributes no primitive, and `expand` splices its
//!   consumers straight onto its producers. Springs alone would pull those two
//!   groups together and never notice the junction between them, so the
//!   junction is added as a body and the pulls are routed through it.
//! - an **isolated** merge contributes one repeater per branch, and that
//!   repeater's cell is the junction's socket for that branch -- which is what
//!   `world_partition::resolve_node_position` already answers. It is welded
//!   there, not relaxed freely.
//!
//! Pulls are built from the netlist's declared inputs rather than from
//! `graph.edges`, which gets the first of those for nothing: looking a
//! producer up by signal name returns a bare merge's own junction, where
//! walking edges would have skipped it.

use crate::compile::geometry::{self, CellFacing};
use crate::compile::physical::{self, PortKind, RelativeSide};
use crate::compile::planner::{Anchor, PortPlacements};
use crate::compile::primitive_graph::{NodeId, PrimitiveGraph, Provenance};
use crate::compile::topology::{Primitive, TemplateNode};
use crate::compile::Netlist;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::BlockKind;

/// Every signal spring pulls the same.
///
/// The spec defers per-edge weighting to the criticality question, and a
/// stiffness that varies without a measurement behind it is the sort of number
/// this project has already spent time removing from the planner.
///
/// The one exception the spec names -- cell cohesion, at the graph's maximum
/// degree -- has no caller in this stage. Every gate with more than one body
/// today is a merge, and a merge holds itself together with a [`Weld`] rather
/// than with a stiff spring. Cohesion arrives with Design H, which is the
/// first gate whose members are genuinely free to move apart.
pub const SIGNAL_STIFFNESS: f64 = 1.0;

/// One thing relaxation may move.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    pub what: BodyKind,
    pub position: [f64; 3],
    /// The net each declared input arrives on, in declared order.
    ///
    /// Copied from the netlist at build time rather than looked up later,
    /// because a facing changes where a body's cells are and never changes
    /// what they carry -- so the labels are settled once and the offsets are
    /// recomputed every round.
    pub inputs: Vec<String>,
    /// The net this body drives, if it drives one. `None` for a body that is
    /// not the one carrying its gate's output -- an isolated branch's
    /// repeater drives into the junction, not out of the gate.
    pub output: Option<String>,
    /// One of four. Never continuous: a body's best facing is found by trying
    /// all four against the pulls on its ports, so there is no angle to
    /// integrate and none to quantise later.
    pub facing: CellFacing,
    /// Fixed by `PortPlacements`. A pinned body contributes force to its
    /// neighbours and takes none, and `snap` returns it where it was pinned.
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// A component the primitive graph named. Its blocks include whatever it
    /// stands on or attaches to; `physical.rs` has always said so.
    Primitive { node: NodeId, kind: Primitive },
    /// A declared wire merge. `expand` produces no primitive for one, and
    /// `place_merge_gate` writes blocks at its anchor regardless.
    Junction { gate: usize },
}

/// Where on a body a spring attaches.
///
/// `Socket` and `Pin` are gate-cell geometry, shared by a NOR and a merge
/// because `place_merge_gate` is built to a NOR's exact footprint. `Port` is
/// for the primitives whose endpoints `physical.rs` names, which is how an
/// isolated branch's repeater says it reads at its rear.
///
/// A NOR's three declared inputs are three *sockets*, not three uses of one
/// `TorchInput` port: they arrive on three different faces, and collapsing
/// them onto the support's one port would tell the solver they are the same
/// place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// The cell a gate's `index`-th declared input arrives in. Air until the
    /// router fills it, and part of the gate either way -- `gate_footprint`
    /// counts it a conductor, because what ends up there is dust or a
    /// repeater.
    Socket(usize),
    /// The cell a gate's outgoing net starts from: one hop out from its torch
    /// for a NOR, one hop out from its junction for a merge.
    Pin,
    /// A port `physical.rs` names, for a body placed as a primitive rather
    /// than as a gate cell.
    Port(PortKind),
}

/// A spring, attached at a point on each end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pull {
    pub from: (usize, Attach),
    pub to: (usize, Attach),
    pub stiffness: f64,
}

/// A relation between two bodies that must hold exactly. Projected, never
/// pulled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weld {
    /// An isolated merge branch's repeater, in the junction's socket for that
    /// branch.
    AtSocket {
        repeater: usize,
        junction: usize,
        input_index: usize,
    },
    /// Design H's lock repeater at the data repeater's side. No caller yet:
    /// `compile()` rejects `GateKind::DffPosedge` before placement, so there
    /// is no Design H region to place.
    BesideAt {
        lock: usize,
        data: usize,
        side: RelativeSide,
    },
}

/// Everything relaxation moves, and everything that decides where.
#[derive(Debug, Clone)]
pub struct BodyGraph {
    pub bodies: Vec<Body>,
    pub pulls: Vec<Pull>,
    pub welds: Vec<Weld>,
    /// Which bodies belong to each of `PlanCandidate`'s nodes -- gates first,
    /// then primary inputs, which is the positional order `emit_primitives`
    /// reads. This is what `snap` collapses through.
    pub nodes: Vec<Vec<usize>>,
    /// Which body carries each node's anchor: a NOR's torch, a merge's
    /// junction, an input's lever.
    pub anchor_body: Vec<usize>,
}

/// How many hops out along a body's output face its pin sits.
///
/// One for everything except a NOR, whose torch stands in the first hop and
/// whose pin is therefore the second: `place_merge_gate` puts a merge's pin
/// one hop from its junction, and `place_primary_input` puts a lever's one hop
/// from the lever. An earlier draft keyed this off [`BodyKind`], which made a
/// lever's pin two hops out here and one hop out everywhere it is really
/// written.
pub fn pin_hops(body: &Body) -> i32 {
    match body.what {
        BodyKind::Primitive { kind: Primitive::Torch, .. } => 2,
        _ => 1,
    }
}

/// Where an attachment sits relative to its body's own position.
///
/// A gate cell's origin is its support (a NOR) or its junction (a merge), and
/// both put their sockets on `geometry::input_directions` and their pin out
/// along `geometry::output_direction`, [`pin_hops`] of them.
pub fn attach_offset(attach: Attach, body: &Body) -> [f64; 3] {
    let facing = body.facing;
    match attach {
        Attach::Socket(index) => {
            let direction = geometry::input_directions(facing)[index];
            let step = Position::new(0, 0, 0).offset(direction);
            [step.x as f64, step.y as f64, step.z as f64]
        }
        Attach::Pin => {
            let direction = geometry::output_direction(facing);
            let mut step = Position::new(0, 0, 0);
            for _ in 0..pin_hops(body) {
                step = step.offset(direction);
            }
            [step.x as f64, step.y as f64, step.z as f64]
        }
        Attach::Port(kind) => {
            let BodyKind::Primitive { kind: primitive, .. } = body.what else {
                unreachable!("a junction has no `physical` port; use Socket or Pin")
            };
            let port = physical::variants(primitive)[usize::from(facing.index())].port(kind);
            [
                port.position.x as f64,
                port.position.y as f64,
                port.position.z as f64,
            ]
        }
    }
}

/// One cell a body occupies, and every net that may lawfully touch it.
///
/// A list rather than one name, because a NOR's support is the sink of *all*
/// its input branches at once and each of them is allowed against it. An
/// earlier draft carried only the first, on the argument that what mattered
/// was being neither inert nor on the output net. It mattered more than that:
/// with one label, a two-input NOR's support says `a` while its second socket
/// says `b`, and separation then pushes `b`'s producer away from the very
/// socket the springs are pulling it onto -- on every gate of arity two or
/// more, for every input past the first.
///
/// Empty means inert: a repeater's floor, a junction's floor, a lever's two.
/// Nothing has to keep clear of it beyond cell exclusivity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub offset: (i32, i32, i32),
    pub carries: Vec<String>,
}

/// Every cell `body` occupies, and what each carries.
///
/// A gate cell's answer is `place_nor_gate`'s and `place_merge_gate`'s, stated
/// as signals rather than as blocks:
///
/// - the **support** (or junction) carries every declared input's net -- all
///   of them, not the first. It is the gate's input node: dust laid against it
///   powers it, and a NOR is N sources into one sink.
/// - each **socket** carries its own branch's net, and is a conductor even
///   though it is air: what ends up there is dust or a repeater.
/// - the **torch** and the **pin** carry the gate's own output net.
/// - a **floor** -- a repeater's, a junction's, a lever's -- carries nothing.
pub fn cells(body: &Body) -> Vec<Cell> {
    let facing = body.facing;
    let mut cells = Vec::new();

    match (&body.output, body.inputs.is_empty()) {
        // A body carrying a gate's output is a gate cell: support or junction,
        // sockets, torch, pin.
        (Some(output), _) => {
            // The support or junction is the gate's input node -- N sources
            // into one sink -- so every input net may lawfully touch it, and
            // all of them are named.
            //
            // A lever has no inputs and its own block still conducts, on the
            // net it drives. Labelling it inert would let a foreign net run
            // straight against a lever.
            let mut sink = body.inputs.clone();
            if sink.is_empty() {
                sink.push(output.clone());
            }
            cells.push(Cell {
                offset: (0, 0, 0),
                carries: sink,
            });

            for (index, signal) in body.inputs.iter().enumerate() {
                let step = Position::new(0, 0, 0).offset(geometry::input_directions(facing)[index]);
                cells.push(Cell {
                    offset: (step.x, step.y, step.z),
                    carries: vec![signal.clone()],
                });
            }

            let out = geometry::output_direction(facing);
            let mut step = Position::new(0, 0, 0);
            for _ in 0..pin_hops(body) {
                step = step.offset(out);
                cells.push(Cell {
                    offset: (step.x, step.y, step.z),
                    carries: vec![output.clone()],
                });
            }

            // What each placer actually lays underneath, which is not the same
            // for all three gate cells.
            match body.what {
                // `place_merge_gate` floors its own junction (`ensure_floor`
                // before the dust), and nothing else it writes.
                BodyKind::Junction { .. } => {
                    cells.push(Cell {
                        offset: (0, -1, 0),
                        carries: Vec::new(),
                    });
                }
                // `place_primary_input` floors the lever's home *and* its pin
                // -- both `ensure_floor` calls -- and `physical::variants`
                // declares the same `DOWN: Solid` on every lever variant. A
                // lever body takes this gate-cell arm and so never consults
                // `physical`, which is how both went missing.
                BodyKind::Primitive {
                    kind: Primitive::Lever,
                    ..
                } => {
                    let home_floor = Position::new(0, 0, 0).down();
                    let pin_floor = step.down();
                    cells.push(Cell {
                        offset: (home_floor.x, home_floor.y, home_floor.z),
                        carries: Vec::new(),
                    });
                    cells.push(Cell {
                        offset: (pin_floor.x, pin_floor.y, pin_floor.z),
                        carries: Vec::new(),
                    });
                }
                // A NOR floors nothing. `place_nor_gate` writes stone *at* the
                // support rather than beneath it, hangs the torch on that
                // support's wall (`wall_torch`, no floor), and leaves its pin's
                // floor to the route that reaches the cell -- exactly as
                // `place_merge_gate` leaves its own pin's.
                _ => {}
            }
        }
        // Anything else is placed as a primitive: an isolated branch's
        // repeater, or a primary input's lever. `physical.rs` says which cells
        // it occupies, and a variant's blocks already include what it stands
        // on.
        (None, _) => {
            let BodyKind::Primitive { kind, .. } = body.what else {
                unreachable!("a junction always carries its gate's output")
            };
            let variant = &physical::variants(kind)[usize::from(facing.index())];
            for block in variant.blocks {
                cells.push(Cell {
                    offset: (block.position.x, block.position.y, block.position.z),
                    carries: match block.kind {
                        // A repeater's floor is inert: a net may run beside it.
                        BlockKind::Solid => Vec::new(),
                        // Everything else is the component itself, on the net
                        // it repeats. A primitive placed this way always has
                        // exactly one input -- an isolated branch's repeater --
                        // and an empty list here would mean "inert", which is
                        // the one thing this cell is not.
                        _ => vec![body
                            .inputs
                            .first()
                            .cloned()
                            .expect("a primitive placed without an output repeats an input")],
                    },
                });
            }
        }
    }
    cells
}

/// Bodies, pulls and welds for `netlist`, started from `start`.
///
/// `start` is one anchor per `PlanCandidate` node -- gates, then primary
/// inputs -- which is what `planner::starting_layout`'s depth-and-barycentre
/// rows produce. Relaxation improves a known-bad answer rather than inventing
/// one, and the improvement is measurable against the numbers it started from.
///
/// `start` is only a guess, and a port `pinned` names ignores it: a pinned
/// body starts at its pin. Nothing afterwards would put it there -- the solve
/// strikes pinned bodies out, `perturb` skips them, `separate` displaces their
/// neighbour instead -- so seeding it here is what makes `Body::pinned`'s
/// promise `build`'s guarantee rather than its caller's discipline.
/// `starting_layout` already agrees; `build`'s own tests deliberately do not.
pub fn build(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    start: &[Anchor],
    pinned: &PortPlacements,
) -> Result<BodyGraph, String> {
    let node_count = netlist.gates.len() + netlist.inputs.len();
    assert_eq!(start.len(), node_count, "one start anchor per candidate node");

    let mut bodies: Vec<Body> = Vec::new();
    let mut nodes: Vec<Vec<usize>> = vec![Vec::new(); node_count];
    let mut anchor_body: Vec<usize> = vec![usize::MAX; node_count];
    let mut welds: Vec<Weld> = Vec::new();

    for (gate_index, gate) in netlist.gates.iter().enumerate() {
        // A gate cell has three input faces -- the fourth is the output's --
        // and `geometry::input_directions` is a `[Facing; 3]` that every
        // socket lookup indexes by declared input index: here for a merge's
        // branch repeaters, and in `attach_offset` and `cells` for every gate.
        // `place_nor_gate` and `place_merge_gate` each `assert!` the same
        // bound, but one stage later, so without this the index panic gets
        // there first with no gate name on it.
        //
        // Reachable, and only for a merge: `compile` admits a gate on
        // `is_realisable() && accepts_arity(len)`, and `Or(4).accepts_arity(4)`
        // is true because `Or`'s arity is whatever it was declared with.
        // `expand`'s merge path then never consults the library, so nothing
        // between here and there objects. A `Nor(4)` is stopped earlier --
        // `expand` asks `library.choose` for an entry and `default_library`
        // registers NOR arities 1..=3 only.
        let faces = geometry::input_directions(CellFacing::NORTH).len();
        if gate.inputs.len() > faces {
            return Err(format!(
                "gate `{}` declares {} inputs, and a gate cell has only {faces} input faces",
                gate.output,
                gate.inputs.len()
            ));
        }

        // A pin is where the body *is*, not merely a flag on it.
        let fixed = pinned.get(&gate.output);
        let at = fixed.unwrap_or(start[gate_index]);
        let position = [at.x as f64, at.y as f64, at.z as f64];
        let is_pinned = fixed.is_some();

        // The body that carries this gate's anchor: a merge's junction, or the
        // single torch its library entry instantiated.
        let anchor = if gate.is_merge() {
            bodies.push(Body {
                what: BodyKind::Junction { gate: gate_index },
                position,
                inputs: gate.inputs.clone(),
                output: Some(gate.output.clone()),
                facing: CellFacing::NORTH,
                pinned: is_pinned,
            });
            bodies.len() - 1
        } else {
            let node = *graph.gate_nodes[gate_index]
                .first()
                .ok_or_else(|| format!("gate `{}` instantiated no primitive", gate.output))?;
            let kind = graph.nodes[node].primitive;
            if physical::variants(kind).is_empty() {
                return Err(format!(
                    "gate `{}` needs a `{kind:?}`, which has no physical variants",
                    gate.output
                ));
            }
            bodies.push(Body {
                what: BodyKind::Primitive { node, kind },
                position,
                inputs: gate.inputs.clone(),
                output: Some(gate.output.clone()),
                facing: CellFacing::NORTH,
                pinned: is_pinned,
            });
            bodies.len() - 1
        };
        nodes[gate_index].push(anchor);
        anchor_body[gate_index] = anchor;

        // An isolated merge's branch repeaters, welded into the sockets the
        // router terminates them in.
        if gate.is_merge() {
            for &node in &graph.gate_nodes[gate_index] {
                let Provenance::Gate {
                    role: TemplateNode::IsolatingRepeater(input_index),
                    ..
                } = graph.nodes[node].provenance
                else {
                    continue;
                };
                let kind = graph.nodes[node].primitive;
                if physical::variants(kind).is_empty() {
                    return Err(format!(
                        "gate `{}`'s branch {input_index} needs a `{kind:?}`, which has no physical variants",
                        gate.output
                    ));
                }
                let direction = geometry::input_directions(CellFacing::NORTH)[input_index];
                let socket = Position::new(at.x, at.y, at.z).offset(direction);
                bodies.push(Body {
                    what: BodyKind::Primitive { node, kind },
                    position: [socket.x as f64, socket.y as f64, socket.z as f64],
                    // Its branch's net, and no output of its own: it drives
                    // into the junction, not out of the gate.
                    inputs: vec![gate.inputs[input_index].clone()],
                    output: None,
                    facing: CellFacing::NORTH,
                    pinned: is_pinned,
                });
                let repeater = bodies.len() - 1;
                nodes[gate_index].push(repeater);
                welds.push(Weld::AtSocket {
                    repeater,
                    junction: anchor,
                    input_index,
                });
            }
        }
    }

    for (input_index, name) in netlist.inputs.iter().enumerate() {
        let node = graph.nodes.len();
        let node = (0..node)
            .find(|&candidate| {
                matches!(&graph.nodes[candidate].provenance,
                    Provenance::PrimaryInput { name: declared } if declared == name)
            })
            .ok_or_else(|| format!("declared input `{name}` has no lever"))?;
        let candidate_node = netlist.gates.len() + input_index;
        let fixed = pinned.get(name);
        let at = fixed.unwrap_or(start[candidate_node]);
        let kind = graph.nodes[node].primitive;
        if physical::variants(kind).is_empty() {
            return Err(format!(
                "declared input `{name}` needs a `{kind:?}`, which has no physical variants"
            ));
        }
        bodies.push(Body {
            what: BodyKind::Primitive { node, kind },
            position: [at.x as f64, at.y as f64, at.z as f64],
            // A lever drives its own name and reads nothing, which is what
            // gives it a gate cell's shape with no sockets.
            inputs: Vec::new(),
            output: Some(name.clone()),
            facing: CellFacing::NORTH,
            pinned: fixed.is_some(),
        });
        nodes[candidate_node].push(bodies.len() - 1);
        anchor_body[candidate_node] = bodies.len() - 1;
    }

    let pulls = signal_pulls(netlist, &anchor_body, &welds);

    Ok(BodyGraph {
        bodies,
        pulls,
        welds,
        nodes,
        anchor_body,
    })
}

/// One pull per declared gate input: from the producer's outgoing pin to the
/// consumer's socket for that branch.
///
/// A declared output's lamp gets none. `emit_primitives` hangs it under its
/// producer's pin and `PlanCandidate` has no anchor for it, so its position is
/// not something relaxation chooses.
fn signal_pulls(netlist: &Netlist, anchor_body: &[usize], welds: &[Weld]) -> Vec<Pull> {
    let mut producer_node = std::collections::BTreeMap::new();
    for (index, gate) in netlist.gates.iter().enumerate() {
        producer_node.insert(gate.output.as_str(), index);
    }
    for (index, name) in netlist.inputs.iter().enumerate() {
        producer_node.insert(name.as_str(), netlist.gates.len() + index);
    }

    let mut pulls = Vec::new();
    for (gate_index, gate) in netlist.gates.iter().enumerate() {
        for (input_index, signal) in gate.inputs.iter().enumerate() {
            let Some(&producer) = producer_node.get(signal.as_str()) else {
                continue;
            };
            let from = (anchor_body[producer], Attach::Pin);

            // A branch with a welded repeater pulls on the repeater rather
            // than on the junction; the weld, not a spring, is what puts the
            // repeater in the socket.
            //
            // Not its rear, despite the port's name: `physical.rs` declares
            // every repeater port at `ORIGIN` and distinguishes them by
            // `direction`, which `attach_offset` drops -- because a repeater
            // occupies one cell. So this changes which body absorbs the force,
            // not where on that body the force lands.
            let welded = welds.iter().find_map(|weld| match weld {
                Weld::AtSocket {
                    repeater,
                    junction,
                    input_index: branch,
                } if *junction == anchor_body[gate_index] && *branch == input_index => {
                    Some(*repeater)
                }
                _ => None,
            });
            let to = match welded {
                Some(repeater) => (repeater, Attach::Port(PortKind::RepeaterRear)),
                None => (anchor_body[gate_index], Attach::Socket(input_index)),
            };

            pulls.push(Pull {
                from,
                to,
                stiffness: SIGNAL_STIFFNESS,
            });
        }
    }
    pulls
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::primitive_graph::expand;
    use crate::compile::topology::Library;
    use crate::compile::{Gate, Netlist};

    fn nor(output: &str, inputs: &[&str]) -> Gate {
        Gate::nor(output, inputs)
    }

    fn merge(output: &str, inputs: &[&str]) -> Gate {
        Gate::merge(output, inputs)
    }

    fn built(netlist: &Netlist) -> BodyGraph {
        let graph = expand(netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; netlist.gates.len() + netlist.inputs.len()];
        build(netlist, &graph, &start, &PortPlacements::default()).expect("builds")
    }

    /// A bare merge places nothing, so the primitive graph wires its consumer
    /// straight to its producer. The spring network must not: the junction is
    /// a real cell in a real place, and springs that skip it place the two
    /// sides on top of it.
    #[test]
    fn a_bare_merge_gets_a_body_and_the_pulls_go_through_it() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into()],
            gates: vec![
                nor("na", &["a"]),
                nor("nb", &["b"]),
                Gate::merge("m", &["na", "nb"]),
                nor("out", &["m"]),
            ],
        };
        let graph = built(&netlist);

        let junction = graph
            .bodies
            .iter()
            .position(|body| matches!(body.what, BodyKind::Junction { gate: 2 }))
            .expect("the merge has a junction body");

        assert!(
            graph.pulls.iter().any(|pull| pull.from.0 == junction),
            "nothing leaves the junction, so its consumer was wired past it"
        );
        assert_eq!(
            graph.pulls.iter().filter(|pull| pull.to.0 == junction).count(),
            2,
            "both branches must arrive at the junction"
        );
    }

    /// An isolated branch's repeater is a free body everywhere except where it
    /// actually goes. `world_partition::resolve_node_position` already says it
    /// is in the junction's socket; a weld is that statement, made a
    /// constraint.
    #[test]
    fn an_isolated_branch_welds_its_repeater_into_the_junctions_socket() {
        // `nb` feeds both the merge and `spy`, so the merge's branch on it is
        // shared rather than bare, and `expand` gives it a repeater.
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into(), "spy".into()],
            gates: vec![
                nor("na", &["a"]),
                nor("nb", &["b"]),
                Gate::merge("m", &["na", "nb"]),
                nor("out", &["m"]),
                nor("spy", &["nb"]),
            ],
        };
        let graph = built(&netlist);

        let junction = graph
            .bodies
            .iter()
            .position(|body| matches!(body.what, BodyKind::Junction { gate: 2 }))
            .expect("the merge has a junction body");
        let weld = graph
            .welds
            .iter()
            .find(|weld| matches!(weld, Weld::AtSocket { junction: j, .. } if *j == junction))
            .expect("the isolated branch is welded");
        let Weld::AtSocket { repeater, input_index, .. } = *weld else {
            unreachable!("matched AtSocket above")
        };

        assert_eq!(input_index, 1, "`nb` is the merge's second declared input");
        assert!(
            matches!(
                graph.bodies[repeater].what,
                BodyKind::Primitive { kind: Primitive::Repeater, .. }
            ),
            "a weld must hold a repeater"
        );
    }

    /// A declared output's lamp hangs under its producer's pin -- that is what
    /// `emit_primitives` does and `PlanCandidate` has no anchor for it. A body
    /// with no position to choose is not a body.
    #[test]
    fn a_declared_outputs_lamp_is_not_a_body() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let graph = built(&netlist);

        assert!(
            !graph.bodies.iter().any(|body| matches!(
                body.what,
                BodyKind::Primitive { kind: Primitive::Lamp, .. }
            )),
            "a lamp's position is its producer's, not its own"
        );
        assert_eq!(
            graph.bodies.len(),
            2,
            "one torch and one lever, and nothing else"
        );
    }

    /// Every node `PlanCandidate` expects has a body to be the anchor for --
    /// gates first, then primary inputs, which is the order `emit_primitives`
    /// reads positionally.
    #[test]
    fn every_candidate_node_has_an_anchor_body() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("na", &["a"]), nor("out", &["na", "b"])],
        };
        let graph = built(&netlist);

        assert_eq!(graph.anchor_body.len(), netlist.gates.len() + netlist.inputs.len());
        assert_eq!(graph.nodes.len(), graph.anchor_body.len());
        for (node, &body) in graph.anchor_body.iter().enumerate() {
            assert!(
                graph.nodes[node].contains(&body),
                "node {node}'s anchor body is not one of its own bodies"
            );
        }
    }

    /// A pinned port takes no force. Recorded here rather than discovered in
    /// the solve, because the solve's matrix is built by striking pinned
    /// bodies out of it.
    #[test]
    fn a_pinned_port_is_a_pinned_body() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let mut placements = PortPlacements::default();
        placements.pin("a", Anchor { x: 40, y: 1, z: 9 });

        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 2];
        let built = build(&netlist, &graph, &start, &placements).expect("builds");

        let lever = built.anchor_body[1];
        assert!(built.bodies[lever].pinned, "a pinned input must be a pinned body");
        assert_eq!(built.bodies[lever].position, [40.0, 1.0, 9.0]);
        assert!(!built.bodies[built.anchor_body[0]].pinned, "nothing pinned the gate");
    }

    /// A lever's pin is one hop out, which is what `place_primary_input`
    /// writes. Two is a NOR's answer, and only because its torch stands in the
    /// first hop; nothing stands in a lever's.
    ///
    /// Tested because an earlier draft keyed the hop count off `BodyKind`,
    /// where a lever and a NOR's torch are the same variant -- so every spring
    /// leaving a primary input attached one cell past the pin that exists, and
    /// no test looked.
    #[test]
    fn a_levers_pin_is_one_hop_out() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let graph = built(&netlist);
        let lever = &graph.bodies[graph.anchor_body[1]];

        assert_eq!(attach_offset(Attach::Pin, lever), [0.0, 0.0, -1.0]);
        assert_eq!(
            cells(lever).len(),
            4,
            "a lever is its own cell and its pin, and `place_primary_input` \
             floors both"
        );
    }

    /// A lever is a power source, and its own block is that source. Marking it
    /// inert would let a foreign net run flush against it -- which is the
    /// 2026-08-12 failure exactly, one body over.
    #[test]
    fn a_levers_own_cell_is_on_the_net_it_drives() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let graph = built(&netlist);
        let lever = &graph.bodies[graph.anchor_body[1]];

        let origin = cells(lever)
            .into_iter()
            .find(|cell| cell.offset == (0, 0, 0))
            .expect("a lever occupies its own cell");
        assert_eq!(origin.carries, vec!["a".to_string()], "a lever is not inert");
    }

    /// The rule the 2026-08-12 full adder broke, tested on the body it broke
    /// it on. That adder passed all four physical invariants and computed the
    /// wrong sums, because a foreign net was free to run against a support the
    /// code treated as inert.
    ///
    /// And the support is on *every* input net, not the first: a two-input
    /// NOR's second socket shares a net with it, so separation must not push
    /// them apart.
    #[test]
    fn a_nors_support_conducts_on_every_one_of_its_input_nets() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a", "b"])],
        };
        let graph = built(&netlist);
        let gate = &graph.bodies[graph.anchor_body[0]];
        let cells = cells(gate);

        let support = cells
            .iter()
            .find(|cell| cell.offset == (0, 0, 0))
            .expect("a NOR occupies its support");
        assert_eq!(
            support.carries,
            vec!["a".to_string(), "b".to_string()],
            "the support is the sink of both branches"
        );
    }

    /// A NOR's pin is two hops out because its torch stands in the first, and
    /// both cells are on the net it drives. An earlier draft keyed the hop
    /// count off `BodyKind`, which got this right and a lever's wrong.
    #[test]
    fn a_nors_torch_and_pin_are_two_hops_out_on_the_net_it_drives() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["out".into()],
            gates: vec![nor("out", &["a"])],
        };
        let graph = built(&netlist);
        let gate = &graph.bodies[graph.anchor_body[0]];
        assert_eq!(pin_hops(gate), 2, "a torch stands in the first hop");

        let out = geometry::output_direction(gate.facing);
        let one = Position::new(0, 0, 0).offset(out);
        let two = one.offset(out);
        for step in [one, two] {
            let cell = cells(gate)
                .into_iter()
                .find(|cell| cell.offset == (step.x, step.y, step.z))
                .unwrap_or_else(|| panic!("a NOR occupies {step:?}"));
            assert_eq!(cell.carries, vec!["out".to_string()]);
        }

        let pin = attach_offset(Attach::Pin, gate);
        assert_eq!(pin, [two.x as f64, two.y as f64, two.z as f64]);
    }

    /// A junction's floor is inert -- `place_merge_gate` writes it, and
    /// nothing has to keep a net clear of it.
    #[test]
    fn a_junctions_floor_is_inert() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["m".into()],
            gates: vec![merge("m", &["a", "b"])],
        };
        let graph = built(&netlist);
        let junction = &graph.bodies[graph.anchor_body[0]];
        assert!(
            matches!(junction.what, BodyKind::Junction { .. }),
            "a merge is placed by a junction"
        );

        let floor = cells(junction)
            .into_iter()
            .find(|cell| cell.offset == (0, -1, 0))
            .expect("place_merge_gate floors its junction");
        assert!(floor.carries.is_empty(), "a floor keeps nothing out");
    }

    /// A gate cell has three input faces, and a merge is the one gate that can
    /// reach `build` asking for a fourth: `compile` admits it on
    /// `Or(4).accepts_arity(4)` and `expand`'s merge path never consults the
    /// library. Refused with a sentence, rather than panicking on a
    /// `[Facing; 3]` one stage before `place_merge_gate`'s own `assert!`.
    #[test]
    fn a_merge_wider_than_a_gate_cell_is_refused_rather_than_indexed() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            outputs: vec!["m".into()],
            gates: vec![merge("m", &["a", "b", "c", "d"])],
        };
        // `expand` really does let this through -- the refusal below is
        // `build`'s to make, not a restatement of one already made.
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; netlist.gates.len() + netlist.inputs.len()];

        let refusal = build(&netlist, &graph, &start, &PortPlacements::default())
            .expect_err("four inputs do not fit on three faces");
        assert!(
            refusal.contains('m') && refusal.contains('4'),
            "the refusal names the gate and its arity, got {refusal:?}"
        );
    }

    /// The precondition the three `is_empty()` guards in `build` exist for:
    /// `physical.rs` really does have a primitive with no variants.
    ///
    /// Named for what it checks. It does not call `build` -- no netlist can
    /// reach those guards today, because nothing in the library instantiates a
    /// comparator -- so calling it a refusal test would claim coverage it does
    /// not have. What it pins is that the guards are not dead reasoning.
    #[test]
    fn physical_really_has_a_primitive_with_no_variants() {
        assert!(
            physical::variants(Primitive::Comparator).is_empty(),
            "this test is about the primitive that has none; if that changed, \
             pick another or delete this"
        );
    }
}

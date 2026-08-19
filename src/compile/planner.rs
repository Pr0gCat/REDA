use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::compile::primitive_graph::{self, reexpand_gate, EntrySelection, NodeId};
use crate::compile::topology::{Library, Primitive};
use crate::compile::{self, geometry, relax, CompiledCircuit, LegacyEmission, Netlist};
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::BlockState;
use crate::redstone::world::storage::World;

/// A fixed coordinate selected by the planner without referring to a world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Anchor {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// What a node becomes when a candidate is turned back into blocks.
///
/// A declared wire merge is the one node with no component of its own: it is
/// dust where two nets join, so it owns an anchor for spacing and routing
/// purposes while emitting no primitive.  Every other node names exactly one,
/// and that name is what selects a [`crate::compile::physical`] variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRealisation {
    Primitive(Primitive),
    WireMerge,
}

/// The node whose primitive is placed at an anchor.  Candidate coordinates
/// alone are ambiguous once two same-shaped primitives exist, so the node
/// identity travels with the candidate and is checked during realisation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimitiveNode {
    pub id: String,
    pub anchor: Anchor,
    /// The component to emit here.  Recorded by whoever chose the anchor;
    /// never re-derived from the node's name or from surrounding blocks.
    pub realisation: NodeRealisation,
    /// The subset of `footprint` that conducts: torches, dust, levers, lamps.
    /// Support and floor blocks are occupied but do not join a passing net.
    pub conductors: Vec<Anchor>,
    /// Every cell this node's realisation occupies, its anchor included.
    ///
    /// A NOR cell is a support block, a torch, its input sockets and its
    /// output pin -- not one cell.  Routing that only knows about the anchor
    /// will run a net straight through another gate's body and short the
    /// two together, which is what happens when this is empty.
    pub footprint: Vec<Anchor>,
    /// Whether somebody fixed this node's position, in which case no move may
    /// change it.
    pub pinned: bool,
    /// The cell this node's outgoing net starts from.
    ///
    /// Not the anchor: a gate's anchor is its support block, and its signal
    /// leaves from the pin one hop out from its torch. Routing from the
    /// anchor lays dust on the support and unsupports the torch.
    pub output_pin: Option<Anchor>,
}

impl PrimitiveNode {
    /// The cells to keep other nets out of: the recorded footprint, or the
    /// anchor alone for a node whose footprint nobody recorded.
    /// Where this node's outgoing net begins: its recorded pin, or its
    /// anchor for a node whose pin nobody recorded.
    pub fn source(&self) -> Anchor {
        self.output_pin.unwrap_or(self.anchor)
    }

    /// Whether `cell` conducts, for a cell this node occupies.
    pub fn occupancy_of(&self, cell: Anchor) -> Occupancy {
        if self.conductors.contains(&cell) {
            Occupancy::GateConductor
        } else {
            Occupancy::Solid
        }
    }

    pub fn occupied(&self) -> &[Anchor] {
        if self.footprint.is_empty() {
            std::slice::from_ref(&self.anchor)
        } else {
            &self.footprint
        }
    }
}

/// One declared sink of a route, recorded directly instead of inferred from
/// the flattened terminal order.  A fanout route can therefore be verified
/// even when its physical branches are emitted in a different order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSink {
    pub gate: String,
    pub input_index: usize,
    pub anchor: Anchor,
}

/// The physical terminal selected for one declared route sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteTerminal {
    pub sink: RouteSink,
    pub kind: RouteTerminalKind,
    /// Repeaters between this route's source and this sink.
    ///
    /// Counted by whoever laid the branch, at the moment it decided to place
    /// each one. A finished world cannot answer this: a route's cells are a
    /// graph, and walking it can cross between two runs of the same net that
    /// lie adjacent without being electrically sequential -- which is exactly
    /// how the walk this replaces lost three of full_adder's repeaters.
    pub repeaters: u64,
}

/// One routed connection in a candidate plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    id: String,
    anchors: Vec<Anchor>,
    owner: Option<String>,
    terminals: Vec<RouteTerminal>,
    /// What actually stands in each anchor, parallel to `anchors`.
    ///
    /// A candidate is meant to be a complete physical realisation, so the
    /// block in every routed cell is recorded by whoever chose it -- the
    /// legacy emitter for a seed, the planner's own router for a moved
    /// route -- rather than re-derived from the finished world.
    realisation: Vec<BlockState>,
    /// What each anchor stands on, parallel to `anchors`.  Recorded rather
    /// than derived: the emitter floors cells it then leaves empty, and a
    /// finished world cannot say which stone was laid for which reason.
    floors: Vec<BlockState>,
}

/// The physical component selected at a route's final socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTerminalKind {
    RepeaterIntoSupport,
    DirectedDustIntoSupport,
    /// A private branch ending directly in a declared wire merge's dust.
    BareMergeDust,
    /// A private merge branch whose strength budget still needs a final
    /// repeater; it terminates at merge dust, never at a NOR support.
    BareMergeRepeater,
}

/// The conservative choice for an ordinary route's final cell.
///
/// A dust terminal is valid only when the route proves a straight, live and
/// isolated approach into the support.  The physical emitter performs the
/// simulator-backed check as well; this planner-level record prevents a
/// local move from assuming that an old terminal decision remains valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalStyle {
    DirectedDustIntoSupport,
    RepeaterIntoSupport,
}

impl From<TerminalStyle> for RouteTerminalKind {
    fn from(style: TerminalStyle) -> Self {
        match style {
            TerminalStyle::DirectedDustIntoSupport => Self::DirectedDustIntoSupport,
            TerminalStyle::RepeaterIntoSupport => Self::RepeaterIntoSupport,
        }
    }
}

/// The three cells which establish whether dust can directly power a support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalApproach {
    pub predecessor: Anchor,
    pub terminal: Anchor,
    pub support: Anchor,
    pub predecessor_strength: u8,
    pub isolation_proven: bool,
}

impl TerminalApproach {
    pub fn new(
        predecessor: Anchor,
        terminal: Anchor,
        support: Anchor,
        predecessor_strength: u8,
        isolation_proven: bool,
    ) -> Self {
        Self {
            predecessor,
            terminal,
            support,
            predecessor_strength,
            isolation_proven,
        }
    }
}

/// Choose dust only for a fully proven directed terminal.
pub fn terminal_style(approach: &TerminalApproach) -> TerminalStyle {
    let incoming = unit_horizontal_direction(approach.predecessor, approach.terminal);
    let outgoing = unit_horizontal_direction(approach.terminal, approach.support);
    if approach.predecessor_strength > 1
        && approach.isolation_proven
        && incoming.is_some()
        && incoming == outgoing
    {
        TerminalStyle::DirectedDustIntoSupport
    } else {
        TerminalStyle::RepeaterIntoSupport
    }
}

impl Route {
    /// Construct immutable route metadata for a candidate or unit test.
    pub fn new(id: impl Into<String>, anchors: Vec<Anchor>) -> Self {
        let mut distinct_anchors = Vec::with_capacity(anchors.len());
        for anchor in anchors {
            if distinct_anchors.last() != Some(&anchor) {
                distinct_anchors.push(anchor);
            }
        }

        Self {
            id: id.into(),
            anchors: distinct_anchors,
            owner: None,
            terminals: Vec::new(),
            realisation: Vec::new(),
            floors: Vec::new(),
        }
    }

    pub(crate) fn from_legacy(
        id: String,
        anchors: Vec<Anchor>,
        terminals: Vec<RouteTerminal>,
        realisation: Vec<BlockState>,
        floors: Vec<BlockState>,
    ) -> Self {
        let mut route = Self::new(id.clone(), anchors);
        route.owner = Some(id);
        route.terminals = terminals;
        route.realisation = realisation;
        route.floors = floors;
        route
    }

    /// A route with declared anchors and terminals but no realisation --
    /// what a fixture, or a freshly rerouted edge, actually looks like.
    #[cfg(test)]
    pub(crate) fn unrealised(
        id: String,
        anchors: Vec<Anchor>,
        terminals: Vec<RouteTerminal>,
    ) -> Self {
        Self::from_legacy(id, anchors, terminals, Vec::new(), Vec::new())
    }

    /// The block this route puts in each of its anchors, in anchor order.
    ///
    /// Empty until something decides: a route that has been moved but not
    /// re-realised has anchors and no blocks, and emission refuses it rather
    /// than inventing dust.
    pub fn realisation(&self) -> &[BlockState] {
        &self.realisation
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    /// The source net this route belongs to, if it came from legacy emission.
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    /// The terminal decisions emitted for this route's sinks.
    pub fn terminal_kinds(&self) -> Vec<RouteTerminalKind> {
        self.terminals
            .iter()
            .map(|terminal| terminal.kind)
            .collect()
    }

    /// The sink identity and physical terminal for every fanout branch.
    pub fn terminals(&self) -> &[RouteTerminal] {
        &self.terminals
    }
}

/// Immutable planner input.  It deliberately contains no [`World`]-backed
/// state: legacy placement remains outside this candidate model for now.
#[derive(Debug, Clone)]
pub struct PlanCandidate {
    anchors: Vec<Anchor>,
    primitive_nodes: Vec<PrimitiveNode>,
    routes: Vec<Route>,
    variant_indices: Vec<u8>,
    topology_entries: BTreeMap<usize, usize>,
    legacy_emission: Option<LegacyEmission>,
}

impl PartialEq for PlanCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.anchors == other.anchors
            && self.primitive_nodes == other.primitive_nodes
            && self.routes == other.routes
            && self.variant_indices == other.variant_indices
            && self.topology_entries == other.topology_entries
    }
}

impl Eq for PlanCandidate {}

impl PlanCandidate {
    /// Construct a pure candidate from its selected anchors and route IDs.
    pub fn new(anchors: Vec<Anchor>, routes: Vec<Route>) -> Self {
        let variant_indices = vec![0; anchors.len()];
        Self {
            anchors,
            primitive_nodes: Vec::new(),
            routes,
            variant_indices,
            topology_entries: BTreeMap::new(),
            legacy_emission: None,
        }
    }

    /// Construct a candidate with the primitive identities required by local
    /// placement moves.  `primitive` is a [`NodeId`] into this ordered list.
    pub fn with_primitive_nodes(
        anchors: Vec<Anchor>,
        primitive_nodes: Vec<PrimitiveNode>,
        routes: Vec<Route>,
    ) -> Self {
        let variant_indices = vec![0; anchors.len()];
        Self {
            anchors,
            primitive_nodes,
            routes,
            variant_indices,
            topology_entries: BTreeMap::new(),
            legacy_emission: None,
        }
    }

    /// Construct a candidate whose nodes are not all built facing north.
    ///
    /// `variant_indices` has existed since the candidate model landed and
    /// every constructor has filled it with zeroes. This is the one that puts
    /// something in it.
    ///
    /// One thing already reads that field: `gate_efforts` copies it into the
    /// `GateEffort::variant` diagnostic. So a candidate built here reports a
    /// non-zero variant for every gate relaxation turned, where before it
    /// always reported zero. Nothing scores or branches on it, and
    /// `gate_effort_reports_route_terminal_and_variant_costs_by_gate` keeps
    /// passing because its fixture still builds through `with_primitive_nodes`.
    pub fn with_facings(
        anchors: Vec<Anchor>,
        primitive_nodes: Vec<PrimitiveNode>,
        routes: Vec<Route>,
        facings: Vec<geometry::CellFacing>,
    ) -> Self {
        assert_eq!(facings.len(), anchors.len(), "one facing per anchor");
        let mut candidate = PlanCandidate::with_primitive_nodes(anchors, primitive_nodes, routes);
        candidate.variant_indices = facings.iter().map(|facing| facing.index()).collect();
        candidate
    }

    pub(crate) fn from_legacy(
        anchors: Vec<Anchor>,
        primitive_nodes: Vec<PrimitiveNode>,
        routes: Vec<Route>,
        legacy_emission: LegacyEmission,
    ) -> Self {
        let variant_indices = vec![0; anchors.len()];
        Self {
            anchors,
            primitive_nodes,
            routes,
            variant_indices,
            topology_entries: BTreeMap::new(),
            legacy_emission: Some(legacy_emission),
        }
    }

    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    /// Where a named primary input or declared output ended up.
    pub fn port_anchor(&self, port: &str) -> Option<Anchor> {
        self.primitive_nodes
            .iter()
            .find(|node| node.id == format!("input:{port}") || node.id == format!("gate:{port}"))
            .map(|node| node.anchor)
    }

    /// The explicit node identity associated with every primitive anchor.
    pub fn primitive_nodes(&self) -> &[PrimitiveNode] {
        &self.primitive_nodes
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    /// Attach deterministic library selections to a synthetic candidate.
    /// Production seeds start with the library default (entry zero).
    pub fn with_topology_entries(mut candidate: Self, entries: BTreeMap<usize, usize>) -> Self {
        candidate.topology_entries = entries;
        candidate
    }

    /// The selected entry for `gate`, or the library default entry zero.
    pub fn selected_entry(&self, gate: usize) -> usize {
        self.topology_entries.get(&gate).copied().unwrap_or(0)
    }

    /// Which way node `node`'s cell is built.
    ///
    /// Panics on an unknown node or an index no facing has, deliberately, and
    /// deliberately *not* the lenient `unwrap_or_default()` this was first
    /// written as. `variant_indices` is a bare `Vec<u8>` with nothing at the
    /// type level stopping a 4 from being written into it, and the lenient
    /// form turns every such mistake into north -- silently, and identically
    /// to a correct north. A gate would then be built, routed and *verified*
    /// facing a way nobody chose, with no failure anywhere to trace back. Its
    /// sibling `candidate.anchors[node]`, read one line away at both call
    /// sites, panics on the same out-of-range node; a facing that shrugged
    /// where a position screams is the asymmetry that hides the bug.
    ///
    /// Please do not "fix" this back to a default.
    pub fn facing_of(&self, node: usize) -> geometry::CellFacing {
        let &index = self.variant_indices.get(node).unwrap_or_else(|| {
            panic!(
                "no facing recorded for node {node}: this candidate has {} node(s)",
                self.variant_indices.len()
            )
        });
        geometry::CellFacing::from_index(index).unwrap_or_else(|| {
            panic!(
                "node {node} has variant index {index}, which is not one of the four \
                 horizontal facings (0..=3)"
            )
        })
    }

    /// Report measured local routing, terminal, and variant effort for every
    /// gate represented by this candidate.
    pub fn gate_effort(&self) -> Vec<GateEffort> {
        gate_efforts(self)
    }

    pub fn cost(&self) -> CostBreakdown {
        CostBreakdown::from_candidate(self)
    }

    /// Score this candidate against itself, which is the normalised seed score.
    pub fn score(&self, weights: &PlannerWeights) -> Result<NormalisedScore, ScoreError> {
        let cost = self.cost();
        cost.normalised_against(&cost, weights)
    }

    /// Score this candidate against immutable seed metadata.
    pub fn score_against(
        &self,
        seed: &PlanCandidate,
        weights: &PlannerWeights,
        effort: PlannerEffort,
    ) -> Result<CandidateScore, ScoreError> {
        self.score_against_at(seed, weights, effort, 0)
    }

    fn score_against_at(
        &self,
        seed: &PlanCandidate,
        weights: &PlannerWeights,
        effort: PlannerEffort,
        original_index: usize,
    ) -> Result<CandidateScore, ScoreError> {
        let cost = self.cost();
        let normalised = cost.normalised_against(&seed.cost(), weights)?;

        Ok(CandidateScore {
            cost,
            normalised,
            effort,
            order: CandidateOrder {
                normalised,
                cost,
                original_index,
            },
        })
    }
}

/// A legacy compiler output cannot be converted into a legal planner seed.
///
/// No `Eq`: [`PlannerError::Relaxation`] carries a `Violation::shortfall`,
/// which is an `f64`, and `f64: Eq` does not hold. Nothing wants it --
/// `PlannerError` derives neither `Hash` nor `Ord`, so it is never a map key
/// nor sorted, and every comparison in this module's tests is `assert_eq!` or
/// `matches!`, which need only `PartialEq` and `Debug`.
#[derive(Debug, Clone, PartialEq)]
pub enum PlannerError {
    LegacyMetadataUnavailable,
    NetlistDoesNotMatchCompiledOutput,
    UnknownPrimitive(NodeId),
    AnchorOccupied(Anchor),
    NoLocalRoute { from: Anchor, to: Anchor },
    /// Somebody fixed this port's position, so no move may change it.
    PortIsPinned(String),
    /// A node's recorded realisation cannot be turned into blocks -- either
    /// the primitive has no emitter yet, or it contradicts the gate it is
    /// supposed to be realising.
    UnrealisableNode { id: String, reason: String },
    PhysicalInvariant(compile::CompileError),
    /// The relaxation could not produce a placement.
    Relaxation(relax::RelaxError),
}

impl std::fmt::Display for PlannerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacyMetadataUnavailable => {
                write!(f, "compiled circuit has no legacy emission metadata")
            }
            Self::NetlistDoesNotMatchCompiledOutput => {
                write!(f, "netlist does not match the legacy compiler output")
            }
            Self::UnknownPrimitive(primitive) => write!(f, "unknown primitive node {primitive}"),
            Self::AnchorOccupied(anchor) => write!(
                f,
                "cannot move a primitive onto occupied anchor ({}, {}, {})",
                anchor.x, anchor.y, anchor.z
            ),
            Self::NoLocalRoute { from, to } => write!(
                f,
                "no safe local route from ({}, {}, {}) to ({}, {}, {})",
                from.x, from.y, from.z, to.x, to.y, to.z
            ),
            Self::PortIsPinned(port) => write!(f, "port {port} is pinned and cannot move"),
            Self::UnrealisableNode { id, reason } => {
                write!(f, "cannot realise node {id}: {reason}")
            }
            Self::PhysicalInvariant(error) => error.fmt(f),
            // Forwarded rather than wrapped: `RelaxError` already says which
            // bodies and by how much, and a second sentence around it would
            // bury the only part anybody can act on.
            Self::Relaxation(error) => write!(f, "{error}"),
        }
    }
}

/// Where a realised candidate's externally visible points ended up.
///
/// The invariants are written against these, and so is anything that later
/// reads the circuit: they are produced by realisation rather than copied
/// from the legacy emitter's own bookkeeping.
#[derive(Debug, Default, Clone)]
pub struct CandidatePorts {
    pub input_positions: BTreeMap<String, (i32, i32, i32)>,
    pub output_positions: BTreeMap<String, (i32, i32, i32)>,
    pub gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
}

/// A candidate turned into blocks, with the ports that realisation chose.
#[derive(Debug, Clone)]
pub struct RealisedCandidate {
    pub world: World,
    pub ports: CandidatePorts,
}

/// Realise a whole candidate: its primitives, then its routes.
///
/// The result is a world built from nothing but the plan.  For a seed this
/// must reproduce the legacy emitter's world exactly; for any other candidate
/// it is that candidate's actual, checkable output rather than a promise.
pub fn emit_candidate(
    candidate: &PlanCandidate,
    netlist: &Netlist,
    size: (i32, i32, i32),
) -> Result<RealisedCandidate, PlannerError> {
    let mut realised = emit_primitives(candidate, netlist, size)?;
    emit_routes(&mut realised.world, candidate)?;
    Ok(realised)
}

/// Write every route's own cells, and the floor each one stands on.
///
/// A route with no recorded realisation is refused: the alternative is to
/// guess that an unrealised cell wants dust, which would silently produce a
/// different circuit from the one the planner scored.
fn emit_routes(world: &mut World, candidate: &PlanCandidate) -> Result<(), PlannerError> {
    for route in &candidate.routes {
        if route.realisation.len() != route.anchors.len()
            || route.floors.len() != route.anchors.len()
        {
            return Err(PlannerError::UnrealisableNode {
                id: route.id.clone(),
                reason: format!(
                    "{} anchor(s) but {} block(s) and {} floor(s)",
                    route.anchors.len(),
                    route.realisation.len(),
                    route.floors.len()
                ),
            });
        }

        for ((anchor, block), floor) in route
            .anchors
            .iter()
            .zip(route.realisation.iter())
            .zip(route.floors.iter())
        {
            world.set(anchor.x, anchor.y - 1, anchor.z, floor.clone());
            world.set(anchor.x, anchor.y, anchor.z, block.clone());
        }
    }

    Ok(())
}

/// Put every primitive a candidate places back into a world.
///
/// This is the half of realisation that needs nothing but the candidate: each
/// node's own cell, written at the anchor the plan chose, rather than
/// re-derived from a floorplan the plan no longer owns.  Routes are not
/// emitted here -- they need the strength model, and they come next.
///
/// Node order is the candidate's own [`NodeId`] order, which is the netlist's
/// gates followed by its primary inputs.  Rather than trust that convention
/// silently, every gate node's recorded realisation is checked against the
/// gate it claims to be: a merge that says "torch", or a NOR that says
/// "merge", is an error rather than a quietly different circuit.
pub fn emit_primitives(
    candidate: &PlanCandidate,
    netlist: &Netlist,
    size: (i32, i32, i32),
) -> Result<RealisedCandidate, PlannerError> {
    let expected_nodes = netlist.gates.len() + netlist.inputs.len();
    if candidate.primitive_nodes.len() != expected_nodes {
        return Err(PlannerError::NetlistDoesNotMatchCompiledOutput);
    }

    let mut world = World::new(size.0, size.1, size.2);
    let mut gate_pin: Vec<Position> = Vec::with_capacity(netlist.gates.len());
    let mut ports = CandidatePorts::default();
    for (index, node) in candidate.primitive_nodes.iter().enumerate() {
        // `anchors` is the store `try_move` and the cost model operate on;
        // `primitive_nodes` carries the same coordinate alongside identity.
        // Realising from one while the other disagrees would build a circuit
        // nobody scored, so a disagreement is an error rather than a choice.
        let anchor = *candidate
            .anchors
            .get(index)
            .ok_or_else(|| PlannerError::UnrealisableNode {
                id: node.id.clone(),
                reason: "no anchor for this node".to_string(),
            })?;
        if anchor != node.anchor {
            return Err(PlannerError::UnrealisableNode {
                id: node.id.clone(),
                reason: format!(
                    "anchor ({}, {}, {}) disagrees with node anchor ({}, {}, {})",
                    anchor.x, anchor.y, anchor.z, node.anchor.x, node.anchor.y, node.anchor.z
                ),
            });
        }
        let origin = (anchor.x, anchor.y, anchor.z);
        // Which way this node's cell was planned. Not north any more: since
        // Task 10 `plan_from_netlist` records a facing per node and this is
        // where it becomes blocks.
        let facing = candidate.facing_of(index);

        match netlist.gates.get(index) {
            Some(gate) => match (node.realisation, gate.is_merge()) {
                (NodeRealisation::WireMerge, true) => {
                    let cell =
                        compile::place_merge_gate(&mut world, origin, gate.inputs.len(), facing);
                    let (torch, pin) = output_pin(&mut world, anchor, &cell, facing);
                    ports
                        .gate_output_positions
                        .insert(gate.output.clone(), (torch.x, torch.y, torch.z));
                    gate_pin.push(pin);
                }
                (NodeRealisation::Primitive(Primitive::Torch), false) => {
                    let cell =
                        compile::place_nor_gate(&mut world, origin, gate.inputs.len(), facing);
                    let (torch, pin) = output_pin(&mut world, anchor, &cell, facing);
                    ports
                        .gate_output_positions
                        .insert(gate.output.clone(), (torch.x, torch.y, torch.z));
                    gate_pin.push(pin);
                }
                (realisation, _) => {
                    return Err(PlannerError::UnrealisableNode {
                        id: node.id.clone(),
                        reason: format!(
                            "{realisation:?} does not realise {:?}",
                            netlist.gates[index].kind
                        ),
                    })
                }
            },
            None => match node.realisation {
                NodeRealisation::Primitive(Primitive::Lever) => {
                    let home = Position::new(anchor.x, anchor.y, anchor.z);
                    let (lever, _) = compile::place_primary_input(&mut world, home, facing);
                    let name = &netlist.inputs[index - netlist.gates.len()];
                    ports
                        .input_positions
                        .insert(name.clone(), (lever.x, lever.y, lever.z));
                }
                realisation => {
                    return Err(PlannerError::UnrealisableNode {
                        id: node.id.clone(),
                        reason: format!("{realisation:?} does not realise a primary input"),
                    })
                }
            },
        }
    }

    // A declared output's lamp is not part of any gate cell and is not
    // claimed by a route: it hangs under the producing gate's own pin, which
    // is the one place nothing else can reach.
    for output in &netlist.outputs {
        let gate = netlist
            .gates
            .iter()
            .position(|gate| &gate.output == output)
            .ok_or_else(|| PlannerError::UnrealisableNode {
                id: output.clone(),
                reason: "declared output has no producing gate".to_string(),
            })?;
        let lamp = gate_pin[gate].down();
        world.set(lamp.x, lamp.y, lamp.z, compile::lamp());
        ports
            .output_positions
            .insert(output.clone(), (lamp.x, lamp.y, lamp.z));
    }

    Ok(RealisedCandidate { world, ports })
}

/// Write a gate's own output pin -- the dust one hop out from its torch --
/// and report where it landed.
///
/// The pin belongs to the gate, not to any route: a gate with no sinks still
/// has one, and a declared output's lamp hangs beneath it.
fn output_pin(
    world: &mut World,
    anchor: Anchor,
    cell: &compile::NorCell,
    facing: compile::geometry::CellFacing,
) -> (Position, Position) {
    let torch = Position::new(
        anchor.x + cell.output_offset.0,
        anchor.y + cell.output_offset.1,
        anchor.z + cell.output_offset.2,
    );
    let pin = torch.offset(compile::geometry::output_direction(facing));
    compile::ensure_floor(world, pin);
    world.set(pin.x, pin.y, pin.z, compile::dust());
    (torch, pin)
}

impl std::error::Error for PlannerError {}

/// Move one primitive and rebuild exactly the routes which touch it.
///
/// The seed may have come from the legacy router, but an incident route is
/// rebuilt from its current endpoint anchors only.  Its old intermediate
/// cells are deliberately never considered as candidates.  Non-incident
/// routes are cloned unchanged, so their byte representation is stable.
pub fn try_move(
    candidate: &PlanCandidate,
    primitive: NodeId,
    to: Anchor,
) -> Result<PlanCandidate, PlannerError> {
    let from = candidate
        .primitive_anchor(primitive)
        .ok_or(PlannerError::UnknownPrimitive(primitive))?;
    if candidate
        .primitive_nodes
        .get(primitive)
        .is_some_and(|node| node.pinned)
    {
        return Err(PlannerError::PortIsPinned(
            candidate.primitive_nodes[primitive].id.clone(),
        ));
    }
    let mut moved = candidate.clone();
    moved.legacy_emission = None;
    moved.set_primitive_anchor(primitive, to)?;

    let incident: Vec<bool> = candidate
        .routes
        .iter()
        .map(|route| candidate.route_is_incident(route, primitive))
        .collect();
    let mut reservation = candidate.live_reservation(&incident);
    let moved_owner = format!("primitive:{primitive}");
    // The whole primitive moves, not just the cell its anchor names: free
    // every cell it occupied and claim every cell it will occupy. Reserving
    // one cell of a NOR lets a rerouted net run through the rest of it.
    let old_cells: Vec<Anchor> = candidate
        .primitive_nodes
        .get(primitive)
        .map(|node| node.occupied().to_vec())
        .unwrap_or_else(|| vec![from]);
    for cell in &old_cells {
        if reservation.owner(cell) == Some(moved_owner.as_str()) {
            reservation.remove(cell);
        }
    }
    let delta = (to.x - from.x, to.y - from.y, to.z - from.z);
    let new_cells: Vec<Anchor> = old_cells
        .iter()
        .map(|cell| Anchor {
            x: cell.x + delta.0,
            y: cell.y + delta.1,
            z: cell.z + delta.2,
        })
        .collect();
    if new_cells.iter().any(|cell| reservation.is_taken(cell)) {
        return Err(PlannerError::AnchorOccupied(to));
    }
    let moved_node = candidate.primitive_nodes.get(primitive);
    for (cell, original) in new_cells.iter().zip(&old_cells) {
        let occupancy = moved_node.map_or(Occupancy::Solid, |node| node.occupancy_of(*original));
        reservation.insert(*cell, &moved_owner, occupancy);
    }

    for (route_index, route) in candidate.routes.iter().enumerate() {
        if !incident[route_index] {
            continue;
        }

        let owner = route.id.clone();
        let (source, terminals) = moved.route_endpoints(route_index, primitive, from, to);
        let mut rebuilt = route.clone();
        rebuilt.anchors.clear();
        rebuilt.realisation.clear();
        rebuilt.floors.clear();
        let mut branches = Vec::with_capacity(terminals.len());
        for (support, terminal) in terminals {
            let path = deterministic_astar(
                source,
                terminal,
                support,
                &owner,
                &reservation,
                &Prices::RipUp(&Congestion::default()),
            )
            .ok_or(
                PlannerError::NoLocalRoute {
                    from: source,
                    to: terminal,
                },
            )?;
            reserve_path(&mut reservation, &owner, &path);
            let laid = realise_branch(source, &path);
            // A fanout's branches share a trunk. The first branch to reach a
            // cell lays it, exactly as the legacy emitter's `claim` records
            // the first net to conduct through one; appending it again would
            // give one cell two blocks and two owners.
            for ((anchor, block), floor) in
                path.iter().zip(laid.blocks).zip(laid.floors)
            {
                if rebuilt.anchors.contains(anchor) {
                    continue;
                }
                rebuilt.anchors.push(*anchor);
                rebuilt.realisation.push(block);
                rebuilt.floors.push(floor);
            }
            branches.push((path, support, laid.strength_before_terminal, laid.repeaters));
        }

        for (terminal, (path, support, strength_before_terminal, branch_repeaters)) in
            rebuilt.terminals.iter_mut().zip(branches)
        {
            // The branch was just re-laid, so its repeater count is a fact
            // again. Keeping the seed's would leave the primary cost term
            // blind to exactly the changes the optimiser makes.
            terminal.repeaters = branch_repeaters;
            if matches!(
                terminal.kind,
                RouteTerminalKind::BareMergeDust | RouteTerminalKind::BareMergeRepeater
            ) {
                continue;
            }
            let Some(&predecessor) = path.get(path.len().saturating_sub(2)) else {
                terminal.kind = RouteTerminalKind::RepeaterIntoSupport;
                continue;
            };
            let terminal_anchor = *path.last().expect("A* paths always include their goal");
            let approach = TerminalApproach::new(
                predecessor,
                terminal_anchor,
                support,
                strength_before_terminal,
                terminal_is_isolated(&reservation, &owner, predecessor, terminal_anchor, support),
            );
            let style = terminal_style(&approach);
            terminal.kind = style.into();
            // The branch ends somewhere new, so the sink's recorded cell has
            // to move with it: everything downstream -- the reservation, the
            // terminal check, the invariants -- reads the terminal from here.
            terminal.sink.anchor = terminal_anchor;

            // And the block there has to be the one the style names. The
            // strength budget laid this cell before the style was chosen; a
            // plan that says repeater over dust is the same lie the legacy
            // emitter used to tell, and the terminal check catches it either
            // way, so make it true rather than let it be caught.
            if let Some(index) = rebuilt
                .anchors
                .iter()
                .position(|anchor| *anchor == terminal_anchor)
            {
                rebuilt.realisation[index] = match style {
                    TerminalStyle::RepeaterIntoSupport
                        if unit_horizontal_direction(predecessor, terminal_anchor).is_some() =>
                    {
                        compile::repeater(compile::direction_from(
                            Position::new(predecessor.x, predecessor.y, predecessor.z),
                            Position::new(terminal_anchor.x, terminal_anchor.y, terminal_anchor.z),
                        ))
                    }
                    _ => compile::dust(),
                };
            }
        }
        moved.routes[route_index] = rebuilt;
    }

    Ok(moved)
}

/// One rerouted branch, turned into the blocks that branch actually needs.
struct LaidBranch {
    blocks: Vec<BlockState>,
    floors: Vec<BlockState>,
    /// The signal strength arriving at the cell before the terminal -- read
    /// off the same repeater plan that produced `blocks`, not estimated from
    /// the path's length.
    strength_before_terminal: u8,
    /// Repeaters this branch lays between the source and its terminal.
    repeaters: u64,
    /// Whether the signal is still alive when it reaches the last cell.
    ///
    /// A refresh can only stand on a flat cell, and a path that climbs and
    /// drops repeatedly leaves nowhere to put one -- so the budget asks for a
    /// refresh, finds every candidate is a stair, and the run carries on
    /// decaying. The route is laid, connected and dead. Saying so here turns
    /// it into a routing failure, which the negotiation loop already knows how
    /// to answer: charge what pushed this path into the air and take a flatter
    /// one.
    carries: bool,
}

/// Lay dust along a rerouted branch, refreshing it with repeaters exactly
/// where the strength budget demands.
///
/// This is `compile::plan_bent_path`, the same budget the legacy router
/// spends -- a second implementation of dust decay would be a second thing to
/// be wrong about, and the planner already had one: the terminal choice used
/// to assume a strength of `16 - path length`, which is neither the real
/// maximum nor aware that a repeater resets it.
fn realise_branch(source: Anchor, cells: &[Anchor]) -> LaidBranch {
    realise_branch_from(
        source,
        crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH,
        cells,
    )
}

/// [`realise_branch`], continuing from a signal that has already travelled.
///
/// A fanout's branches share a trunk, and the trunk keeps the blocks the first
/// branch laid. A later branch that plans its refreshes from full strength
/// across its whole path is therefore planning refreshes it will not get: the
/// ones it wanted on the trunk are discarded, and its tail runs from wherever
/// the trunk actually left the signal. So it is told.
fn realise_branch_from(previous_cell: Anchor, incoming: u8, cells: &[Anchor]) -> LaidBranch {
    let source = previous_cell;
    let mut bends: BTreeSet<usize> = cells
        .windows(3)
        .enumerate()
        .filter(|(_, window)| direction(window[0], window[1]) != direction(window[1], window[2]))
        .map(|(index, _)| index + 1)
        .collect();
    // A repeater needs a flat cell to stand on and a horizontal facing, so a
    // staircase step can never hold one. `bend_indices` already means exactly
    // "no repeater here", so saying it there lets `plan_bent_path` put the
    // refresh somewhere it fits -- rather than placing one on a stair and
    // having realisation quietly downgrade it to dust and lose the refresh.
    let mut previous = source;
    for (index, cell) in cells.iter().enumerate() {
        if cell.y != previous.y {
            bends.insert(index);
        }
        previous = *cell;
    }

    // Reserve for the stairs. Every cell of a climb spends strength and none
    // of them can hold a repeater, so the refreshes have to be far enough
    // ahead to carry the run through them -- which is exactly what `reserve`
    // means to `plan_bent_path`. Pricing climbs in the search instead was
    // tried and moved them to where the signal could no longer afford them.
    let stairs = bends
        .iter()
        .filter(|&&index| {
            let before = if index == 0 { source } else { cells[index - 1] };
            cells[index].y != before.y
        })
        .count();
    let reserve = (stairs as i32).min(compile::MAX_DUST_RUN - 2);
    let (is_repeater, _) = compile::plan_bent_path(cells.len(), &bends, incoming, reserve);

    // A refresh immediately before every climb. A staircase spends one
    // strength per level and can hold no repeater anywhere along it, so a
    // climb entered on a tired signal arrives dead however short it is --
    // which is what left segment_a connected and unpowered. Entered at full
    // strength it is affordable, and this is the only cell that can make it
    // so: the last flat one before the stairs.
    let mut is_repeater = is_repeater;
    let mut previous = source;
    for (index, cell) in cells.iter().enumerate() {
        if cell.y != previous.y && index > 0 {
            let before = index - 1;
            if !bends.contains(&before) {
                is_repeater[before] = true;
            }
        }
        previous = *cell;
    }

    let mut blocks = Vec::with_capacity(cells.len());
    let mut previous = source;
    for (index, cell) in cells.iter().enumerate() {
        // A repeater needs a horizontal facing, so a cell reached by a step
        // in Y can only be dust -- that is what a dust staircase is. The
        // strength budget may have wanted a refresh here; if losing it
        // matters, `verify_signal_strength` says so rather than this guessing.
        let step = unit_horizontal_direction(previous, *cell);
        let block = match (is_repeater[index], step) {
            (true, Some(_)) => compile::repeater(compile::direction_from(
                Position::new(previous.x, previous.y, previous.z),
                Position::new(cell.x, cell.y, cell.z),
            )),
            _ => compile::dust(),
        };
        blocks.push(block);
        previous = *cell;
    }

    // Strength at the cell before the terminal: full again if that cell is a
    // repeater, otherwise the maximum less one per dust cell since the last
    // refresh.
    let strength_before_terminal = match cells.len().checked_sub(2) {
        None => incoming,
        Some(index) => {
            let last_refresh = (0..=index).rev().find(|&i| is_repeater[i]);
            match last_refresh {
                Some(refresh) => crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH
                    .saturating_sub((index - refresh) as u8),
                None => incoming.saturating_sub((index + 1) as u8),
            }
        }
    };

    // Walk the blocks that were actually laid, not the plan that asked for
    // them: a refresh the plan wanted and could not place is exactly the case
    // this has to catch.
    let mut carried = incoming;
    let mut carries = true;
    for block in &blocks {
        if block.kind == crate::redstone::world::block::BlockKind::Repeater {
            carried = crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
            continue;
        }
        carried = carried.saturating_sub(1);
        if carried == 0 {
            carries = false;
            break;
        }
    }

    LaidBranch {
        floors: vec![compile::stone(); blocks.len()],
        repeaters: blocks
            .iter()
            .filter(|block| block.kind == crate::redstone::world::block::BlockKind::Repeater)
            .count() as u64,
        blocks,
        strength_before_terminal,
        carries,
    }
}

impl PlanCandidate {
    fn primitive_anchor(&self, primitive: NodeId) -> Option<Anchor> {
        self.primitive_nodes
            .get(primitive)
            .map(|node| node.anchor)
            .or_else(|| self.anchors.get(primitive).copied())
    }

    fn set_primitive_anchor(
        &mut self,
        primitive: NodeId,
        anchor: Anchor,
    ) -> Result<(), PlannerError> {
        if let Some(node) = self.primitive_nodes.get_mut(primitive) {
            let delta = (
                anchor.x - node.anchor.x,
                anchor.y - node.anchor.y,
                anchor.z - node.anchor.z,
            );
            for cell in &mut node.footprint {
                cell.x += delta.0;
                cell.y += delta.1;
                cell.z += delta.2;
            }
            if let Some(pin) = node.output_pin.as_mut() {
                pin.x += delta.0;
                pin.y += delta.1;
                pin.z += delta.2;
            }
        }
        let Some(slot) = self.anchors.get_mut(primitive) else {
            return Err(PlannerError::UnknownPrimitive(primitive));
        };
        *slot = anchor;
        if let Some(node) = self.primitive_nodes.get_mut(primitive) {
            node.anchor = anchor;
        }
        Ok(())
    }


    fn route_is_incident(&self, route: &Route, primitive: NodeId) -> bool {
        let Some(node) = self.primitive_nodes.get(primitive) else {
            return false;
        };
        route.owner.as_deref() == Some(node.id.strip_prefix("input:").unwrap_or(&node.id))
            || route.owner.as_deref() == Some(node.id.strip_prefix("gate:").unwrap_or(&node.id))
            || route
                .terminals
                .iter()
                .any(|terminal| node.id == format!("gate:{}", terminal.sink.gate))
    }

    fn live_reservation(&self, incident: &[bool]) -> Reservation {
        // A primitive keeps other nets out of every cell it occupies, not
        // just the one its anchor names -- but only the cells that conduct
        // keep them out of the cells *beside* it.
        //
        // The nodes go in first, and that ordering is the whole of it.
        // `Reservation::insert` is `or_insert_with`, so the first writer of a
        // cell decides its occupancy and no later one can upgrade it. With the
        // anchor sweep below running first, every anchor a node declares
        // `Conductor` was already down as `Solid` -- measured: 11 of and4's 11
        // and 25 of full_adder's 25, every NOR support and every lever, against
        // 0 of each under `route_in_order`'s reservation, which never had the
        // pre-seed. An inert claim survives `owner` and dies at
        // `conductor_owner`, which is what `anchor_is_free_for`'s floor test and
        // `keep_out` both ask, so `try_move` was offered 20 cells in and4 and 26
        // in full_adder that the router itself refuses -- among them dust laid
        // directly beside a lit lever, reading 15.
        //
        // Latent rather than shipped: `optimise`/`try_move` is the only caller,
        // and `compile_planned` routes through `route_in_order`. It dates from
        // `6dfbe56`, the commit that introduced `Occupancy` and left this one
        // call site writing the old flat `Solid`.
        let mut reservation = reserve_primitives(&self.primitive_nodes);
        // Anything with an anchor but no node -- nothing builds one today, and
        // it stays because an unclaimed anchor is worse than an inert one.
        for (index, anchor) in self.anchors.iter().copied().enumerate() {
            reservation.insert(anchor, &format!("primitive:{index}"), Occupancy::Solid);
        }
        for (index, route) in self.routes.iter().enumerate() {
            if !incident[index] {
                reserve_path(&mut reservation, &route.id, &route.anchors);
            }
        }
        reservation
    }

    /// A gate's name to the candidate node index that holds its facing.
    ///
    /// `node_for_gate` answers the same question with an anchor, and every
    /// caller of it wants the anchor. This one exists because a [`RouteSink`]
    /// records a name and [`PlanCandidate::facing_of`] takes an index, and
    /// there is nothing on a route that carries the index itself.
    fn node_index_for_gate(&self, gate: &str) -> Option<usize> {
        self.primitive_nodes
            .iter()
            .position(|node| node.id == format!("gate:{gate}"))
    }

    /// The cell a declared sink's route has to arrive in: `support`'s socket
    /// for the declared input this sink feeds.
    ///
    /// The netlist's answer, not the geometry's. `terminal_socket` guesses one
    /// from the direction the route approached out of, which was right while
    /// every gate faced north and every socket was in a fixed place; with
    /// facings varying it names a different cell from the one `route_in_order`
    /// laid dust to and `equivalence` checks.
    ///
    /// `support` is a parameter rather than another `node_for_gate` call
    /// because [`PlanCandidate::route_endpoints`] has already remapped it for
    /// the primitive it is moving, and that remapping should exist once.
    fn declared_socket(&self, support: Anchor, sink: &RouteSink) -> Anchor {
        let facing = self
            .node_index_for_gate(&sink.gate)
            .map(|node| self.facing_of(node))
            .unwrap_or_default();
        step(
            support,
            compile::geometry::input_directions(facing)[sink.input_index],
        )
    }

    /// A route's source pin, and one `(support, socket)` pair per branch.
    ///
    /// The socket travels with the support because the rebuild loop has no way
    /// to derive it: a `RouteSink` is what says which declared input a branch
    /// feeds, and the loop never sees one.
    fn route_endpoints(
        &self,
        route_index: usize,
        moved_primitive: NodeId,
        old_anchor: Anchor,
        new_anchor: Anchor,
    ) -> (Anchor, Vec<(Anchor, Anchor)>) {
        let route = &self.routes[route_index];
        // A net leaves its producer's output pin, never its support block.
        //
        // `self` is the already-moved candidate, so a node-derived endpoint is
        // at its new position and must not be remapped again: the moved
        // primitive's new pin can land exactly on its own old anchor, and a
        // second remap would drag the route's source onto the support block --
        // laying dust on it and unsupporting the torch.
        let source = match self.node_index_for_route_owner(route) {
            Some(index) => self.primitive_nodes[index].source(),
            None => route
                .anchors
                .first()
                .copied()
                .map(|anchor| {
                    if anchor == old_anchor {
                        new_anchor
                    } else {
                        anchor
                    }
                })
                .unwrap_or(new_anchor),
        };
        let supports = if route.terminals.is_empty() {
            // A route with no declared terminals: a hand-built fixture rather
            // than anything `route_every_net` produces, since `route_in_order`
            // pushes a `RouteTerminal` for every consumer it routes to. There
            // is no declared input index to ask for, so the socket is the
            // geometric guess and nothing better exists.
            let support = route
                .anchors
                .last()
                .copied()
                .map(|anchor| {
                    if anchor == old_anchor {
                        new_anchor
                    } else {
                        anchor
                    }
                })
                .unwrap_or(new_anchor);
            vec![(support, terminal_socket(source, support))]
        } else {
            route
                .terminals
                .iter()
                .map(|terminal| {
                    let support = match self.node_for_gate(&terminal.sink.gate) {
                        // Already moved with its node, as above.
                        Some(anchor) => anchor,
                        None if moved_primitive < self.anchors.len()
                            && terminal.sink.anchor == old_anchor =>
                        {
                            new_anchor
                        }
                        None => terminal.sink.anchor,
                    };
                    (support, self.declared_socket(support, &terminal.sink))
                })
                .collect()
        };
        (source, supports)
    }

    fn node_index_for_route_owner(&self, route: &Route) -> Option<usize> {
        let owner = route.owner.as_deref()?;
        self.primitive_nodes.iter().position(|node| {
            node.id == format!("input:{owner}") || node.id == format!("gate:{owner}")
        })
    }

    fn node_for_gate(&self, gate: &str) -> Option<Anchor> {
        self.primitive_nodes
            .iter()
            .find_map(|node| (node.id == format!("gate:{gate}")).then_some(node.anchor))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SearchState {
    estimate: u64,
    travelled: u64,
    anchor: Anchor,
}

fn deterministic_astar(
    start: Anchor,
    goal: Anchor,
    terminal_support: Anchor,
    owner: &str,
    reservation: &Reservation,
    prices: &Prices,
) -> Option<Vec<Anchor>> {
    let margin = manhattan_distance(start, goal).saturating_add(2) as i32;
    // Y is widened by a couple of levels rather than by `margin`: a route may
    // now climb, because a step is a real staircase and the strength budget
    // knows a stair cannot hold a repeater. It has no reason to climb far, and
    // every level costs the search a whole plane of cells to consider.
    // Climb only. Every cell stands on a floor one level below it, and the
    // gate plane already sits on the lowest floor there is -- a route that
    // descends from it is digging through the ground, which is why one did,
    // and why its blocks were written outside the world and carried nothing.
    const CLIMB: i32 = 3;
    let min = Anchor {
        x: start.x.min(goal.x).saturating_sub(margin),
        y: start.y.min(goal.y),
        z: start.z.min(goal.z).saturating_sub(margin),
    };
    let max = Anchor {
        x: start.x.max(goal.x).saturating_add(margin),
        y: start.y.max(goal.y).saturating_add(CLIMB),
        z: start.z.max(goal.z).saturating_add(margin),
    };
    let mut frontier = BTreeSet::from([SearchState {
        estimate: manhattan_distance(start, goal),
        travelled: 0,
        anchor: start,
    }]);
    let mut travelled = BTreeMap::from([(start, 0_u64)]);
    let mut previous = BTreeMap::new();

    while let Some(state) = frontier.iter().next().copied() {
        frontier.remove(&state);
        if state.anchor == goal {
            return Some(reconstruct_path(previous, goal));
        }
        if travelled.get(&state.anchor) != Some(&state.travelled) {
            continue;
        }
        for next in neighbours(state.anchor) {
            // A path may not build over or under its own steps. Every cell
            // lays a floor one below itself, a drop needs the cell it falls
            // past to stay air, and a climb needs the cell above the one it
            // leaves to stay clear -- so a path that doubles back on itself in
            // Y breaks a step it already took. Neither case is visible to the
            // reservation, because none of this path is written yet.
            if self_obstructs(&previous, state.anchor, next) {
                continue;
            }

            if !within_bounds(next, min, max)
                || !anchor_is_free_for(next, start, goal, terminal_support, owner, reservation)
                || staircase_clearance(state.anchor, next).into_iter().any(|cell| {
                    let foreign = reservation.owner(&cell).is_some_and(|occupied_by| {
                        occupied_by != owner && occupied_by != stair_guard(owner)
                    });
                    // The riser is the one cell a climb *wants* filled -- it
                    // becomes this route's own floor -- so it may already be
                    // ours, or a stair this route built: two branches climbing
                    // one stair is reuse. Everything else a staircase needs has
                    // to be empty, and empty means empty. A solid block is what
                    // stops a climb, and every route lays solid floors, so a
                    // route passing overhead seals this one's way up without
                    // owning anything that conducts.
                    let is_riser = next.y > state.anchor.y && cell.y == state.anchor.y;
                    if is_riser {
                        return foreign || reservation.conductor_owner(&cell).is_some();
                    }
                    reservation.owner(&cell).is_some()
                })
            {
                continue;
            }
            // Every step costs one, height included. Pricing a climb higher
            // was tried and made things worse, for a reason worth keeping:
            // a climb spends one signal strength per level and no cell of it
            // can hold a repeater, so it has to happen while the signal is
            // still fresh. Discouraging it just moves it later, to where the
            // signal can no longer afford it. What a climb needs is for the
            // strength budget to reserve for it in advance --
            // `plan_bent_path` takes a `reserve` for exactly that -- not to
            // be made unattractive.
            // A step in Y costs more than a step across. Not to forbid
            // climbing -- congestion will still buy it where it is the only
            // way through -- but because a staircase can hold no refresh
            // anywhere along it, so a path that wanders up and down leaves the
            // strength budget nowhere to spend. Pricing it alone was tried
            // before there was anything to overrule it, and only moved climbs
            // to where the signal could no longer afford them.
            const CLIMB_COST: u64 = 3;
            let closer_in_y =
                (next.y - goal.y).abs() < (state.anchor.y - goal.y).abs();
            let step_cost = if next.y == state.anchor.y || closer_in_y {
                1
            } else {
                CLIMB_COST
            };
            let next_travelled = state
                .travelled
                .saturating_add(step_cost)
                .saturating_add(prices.price(&next));
            if travelled
                .get(&next)
                .is_some_and(|&known| known <= next_travelled)
            {
                continue;
            }
            travelled.insert(next, next_travelled);
            previous.insert(next, state.anchor);
            frontier.insert(SearchState {
                estimate: next_travelled.saturating_add(manhattan_distance(next, goal)),
                travelled: next_travelled,
                anchor: next,
            });
        }
    }
    None
}

fn reconstruct_path(previous: BTreeMap<Anchor, Anchor>, goal: Anchor) -> Vec<Anchor> {
    let mut path = vec![goal];
    while let Some(&parent) = previous.get(path.last().expect("path is non-empty")) {
        path.push(parent);
    }
    path.reverse();
    path
}

/// Where dust at `anchor` can reach in one step.
///
/// `connectivity::dust_reach` is the rule: the four horizontal neighbours, and
/// each of those one level up or one level down. Never the cell directly above
/// or below -- dust does not stack, it climbs, and a search that thinks
/// otherwise lays runs that carry nothing.
fn neighbours(anchor: Anchor) -> Vec<Anchor> {
    let mut steps = Vec::with_capacity(12);
    for sideways in horizontal_neighbours(anchor) {
        steps.push(sideways);
        steps.push(Anchor { y: sideways.y + 1, ..sideways });
        steps.push(Anchor { y: sideways.y - 1, ..sideways });
    }
    steps
}

/// Whether stepping from `at` to `next` breaks a step this same path already
/// took, or is broken by one.
///
/// Two mirrored cases, both found the same way -- by asking the simulator
/// where a route's signal actually stopped:
///
/// * a drop past a cell whose air is occupied by the floor of a cell the path
///   already passed through, two levels up;
/// * a step into a cell whose own floor lands directly above a cell the path
///   climbed out of, which blocks that climb.
fn self_obstructs(
    previous: &BTreeMap<Anchor, Anchor>,
    at: Anchor,
    next: Anchor,
) -> bool {
    // The cell whose floor would fill the gap this drop needs.
    let drop_blocker = (next.y < at.y).then(|| Anchor {
        x: next.x,
        y: at.y + 1,
        z: next.z,
    });
    // The cell this step's own floor would land on top of.
    let smothered = Anchor {
        x: next.x,
        y: next.y - 2,
        z: next.z,
    };

    let mut successor = None;
    let mut walk = Some(at);
    while let Some(cell) = walk {
        if Some(cell) == drop_blocker {
            return true;
        }
        // `cell` climbed if the step the path took out of it went up.
        if cell == smothered && successor.is_some_and(|after: Anchor| after.y > cell.y) {
            return true;
        }
        successor = Some(cell);
        walk = previous.get(&cell).copied();
    }
    false
}

/// What each cell has cost a net that could not get past it.
///
/// Routing every net once and giving nothing back makes the first net to
/// reach a corridor its owner, and no ordering fixes a layout where corridors
/// have to be shared: segment_a fails with one net laid clean across the only
/// row another needs, and moving either to the front just moves the wall.
///
/// So a net that cannot reach its sink charges the cells that were in its way,
/// and the round is laid again. Nothing is forbidden by this -- a charged cell
/// is still usable, just dearer than going round -- which is what lets the
/// nets separate themselves rather than be ordered by a guess. Prices only
/// ever rise, so a corridor fought over repeatedly ends up avoided by
/// everything that has an alternative.
#[derive(Debug, Default, Clone)]
pub struct Congestion {
    charged: BTreeMap<Anchor, u64>,
}

impl Congestion {
    /// What using `cell` adds to a route's length, in cells.
    fn price(&self, cell: &Anchor) -> u64 {
        const PER_ROUND: u64 = 6;
        self.charged.get(cell).copied().unwrap_or(0) * PER_ROUND
    }

    /// Charge these cells whatever they belong to.
    fn charge_cells(&mut self, cells: &[Anchor]) -> bool {
        for cell in cells {
            *self.charged.entry(*cell).or_insert(0) += 1;
        }
        !cells.is_empty()
    }

    /// Charge every cell inside the box a net gave up crossing that belongs to
    /// somebody else. Its own cells and the fixed furniture are not charged:
    /// primitives cannot move, and a net cannot get out of its own way.
    fn charge(&mut self, reservation: &Reservation, from: Anchor, to: Anchor, mine: &str) -> bool {
        let mut charged_any = false;
        for cell in reservation.cells_within(from, to) {
            let Some(owner) = reservation.owner(&cell) else {
                continue;
            };
            if owner == mine || owner.starts_with("primitive:") {
                continue;
            }
            *self.charged.entry(cell).or_insert(0) += 1;
            charged_any = true;
        }
        charged_any
    }
}

/// What a cell costs a search beyond the step onto it.
///
/// Two routers, one search. [`deterministic_astar`] is identical under both;
/// the whole of the difference between them is which of these it is handed and
/// what the reservation it is handed contains.
///
/// * [`Prices::RipUp`] is the shipping router: the price is [`Congestion`]'s
///   flat, never-decaying bounding-box charge, and every foreign cell is a
///   *refusal* in [`anchor_is_free_for`] besides.
/// * [`Prices::Negotiated`] is [`route_negotiated`]: foreign routed cells are
///   absent from the reservation the search sees, so the price is the only
///   thing that expresses them.
enum Prices<'a> {
    RipUp(&'a Congestion),
    Negotiated {
        table: &'a Negotiation,
        mine: &'a str,
    },
}

impl Prices<'_> {
    fn price(&self, cell: &Anchor) -> u64 {
        match self {
            Prices::RipUp(congestion) => congestion.price(cell),
            Prices::Negotiated { table, mine } => table.price(cell, mine),
        }
    }
}

/// The owner a route's staircase structure is claimed under: its own name
/// would let its other branches through, which is exactly what has to be
/// stopped.
fn stair_guard(owner: &str) -> String {
    format!("stair:{owner}")
}

/// The cells a staircase step needs, beyond the one it lands on.
///
/// Climbing to `to` puts dust on top of the cell beside `from`, so that cell
/// becomes the riser -- which is also the floor the realisation lays under
/// `to`, so it costs nothing extra as long as nobody else owns it. The cell
/// directly above `from` has to stay clear, or the climb is blocked.
///
/// Descending needs the opposite: the cell beside `from` must stay *empty*,
/// because a solid one there is what would have made this a climb instead.
fn staircase_clearance(from: Anchor, to: Anchor) -> Vec<Anchor> {
    if to.y == from.y {
        return Vec::new();
    }
    let riser = Anchor { y: from.y, ..to };
    if to.y > from.y {
        vec![riser, Anchor { y: from.y + 1, ..from }]
    } else {
        vec![riser]
    }
}

fn within_bounds(anchor: Anchor, min: Anchor, max: Anchor) -> bool {
    anchor.x >= min.x
        && anchor.x <= max.x
        && anchor.y >= min.y
        && anchor.y <= max.y
        && anchor.z >= min.z
        && anchor.z <= max.z
}

fn anchor_is_free_for(
    anchor: Anchor,
    start: Anchor,
    goal: Anchor,
    terminal_support: Anchor,
    owner: &str,
    reservation: &Reservation,
) -> bool {
    if anchor != start
        && anchor != goal
        && reservation
            .owner(&anchor)
            .is_some_and(|occupied_by| occupied_by != owner)
    {
        return false;
    }
    // A cell the plan has already committed to stone stays stone -- for every
    // owner, this net's own included. Two things stand on that commitment: the
    // routed cell one storey up, whose floor it is, and the two nets it holds
    // apart, because `keep_out_against` now reads exactly this entry to decide
    // a vertical pair. Dust here deletes both at once, and `emit_routes` would
    // let it: it writes floor-then-block per anchor in route order, so a later
    // anchor lands on top of an earlier floor without a word.
    if reservation.stone_owner(&anchor).is_some() {
        return false;
    }
    // Every cell stands on a floor, and realisation writes that floor as
    // stone. Laying one over another net's conductor deletes it -- which is
    // exactly what a route climbing to the storey above did to the trunk
    // running underneath it, replacing live dust with the floor it stood on.
    let below = Anchor {
        y: anchor.y - 1,
        ..anchor
    };
    // Any conductor, including this net's own: a net may run beside itself,
    // but burying its own trunk under the floor of a branch climbing over it
    // deletes the trunk just as thoroughly as burying a stranger's.
    if reservation.conductor_owner(&below).is_some() {
        return false;
    }
    // THE LID RULE (2026-08-19). A cell a laid staircase needs to stay air --
    // any staircase, this net's own included -- refuses this cell, because
    // this cell's floor is stone written exactly there, and a stone lid is
    // what cuts a climb: `docs/derived/dust-join-relation.md`'s closed form,
    // climb joins iff the step supports and the lid does not conduct. This is
    // game physics, not contention, so it is a refusal and never a price.
    // Measured before this arm existed: negotiated segment_a's g0 laid its
    // third branch at (86,3,109), one storey over its own certified climb
    // (86,1,109) -> (87,2,109); every arm of this function passed, the floor
    // sealed the lid, and the simulator read the climb dead at all 16 vectors
    // (`the_dead_climb_of_negotiated_segment_a_read_to_the_cell`). The
    // symmetric half -- refusing a NEW climb whose headroom is already
    // committed, by anyone -- is `deterministic_astar`'s staircase arm, which
    // refuses any owned clearance cell.
    if reservation.air_owner(&below).is_some() {
        return false;
    }

    keep_out(anchor).into_iter().all(|neighbour| {
        neighbour == start
            || neighbour == goal
            || (anchor == goal && neighbour == terminal_support)
            || reservation
                .conductor_owner(&neighbour)
                .is_none_or(|occupied_by| occupied_by == owner)
    })
}

/// The cells a dust at `anchor` could join, geometrically.
///
/// `connectivity::dust_reach` is the exact rule but needs a world, and a plan
/// is checked before one exists. This is its conservative shape: each of the
/// four horizontal neighbours, and the cell above and below each -- dust
/// climbs and descends one step, which is how a route that looks clear in
/// plan view ends up shorted to the net running one layer down.
fn keep_out(anchor: Anchor) -> Vec<Anchor> {
    let mut cells = Vec::with_capacity(12);
    for neighbour in horizontal_neighbours(anchor) {
        cells.push(neighbour);
        cells.push(Anchor { y: neighbour.y + 1, ..neighbour });
        cells.push(Anchor { y: neighbour.y - 1, ..neighbour });
    }
    cells
}

/// The one cell whose material decides a vertical [`keep_out`] pair -- and
/// `None` for a same-layer pair, which is joined unconditionally and has no
/// such cell.
///
/// `docs/derived/dust-join-relation.md`, closed form, with P the higher of the
/// two dust cells and Q the lower, `S = P.down()` the step and `C = Q.up()`
/// the lid:
///
/// ```text
/// same layer       joined, unconditionally, both ways
/// Q -> P (climb)   supports_dust_step(S) && !is_conductive(C)
/// P -> Q (descend) !supports_dust_step(C)
/// ```
///
/// Either direction merges two nets, so the pair is apart only when both are
/// false. **`S` drops out of that conjunction here, and the reason is the
/// router's own invariant, not an approximation**: realisation lays a stone
/// floor under every routed cell (`realise_branch_from`'s `floors`,
/// `emit_routes`), and `S` is exactly the floor under P -- so
/// `supports_dust_step(S)` is true whenever the upper conductor exists at all.
/// What is left is `supports_dust_step(C) && is_conductive(C)`, and in this
/// compiler's write vocabulary the second implies the first. Hence: **a
/// vertical pair is apart iff the lid is a conductive full block**, which is
/// what [`Occupancy::Stone`] records.
///
/// Only [`keep_out_against`] reads this, and that function is not wired into
/// the router -- see its doc comment for the two measurements that stopped it.
#[cfg(test)]
fn join_lid(anchor: Anchor, neighbour: Anchor) -> Option<Anchor> {
    match neighbour.y.cmp(&anchor.y) {
        std::cmp::Ordering::Equal => None,
        // `neighbour` is the higher one, so `anchor` is Q and the lid is
        // `anchor`'s own ceiling.
        std::cmp::Ordering::Greater => Some(Anchor {
            y: anchor.y + 1,
            ..anchor
        }),
        // `anchor` is the higher one, so `neighbour` is Q and the lid is the
        // cell over it -- a same-layer horizontal neighbour of `anchor`.
        std::cmp::Ordering::Less => Some(Anchor {
            y: anchor.y,
            ..neighbour
        }),
    }
}

/// [`keep_out`]'s twelve cells as asked **from a cell that will hold wire**,
/// less the vertical ones this reservation has already sealed.
///
/// A cell leaves the list only when both halves of the derivation hold:
///
/// 1. the offender is this plan's own [`Occupancy::Wire`], which is what the
///    join relation was measured over -- a [`Occupancy::GateConductor`] is
///    kept clear by up to four different rules and only one of them is the
///    join relation, so it never leaves; and
/// 2. [`join_lid`]'s cell is committed [`Occupancy::Stone`].
///
/// The four same-layer cells never leave either: `dust_connections`'
/// same-layer arm has no gate to open, measured over all ten blocks the
/// compiler can write (`tests/dust_join_relation.rs`).
///
/// **What this is and is not exact about.** As a predicate on the reservation
/// it is given it is exact: a cell nobody claims is air in the emitted world,
/// air is neither conductive nor step-supporting, and every other value the
/// reservation can hold is a non-lid too. What it cannot be is prescient --
/// asked mid-search, the lid may be claimed as some later net's floor and
/// become stone after this cell was refused. That residue is the search's
/// order-dependence, not a gap between the rule and the game; and it errs
/// towards refusing, so it can cost a route and cannot cause a short.
///
/// **Asked from a gate's side, use [`keep_out`] instead.** There the cell
/// under test is a gate conductor, so premise 1 fails on the *other* side of
/// the pair and there is nothing to harvest.
///
/// ---
///
/// # THIS IS NOT WIRED INTO THE ROUTER, AND THE REASON IS MEASURED
///
/// `anchor_is_free_for` still asks [`keep_out`] for all twelve. Wiring this in
/// was tried on 2026-08-16 and is refused by two independent measurements,
/// both reproducible in this module:
///
/// 1. **The lid seals dust and does nothing at all for a repeater.**
///    `a_stone_lid_seals_a_dust_pair_and_does_not_seal_a_repeater` puts a
///    repeater in Q's cell aimed at P's floor and reads P at **15** with the
///    lid stone -- against **0** for dust in the same cell, same lid. The
///    derived relation is `dust_connections`, and `dust_connections` is
///    dust-against-dust; a repeater reaches P by strongly powering the floor
///    block P stands on, which no lid touches. [`Occupancy::Wire`] covers
///    both, because `realise_branch_from` decides which cells of a laid path
///    become repeaters from a strength budget computed **after**
///    [`reserve_path`] has already written the reservation this rule reads.
///    So the query is asked before the answer exists -- decidable in
///    principle, undecided in fact, and in the unsafe direction.
/// 2. **Even where the pair itself is clean, the reroute is not.** With this
///    active -- and with the `Wire`/`GateConductor` split already in place, so
///    this is not the conflation -- `plan_from_netlist`'s full_adder permitted
///    exactly two vertical pairs, `(37,2,124)`/`(37,1,125)` and
///    `(43,2,118)`/`(42,1,118)`, and both were then confirmed electrically
///    clean in isolation against controls. The circuit still came out with
///    **2 of 8 truth-table rows wrong** (`011` and `101`), and
///    `verify_realised_world` refused it: `TorchMergeViolation { gate: "g2",
///    ForeignNetReachesSupport { torch: (40, 1, 131), support: (40, 1, 132),
///    net: "g3" } }`. Traced through `net_reach`'s own walk: g3's terminal
///    repeater at `(40,1,124)` strongly powers the block in front of it, and
///    g0's dust at `(40,1,126)` sits on the far side of that block, so g3
///    drives g0's whole wire and g0 reaches g2's support. That is a hazard of
///    the *terminal* model, exposed by the reroute rather than caused by it --
///    **whether any other perturbation of this router would also expose it is
///    NOT MEASURED** -- but it is what "everything that verified still
///    verifies" cost, so it is recorded here rather than downstream.
///
/// Kept, tested and `#[cfg(test)]` rather than deleted, because the derivation
/// is right and it is the *premise set* that is short: the same function
/// becomes shippable the moment the reservation records which routed cells
/// realise as repeaters. See §8.17 of `2026-08-15-routing-at-scale.md`.
#[cfg(test)]
fn keep_out_against(anchor: Anchor, reservation: &Reservation) -> Vec<Anchor> {
    keep_out(anchor)
        .into_iter()
        .filter(|neighbour| {
            let sealed = reservation.wire_owner(neighbour).is_some()
                && join_lid(anchor, *neighbour)
                    .is_some_and(|lid| reservation.stone_owner(&lid).is_some());
            !sealed
        })
        .collect()
}

/// Every cell every primitive occupies, at the occupancy the primitive itself
/// declares.
///
/// One function because there were two copies of it and they had drifted:
/// `route_in_order`'s and `PlanCandidate::live_reservation`'s were the same
/// five lines, except that the second ran after a sweep that had already
/// written every anchor as `Solid`, and `Reservation::insert` cannot upgrade
/// what is already there. The two disagreed about every support and every
/// lever in the tree. Neither copy was wrong to read; the pair was.
fn reserve_primitives(nodes: &[PrimitiveNode]) -> Reservation {
    let mut reservation = Reservation::new();
    for (index, node) in nodes.iter().enumerate() {
        let owner = format!("primitive:{index}");
        for &cell in node.occupied() {
            reservation.insert(cell, &owner, node.occupancy_of(cell));
        }
    }
    reservation
}

fn reserve_path(reservation: &mut Reservation, owner: &str, path: &[Anchor]) {
    let guard = stair_guard(owner);
    for window in path.windows(2) {
        for cell in staircase_clearance(window[0], window[1]) {
            // Claimed under a name nobody routes as, so not even another
            // branch of this same net may take it. A riser has to stay a solid
            // block and a descent has to stay air; a branch that runs through
            // either one destroys the staircase that depends on it, and a net
            // is otherwise free to run through its own cells.
            //
            // **The two are no longer the same entry.** A climb's riser is the
            // block the upper cell stands on, so realisation writes stone into
            // it -- it is that cell's own floor, written by the loop below as
            // well. The cell over the climber's head, and the cell a descent
            // needs left empty, both have to stay AIR -- `Occupancy::Air`, the
            // commitment the lid rule in `anchor_is_free_for` reads, and the
            // join rule reads the riser/air difference: see [`join_lid`].
            let is_riser = window[1].y > window[0].y && cell.y == window[0].y;
            reservation.insert(
                cell,
                &guard,
                if is_riser {
                    Occupancy::Stone
                } else {
                    Occupancy::Air
                },
            );
        }
    }
    for &anchor in path {
        reservation.insert(anchor, owner, Occupancy::Wire);
        // The floor this cell stands on is this route's too. Inert, because a
        // floor is inert: another net may run beside it, just not through it.
        // `Stone` rather than `Solid` because that is what realisation puts
        // there -- `realise_branch_from` fills `floors` with
        // `compile::stone()` and `emit_routes` writes it -- and because the
        // join rule needs to know.
        reservation.insert(
            Anchor {
                y: anchor.y - 1,
                ..anchor
            },
            owner,
            Occupancy::Stone,
        );
    }
}


/// The socket a route ends in, guessed from the direction it approached out
/// of -- the answer of last resort, for a route whose sink the netlist never
/// declared.
///
/// Since Task 10 it has one production caller -- `route_endpoints`'
/// no-terminals arm -- and one in a test, where
/// `a_rebuilt_branch_aims_at_the_socket_the_netlist_declared` uses it to prove
/// the two answers differ. Every route `route_every_net` produces carries a
/// `RouteTerminal` per consumer, so that arm is reachable only from a
/// hand-built fixture, and `PlanCandidate::declared_socket` is what answers for
/// everything else. The two agree while every gate faces north and disagree
/// once relaxation starts turning them, which is why the declared answer is the
/// one that survived.
fn terminal_socket(source: Anchor, support: Anchor) -> Anchor {
    let direction = preferred_axis_direction(source, support);
    Anchor {
        x: support.x - direction.0,
        y: support.y - direction.1,
        z: support.z - direction.2,
    }
}

fn preferred_axis_direction(from: Anchor, to: Anchor) -> (i32, i32, i32) {
    let delta = (to.x - from.x, to.y - from.y, to.z - from.z);
    if delta.0 != 0 {
        (delta.0.signum(), 0, 0)
    } else if delta.2 != 0 {
        (0, 0, delta.2.signum())
    } else if delta.1 != 0 {
        (0, delta.1.signum(), 0)
    } else {
        (1, 0, 0)
    }
}

fn terminal_is_isolated(
    reservation: &Reservation,
    owner: &str,
    predecessor: Anchor,
    terminal: Anchor,
    support: Anchor,
) -> bool {
    let _ = owner;
    // Any conductor beside the terminal spoils it, including one belonging to
    // this same net: a directed dust terminal has to be a straight line into
    // the support, and a second branch of its own route running alongside
    // gives it another connection and turns it into a corner. Keep-out used to
    // hide this by keeping everything away; it does not any more.
    horizontal_neighbours(terminal)
        .into_iter()
        .all(|neighbour| {
            neighbour == predecessor
                || neighbour == support
                || reservation.conductor_owner(&neighbour).is_none()
        })
}

fn horizontal_neighbours(anchor: Anchor) -> [Anchor; 4] {
    [
        Anchor {
            x: anchor.x - 1,
            ..anchor
        },
        Anchor {
            x: anchor.x + 1,
            ..anchor
        },
        Anchor {
            z: anchor.z - 1,
            ..anchor
        },
        Anchor {
            z: anchor.z + 1,
            ..anchor
        },
    ]
}

fn unit_horizontal_direction(from: Anchor, to: Anchor) -> Option<(i32, i32, i32)> {
    let direction = (to.x - from.x, to.y - from.y, to.z - from.z);
    (direction.1 == 0 && direction.0.unsigned_abs() + direction.2.unsigned_abs() == 1)
        .then_some(direction)
}

/// What a reserved cell holds, as far as keep-out is concerned.
///
/// The channel-safety spec derives the rule from `dust_reach`: it is two
/// *conductor* cells of different nets that need clearance. A support block or
/// a floor is occupied -- nothing else may be written there -- but a route may
/// pass beside it, which is what the cell in front of every gate is.
/// **Four values, not two, and two of the three splits are load-bearing.**
///
/// `docs/derived/dust-join-relation.md` derives that a vertical pair of dust
/// cells is joined *unless* the cell directly above the lower one is a
/// conductive full block. Acting on that derivation needs two distinctions the
/// old pair could not make:
///
/// 1. **`Stone` out of `Solid`** -- which occupied cells are conductive full
///    blocks. `Solid` was the catch-all for "occupied and not a conductor",
///    and [`reserve_path`] wrote a climb's stone riser and the cell over the
///    climber's head -- which has to stay **air** -- under the same value.
/// 2. **`Wire` out of `GateConductor`** -- which cells the derivation is
///    actually *about*. It is about dust against dust; `gate_footprint` marks
///    three things conductors that are not dust and are kept clear for three
///    other reasons entirely. Deciding those by the join relation produced a
///    measurably wrong circuit; see [`Occupancy::GateConductor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    /// Redstone wire this plan will lay: dust, or the repeater the strength
    /// budget puts in the middle of a run. The derived join relation is
    /// measured for exactly this against exactly this, so this is the only
    /// value the exact vertical rule acts on.
    Wire,
    /// A cell of a primitive that keeps foreign wire out.
    ///
    /// **Not a synonym for `Wire`, and the split is about the *scope of the
    /// derivation*.** `compile::gate_footprint` calls a cell a conductor when
    /// any of at least four different physical rules needs foreign dust kept
    /// away from it:
    ///
    /// - a NOR's support block, which is **stone** -- dust laid against it
    ///   powers it and turns the torch off;
    /// - the **air** cell above a torch -- a lit torch strongly powers it and
    ///   a strongly powered block drives every dust beside it;
    /// - the input sockets, stone placeholders for whatever the router lands;
    /// - and the genuinely dusty parts, the output pin and a lever's cell.
    ///
    /// Only the last of those is the dust-join relation, so only the last is
    /// something that relation may be allowed to decide.
    ///
    /// **NOT MEASURED: whether letting the exact rule decide this value too
    /// breaks anything.** The wrong circuit that stopped the exact rule
    /// (`keep_out_against`'s doc comment) reproduced with this split already in
    /// place, so it is not evidence about the split; and with the rule out of
    /// the router there is no configuration left in which the question is
    /// asked. The split stands on the derivation's scope, not on a measurement.
    GateConductor,
    /// A cell realisation writes as **stone**, and the plan is bound to it.
    ///
    /// [`reserve_path`] writes this for the floor under every routed cell and
    /// for a climb's riser; `emit_routes` writes `compile::stone()` into
    /// exactly those cells (`realise_branch_from`'s `floors` is
    /// `vec![compile::stone(); ..]` and nothing else ever fills it).
    ///
    /// **It is NOT the commitment this comment used to claim it was.** The
    /// original wording said nothing routed later can turn it back into air,
    /// because [`anchor_is_free_for`] refuses wire in such a cell for every
    /// owner including its own. The guard is real and it is live, but it can
    /// only see the reservation, and [`reserve_path`] runs *after* a whole path
    /// is chosen -- so a route never sees its own floors while it is searching.
    /// A path that visits `(x, y+1, z)` at one index and `(x, y, z)` at a later
    /// one writes floor-`Stone` first and anchor-`Wire` second, and
    /// [`Reservation::insert`] is `or_insert`, so the entry stays `Stone` while
    /// `emit_routes` writes dust over it.
    ///
    /// Measured on `full_adder` through the shipping `plan_from_netlist`, and
    /// every one of them is a route clashing with *itself*:
    /// route `b` at `(40, 1, 137)`, `g11` at `(58, 1, 98)`, `g14` at
    /// `(57, 1, 83)`. `a_route_lays_dust_on_its_own_committed_stone` pins them.
    ///
    /// So `stone_owner` is sound across nets and **lies within one**. Anything
    /// that reasons from it -- the exact lid rule above is the first thing that
    /// does -- is reasoning about a world that will not exist at those three
    /// cells. That is why the exact rule is not wired into the shipping router.
    Stone,
    /// A cell the plan has committed to **air**, and realisation depends on
    /// it: the cell over a climber's head and the cell a descent falls past.
    /// [`reserve_path`] writes this under a `stair:` guard and nothing else
    /// writes it at all.
    ///
    /// Split out of [`Occupancy::Solid`] on 2026-08-19, because the two mean
    /// opposite futures and one rule has to tell them apart: `Solid` is
    /// "something stands here", `Air` is "nothing may ever stand here". The
    /// measured failure that forced the split is negotiated `segment_a`'s g0,
    /// whose third branch laid a wire one storey above its own certified
    /// climb -- the wire's floor landed in the climb's headroom, this entry
    /// said `Solid` like a torch does, `anchor_is_free_for`'s below-floor arm
    /// asks only `conductor_owner`, and the stone that `emit_routes` then
    /// wrote into the lid cut the climb dead
    /// (`the_dead_climb_of_negotiated_segment_a_read_to_the_cell`). The lid
    /// rule -- `anchor_is_free_for` refusing any anchor whose floor cell is
    /// committed `Air`, own net included -- reads exactly this value.
    Air,
    /// Occupied, and *not* known to be a conductive full block: a gate's torch
    /// or lever, or a terminal guard. For the join rule this reads the
    /// same as air, which is the safe reading -- it refuses.
    Solid,
}

/// Which cells are taken, by whom, and whether they conduct.
#[derive(Debug, Default, Clone)]
pub struct Reservation {
    cells: BTreeMap<Anchor, (String, Occupancy)>,
}

impl Reservation {
    fn new() -> Self {
        Self::default()
    }

    fn insert(&mut self, anchor: Anchor, owner: &str, occupancy: Occupancy) {
        self.cells
            .entry(anchor)
            .or_insert_with(|| (owner.to_string(), occupancy));
    }

    fn remove(&mut self, anchor: &Anchor) {
        self.cells.remove(anchor);
    }

    fn owner(&self, anchor: &Anchor) -> Option<&str> {
        self.cells.get(anchor).map(|(owner, _)| owner.as_str())
    }

    /// The owner of a cell that conducts; `None` for an empty cell or one
    /// holding nothing but solid material.
    fn conductor_owner(&self, anchor: &Anchor) -> Option<&str> {
        self.cells.get(anchor).and_then(|(owner, occupancy)| {
            matches!(occupancy, Occupancy::Wire | Occupancy::GateConductor)
                .then_some(owner.as_str())
        })
    }

    /// The owner of a cell that will hold this plan's own **wire** -- dust or
    /// a repeater -- as opposed to a primitive's cell that merely keeps wire
    /// out. The exact vertical rule acts on this and not on
    /// [`Reservation::conductor_owner`], because the derived join relation is
    /// about wire against wire and a gate's "conductors" are mostly not wire.
    ///
    /// `#[cfg(test)]` for the same reason [`keep_out_against`] is: its only
    /// caller is that rule, and that rule is not in the router. The
    /// *distinction* it reads is not test-only -- [`reserve_path`] writes
    /// `Wire` on the shipping path -- only this query is.
    #[cfg(test)]
    fn wire_owner(&self, anchor: &Anchor) -> Option<&str> {
        self.cells.get(anchor).and_then(|(owner, occupancy)| {
            matches!(occupancy, Occupancy::Wire).then_some(owner.as_str())
        })
    }

    /// The owner of a cell the plan has committed to **stone**.
    ///
    /// The one query the exact vertical keep-out rule makes. `None` means
    /// "will not be a conductive full block" -- air, dust, a torch, a lever,
    /// or a cell nobody has claimed at all -- and every one of those leaves a
    /// vertical pair joined, so `None` is a refusal.
    fn stone_owner(&self, anchor: &Anchor) -> Option<&str> {
        self.cells.get(anchor).and_then(|(owner, occupancy)| {
            matches!(occupancy, Occupancy::Stone).then_some(owner.as_str())
        })
    }

    /// The owner of a cell the plan has committed to stay **air** -- a laid
    /// climb's headroom or a laid descent's drop, [`Occupancy::Air`] under a
    /// `stair:` guard. `Some` means a staircase already depends on this cell
    /// being empty, so a floor written here cuts that staircase: the lid rule
    /// in [`anchor_is_free_for`] is the reader.
    fn air_owner(&self, anchor: &Anchor) -> Option<&str> {
        self.cells.get(anchor).and_then(|(owner, occupancy)| {
            matches!(occupancy, Occupancy::Air).then_some(owner.as_str())
        })
    }

    /// What this reservation calls a cell, whoever owns it.
    ///
    /// A read-only accessor, `#[cfg(test)]`, for the arithmetic harness: the
    /// four typed queries above each collapse the value to a yes/no, and the
    /// question "what did the plan think was here" needs the value itself.
    #[cfg(test)]
    fn occupancy(&self, anchor: &Anchor) -> Option<Occupancy> {
        self.cells.get(anchor).map(|(_, occupancy)| *occupancy)
    }

    /// The cells `owner` has claimed that must end up **air**.
    ///
    /// Asked of a `stair:` guard, this is [`staircase_clearance`]'s
    /// mandatory-air half -- the cell over a climber's head and the one a
    /// descent falls past -- read back out of what [`reserve_path`] wrote
    /// rather than derived a second time. [`Occupancy::Air`] is exactly that
    /// commitment, and a guard's other cell, the riser, is
    /// [`Occupancy::Stone`].
    fn mandatory_air_of(&self, owner: &str) -> Vec<Anchor> {
        self.cells
            .iter()
            .filter(|(_, (held_by, occupancy))| {
                held_by == owner && matches!(occupancy, Occupancy::Air)
            })
            .map(|(cell, _)| *cell)
            .collect()
    }

    /// Every reserved cell inside the box spanned by `from` and `to`.
    fn cells_within(&self, from: Anchor, to: Anchor) -> Vec<Anchor> {
        let (lo, hi) = (
            Anchor {
                x: from.x.min(to.x),
                y: from.y.min(to.y),
                z: from.z.min(to.z),
            },
            Anchor {
                x: from.x.max(to.x),
                y: from.y.max(to.y),
                z: from.z.max(to.z),
            },
        );
        self.cells
            .keys()
            .filter(|cell| {
                cell.x >= lo.x && cell.x <= hi.x && cell.z >= lo.z && cell.z <= hi.z
            })
            .copied()
            .collect()
    }

    fn is_taken(&self, anchor: &Anchor) -> bool {
        self.cells.contains_key(anchor)
    }
}

/// Where the ports somebody has decided about must go.
///
/// A port has no position by default. Fixing every lever and lamp before
/// planning starts is what stops a layout ever being as small as it could be:
/// the planner can compact everything between the ports and nothing about the
/// ports themselves. So this is an input, it defaults to empty, and whatever
/// nobody pins the planner places and `optimise` is free to move.
#[derive(Debug, Default, Clone)]
pub struct PortPlacements {
    fixed: BTreeMap<String, Anchor>,
}

impl PortPlacements {
    /// Require `port` to sit at `anchor`. Nothing may move it afterwards.
    pub fn pin(&mut self, port: impl Into<String>, anchor: Anchor) -> &mut Self {
        self.fixed.insert(port.into(), anchor);
        self
    }

    pub fn get(&self, port: &str) -> Option<Anchor> {
        self.fixed.get(port).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.fixed.is_empty()
    }
}

/// Storeys are far enough apart that one's routing cannot reach the next, and
/// close enough that a route can climb between them on one staircase.
///
/// Both bounds are real. A route may rise `CLIMB` levels above its own plane
/// to cross another, and every cell stands on a floor one below, so a storey
/// owns that band plus the floor under it -- the pitch must clear it. And a
/// staircase cell can never hold a repeater, so a climb spends one signal
/// strength per level with no chance to refresh: a pitch of nine was enough
/// to kill the signal before it arrived.
const STOREY_PITCH: i32 = 5;

/// Deliberately sparse. A single routing plane with no reserved corridors --
/// which is what this is, and what channels and tracks exist to avoid -- needs
/// slack between rows or the searches block each other and a net simply has
/// nowhere to go.
///
/// Sparse is what makes it a *starting* layout. Compaction is relaxation's job
/// since Task 10 -- `separation` is a derived distance and this is not one --
/// and `optimise` still runs after it. Both can only work on a layout that
/// exists, which is what these four numbers are for.
const ROW_PITCH: i32 = 16;
const COLUMN_PITCH: i32 = 20;
const GROUND_ROW_Z: i32 = 5;
const GATE_COLUMN_X: i32 = 14;
const INPUT_COLUMN_X: i32 = 12;
const PLANNER_Y: i32 = 1;

/// Every anchor and facing this candidate chose, in candidate-node order.
///
/// Text rather than a hash: when two toolchains disagree, the useful output is
/// which node moved, not that something did.
///
/// **What a match proves, and what it does not.** Every number here has been
/// through [`relax::snap`], which rounds and casts to `i32`, so this compares
/// the *visible* layout -- the one the viewer draws and the emitter builds --
/// and nothing finer. Two toolchains whose solves differ by less than the
/// distance to the nearest rounding boundary produce the same string. That
/// distance is not small. Measured by `measure_snapped_fingerprint_slack` on
/// 2026-08-15: the tightest rounding margin is **0.0268 cells** on and4,
/// 0.0085 on full_adder, 0.0016 on segment_a, 0.0033 on seven_segment. Against
/// an `f64` epsilon of 2.2e-16 that is thirteen orders of magnitude of room, so
/// a matching string here means the two toolchains agree to about a fortieth of
/// a cell and says nothing finer. The same harness reports the other margin
/// this string depends on -- the facing argmin, which has no boundary to cross
/// at all -- at 4.55e-2 relative on and4 and ~4e-3 elsewhere, with no exact
/// ties. [`continuous_placement_fingerprint`] is the companion that closes the
/// gap; this one is the readable half.
pub fn placement_fingerprint(candidate: &PlanCandidate) -> String {
    candidate
        .anchors()
        .iter()
        .enumerate()
        .map(|(node, anchor)| {
            format!(
                "{node} {} {} {} {}",
                anchor.x,
                anchor.y,
                anchor.z,
                candidate.facing_of(node).index()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The same placement one step earlier, before [`relax::snap`] rounds it: every
/// body's position as raw `f64` bits, plus the step count the loop exited on.
///
/// [`placement_fingerprint`] is what the browser and the emitter can see, and
/// that is exactly why it is the weaker of the two tests. It answers "do the
/// toolchains lay out the same circuit *today*"; this one answers "do they
/// compute the same numbers", which is what decides whether they will still lay
/// out the same circuit on a netlist nobody has run yet. A divergence in the
/// last bits is invisible to the rounded string until some body happens to sit
/// near a half-integer, and then it appears as a whole cell with no warning and
/// no way to date it.
///
/// Bodies, not nodes: the solver's unknowns are bodies, and `snap` collapses
/// several of them onto one node. Hex `f64` bits, not decimal: `{}` on an `f64`
/// prints the shortest string that round-trips, which is a lossless but
/// *unstable* rendering -- two bit patterns can only differ here if they really
/// differ, and the digits do not move when a formatter is improved.
pub fn continuous_placement_fingerprint(
    netlist: &Netlist,
    placements: &PortPlacements,
) -> Result<String, PlannerError> {
    let placement = relaxed_placement(netlist, placements, SHIPPING_AXES)?;
    let mut out = format!("steps {}\n", placement.iterations);
    for (index, body) in placement.graph.bodies.iter().enumerate() {
        out.push_str(&format!(
            "{index} {:016x} {:016x} {:016x} {}\n",
            body.position[0].to_bits(),
            body.position[1].to_bits(),
            body.position[2].to_bits(),
            body.facing.index()
        ));
    }
    Ok(out)
}

/// The axis set every shipping placement is solved on.
///
/// A constant with two readers rather than a literal at each: this is the one
/// thing that decides which solve happens, and
/// [`continuous_placement_fingerprint`] exists to fingerprint *that* solve. A
/// second `Axes::IN_PLANE` written out by hand is a second thing to flip, and a
/// fingerprint taken on an axis set the placer no longer uses is a test that
/// still passes while measuring a circuit nobody builds.
///
/// Still not a `pub` knob -- see [`plan_from_netlist`] for why that is `Shape`
/// again -- and see [`relax::VERTICAL_CLEARANCE`] for what `Axes::ALL` costs.
const SHIPPING_AXES: relax::Axes = relax::Axes::IN_PLANE;

/// Which router every shipping plan is routed by.
///
/// **`RipUp` in this commit, deliberately.** [`route_negotiated`] is built,
/// measurable beside the old one through
/// [`plan_from_netlist_with_router`], and does not ship until the Verify phase
/// says so. While this says `RipUp`, `compile()` and [`plan_from_netlist`]
/// behave exactly as they did before it existed, and the four pinned block
/// counts (232 / 1,065 / 6,416 / 16,244) do not move.
///
/// A constant with one reader rather than a literal at each call site, for the
/// same reason [`SHIPPING_AXES`] is: this is the one thing that decides which
/// router runs, and a second `RouterKind::RipUp` written out by hand is a
/// second thing to flip.
const SHIPPING_ROUTER: RouterKind = RouterKind::RipUp;

/// Place and route a netlist without the legacy emitter.
///
/// Everything until now has come from `seed_from_legacy`, which bounds the
/// planner to what the row/channel/track router could lay down first: it can
/// improve that layout, but never propose a shape the old emitter has no way
/// of expressing, and never place a primitive the old emitter cannot place.
///
/// The layout is a **relaxation**, not rows and barycentres. Those are still
/// here -- [`starting_layout`] -- because a spring system with hard constraints
/// is not convex and something legal and reproducible has to go in. What comes
/// out is `relax` plus `snap`: springs pull a body toward everything it is
/// wired to, separation pushes back by a derived distance, and each body is
/// built to whichever of the four facings its own incident springs make
/// cheapest. That last part is why this is the first function in the tree that
/// produces a gate not facing north.
///
/// The measurement that motivated it: above and4, the old optimiser found 0
/// legal moves out of 24 -- it could not improve a layout it had no way to
/// move. Relaxation is the replacement for moving, not for `optimise`, which
/// still runs afterwards on whatever this produces. What this buys, measured on
/// 2026-08-14 (`measure_and4_both_ways`, `measure_anchor_boxes`): and4's anchor
/// box 4,095 -> 1,035 cells, 572 -> 232 blocks, delay term 22 -> 10;
/// full_adder's box 10,143 -> 3,465. All of it under `Axes::IN_PLANE`, which is
/// still what this passes -- see the comment on the `relax` call, and
/// `relax::VERTICAL_CLEARANCE` for what `Axes::ALL` was measured to cost.
pub fn plan_from_netlist(
    netlist: &Netlist,
    placements: &PortPlacements,
) -> Result<PlanCandidate, PlannerError> {
    // Not `Axes::ALL`. The projection can reach for height -- the ground plane
    // and the amount guard that made that safe are both shipped -- and Task 11
    // first recorded here that the stacks it produces are never routable, on a
    // derivation that a review then measured and refuted. What is true is
    // narrower: at the shipped requirement the mechanism moves one lever and no
    // gate, in any circuit, at any value tried; and it costs full_adder exactly
    // one route, `(38,1,124)` to `(40,3,124)`. That is a routing defect with an
    // address, not a dead premise. `relax::VERTICAL_CLEARANCE`'s doc carries the
    // table, what the refuted version claimed, and what replaced it;
    // `measure_axes_all_against_the_reference_circuits` re-runs every row.
    //
    // The axis set is a parameter of the two functions below and not of this
    // one: it is this design's decision, not a caller's, and a `pub` knob for
    // it is the `Shape` Task 11 deleted all over again. What it is *here* is
    // the private [`SHIPPING_AXES`], because Task 12 gave it a second reader
    // and a literal written out twice is a thing that can be flipped once.
    plan_with_axes(
        netlist,
        placements,
        SHIPPING_AXES,
        RIP_UP_ROUNDS,
        SHIPPING_ROUTER,
        PresentSchedule::SHIPPING,
    )
}

/// [`plan_from_netlist`], with the router's rip-up budget as a parameter.
///
/// `compile` runs the planner as a *trial* it can abandon, so it buys a
/// cheaper failure with [`TRIAL_RIP_UP_ROUNDS`]. Nothing else does; see that
/// constant for what the two budgets admit and what they cost.
///
/// `pub(crate)` and not `pub`: how hard to try is the compiler's decision, and
/// a public knob for it is the `Shape` Task 11 deleted all over again.
pub(crate) fn plan_from_netlist_within(
    netlist: &Netlist,
    placements: &PortPlacements,
    rip_up_rounds: usize,
) -> Result<PlanCandidate, PlannerError> {
    plan_with_axes(
        netlist,
        placements,
        SHIPPING_AXES,
        rip_up_rounds,
        SHIPPING_ROUTER,
        PresentSchedule::SHIPPING,
    )
}

/// Build the body graph and run the springs. Everything `plan_with_axes` does
/// before rounding.
///
/// Split out so [`continuous_placement_fingerprint`] fingerprints the placement
/// this function returns rather than a re-derivation of it. A copy of these
/// twenty lines living in the fingerprint would make the test agree with itself
/// and stop agreeing with the placer the first time one of them was edited.
fn relaxed_placement(
    netlist: &Netlist,
    placements: &PortPlacements,
    axes: relax::Axes,
) -> Result<relax::ContinuousPlacement, PlannerError> {
    let start = starting_layout(netlist, placements)?;
    let graph = primitive_graph::expand(netlist, &Library::default_library()).map_err(|error| {
        PlannerError::UnrealisableNode {
            id: "netlist".to_string(),
            reason: error.to_string(),
        }
    })?;

    relax::relax(
        netlist,
        &graph,
        &start,
        placements,
        axes,
        relax::RelaxEffort::default(),
    )
    .map_err(PlannerError::Relaxation)
}

/// [`plan_from_netlist`], with the router as a parameter.
///
/// The only way to reach [`route_negotiated`], and `pub(crate)` rather than
/// `pub` for the same reason the rip-up budget is: which router lays the nets
/// is the compiler's decision, not a caller's. It exists so the two routers can
/// be measured side by side on the same placement -- see
/// `what_negotiation_does_to_the_route_the_rip_up_router_detours`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn plan_from_netlist_with_router(
    netlist: &Netlist,
    placements: &PortPlacements,
    budget: usize,
    router: RouterKind,
) -> Result<PlanCandidate, PlannerError> {
    plan_with_axes(
        netlist,
        placements,
        SHIPPING_AXES,
        budget,
        router,
        PresentSchedule::SHIPPING,
    )
}

/// [`plan_from_netlist_with_router`] on [`RouterKind::Negotiated`], with the
/// present-term schedule named rather than assumed.
///
/// Exists so a measurement can say which schedule produced its numbers -- see
/// [`PresentSchedule`]. Passing [`PresentSchedule::SHIPPING`] here is exactly
/// [`plan_from_netlist_with_router`] with [`RouterKind::Negotiated`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn plan_negotiated_on_schedule(
    netlist: &Netlist,
    placements: &PortPlacements,
    budget: usize,
    schedule: PresentSchedule,
) -> Result<PlanCandidate, PlannerError> {
    plan_with_axes(
        netlist,
        placements,
        SHIPPING_AXES,
        budget,
        RouterKind::Negotiated,
        schedule,
    )
}

/// `budget` is rip-up rounds for [`RouterKind::RipUp`] and negotiation
/// iterations for [`RouterKind::Negotiated`]. Two different things counted with
/// one parameter because they are the same thing to a caller: how hard to try.
fn plan_with_axes(
    netlist: &Netlist,
    placements: &PortPlacements,
    axes: relax::Axes,
    budget: usize,
    router: RouterKind,
    schedule: PresentSchedule,
) -> Result<PlanCandidate, PlannerError> {
    let placement = relaxed_placement(netlist, placements, axes)?;
    let snapped = relax::snap(&placement).map_err(PlannerError::Relaxation)?;
    let candidate = candidate_from_snapped(netlist, placements, &snapped);
    match router {
        RouterKind::RipUp => route_every_net(candidate, netlist, budget),
        RouterKind::Negotiated => route_negotiated(candidate, netlist, budget, schedule),
    }
}

/// A rounded placement, as the unrouted candidate the router is handed.
///
/// Split from [`plan_with_axes`] so a probe can place a circuit some other way
/// and still hand the router exactly what the shipping path hands it. A copy of
/// these thirty lines living in a test module would answer a question about the
/// copy the first time one of them was edited -- which is the same reason
/// [`relaxed_placement`] is split out for the fingerprint.
fn candidate_from_snapped(
    netlist: &Netlist,
    placements: &PortPlacements,
    snapped: &[relax::SnappedNode],
) -> PlanCandidate {
    let mut anchors = Vec::with_capacity(snapped.len());
    let mut facings = Vec::with_capacity(snapped.len());
    let mut primitive_nodes = Vec::with_capacity(snapped.len());

    // Pushed in iteration order, which is candidate node order because that is
    // what `snap` promises -- it maps over `anchor_body`, one entry per node,
    // gates then primary inputs, and `snap_answers_once_per_candidate_node_in_
    // candidate_order` is what holds it to that. The two loops below index
    // `anchors` and `facings` by node, so a `snap` that answered in body order
    // would give every gate after a merge's welded repeater somebody else's
    // anchor.
    for node in snapped {
        anchors.push(node.anchor);
        facings.push(node.facing);
    }

    for (index, gate) in netlist.gates.iter().enumerate() {
        let anchor = anchors[index];
        let (footprint, conductors, output_pin) =
            compile::gate_footprint((anchor.x, anchor.y, anchor.z), gate, facings[index]);
        primitive_nodes.push(PrimitiveNode {
            id: format!("gate:{}", gate.output),
            anchor,
            realisation: if gate.is_merge() {
                NodeRealisation::WireMerge
            } else {
                NodeRealisation::Primitive(Primitive::Torch)
            },
            footprint,
            conductors,
            pinned: placements.get(&gate.output).is_some(),
            output_pin: Some(output_pin),
        });
    }

    for (index, input) in netlist.inputs.iter().enumerate() {
        let node = netlist.gates.len() + index;
        let anchor = anchors[node];
        let (cells, pin) = compile::lever_footprint(anchor, facings[node]);
        primitive_nodes.push(PrimitiveNode {
            id: format!("input:{input}"),
            anchor,
            realisation: NodeRealisation::Primitive(Primitive::Lever),
            footprint: cells.clone(),
            conductors: cells,
            pinned: placements.get(input).is_some(),
            output_pin: Some(pin),
        });
    }

    PlanCandidate::with_facings(anchors, primitive_nodes, Vec::new(), facings)
}

/// Gates in rows by depth, one signal per column, primary inputs one row past
/// the deepest.
///
/// Kept, because relaxation starts from it. A spring system with hard
/// constraints is not convex, so the starting point decides which solution it
/// finds: starting everything at the origin gives a knot the projection has to
/// unpick, and starting at random makes the result unreproducible. This layout
/// is legal, reproducible, and measurably poor -- which makes relaxation's job
/// "improve a known-bad answer" rather than "invent one", and its improvement
/// measurable against the numbers it started from.
///
/// It is therefore part of the design rather than scaffolding, and changing it
/// changes the answer. Measured on 2026-08-14 by `measure_anchor_boxes`, which
/// states the metric it uses: the X extent times the Z extent of the node
/// anchors, inclusive, before and after `relax` plus `snap` under
/// `Axes::IN_PLANE` with nothing pinned and `RelaxEffort::default()`. Since
/// Task 11 this function lays exactly one storey -- `Shape` and its
/// `TALL_COLUMN_LIMIT` are deleted -- so a gate is off the ground plane only
/// when a pin puts it there, which is what `STOREY_PITCH` still measures.
///
/// | | from this layout | relaxed | steps |
/// |---|---|---|---|
/// | and4 | 63x65 = 4,095 | 45x23 = **1,035** | 8 |
/// | full_adder | 63x161 = 10,143 | 33x105 = **3,465** | 9 |
/// | segment_a | 203x145 = 29,435 | 89x91 = **8,099** | 11 |
/// | seven_segment | 303x145 = 43,935 | 184x93 = **17,112** | 11 |
///
/// All four reach a converged placement and round onto the lattice. Only the
/// first two then *route*: `how_far_the_planners_own_placement_carries`, run by
/// hand the same day, still stops at segment_a with `no safe local route from
/// (90, 1, 109) to (96, 1, 105)` and so never reaches seven_segment, whose
/// routing has not been measured either way here. Both were already the case
/// before this function was a starting layout rather than the whole answer.
///
/// **seven_segment has since been measured, and it does not route either.**
/// `viewer/tests/placement_agrees_with_native.rs`'s
/// `which_reference_circuits_place_and_which_also_route` swallows failures
/// instead of panicking on the first, so it reports all four rather than
/// stopping at segment_a. On 2026-08-15 at this HEAD: seven_segment fails with
/// `no safe local route from (83, 1, 106) to (83, 1, 96)`. See that test for
/// the segment_a address, which has moved since the run above.
///
/// Whoever replaces this function is changing what relaxation finds, not just
/// where it begins.
///
/// `pub(crate)` rather than private because `relax`'s own measurements start
/// here: `the_anchor_schedule_still_places_and4_small` relaxes from this
/// layout, and it has to be *this* one rather than `plan_from_netlist`'s
/// output, which since Task 10 is already relaxed -- relaxing that again would
/// measure how far a converged placement moves when converged a second time,
/// and would return a small box at any anchor schedule whatever.
pub(crate) fn starting_layout(
    netlist: &Netlist,
    placements: &PortPlacements,
) -> Result<Vec<Anchor>, PlannerError> {
    let depths = gate_depths(netlist)?;
    let deepest = depths.iter().copied().max().unwrap_or(0);
    let producer: BTreeMap<&str, usize> = netlist
        .gates
        .iter()
        .enumerate()
        .map(|(index, gate)| (gate.output.as_str(), index))
        .collect();

    // Primary inputs first: they are the only things with nothing behind them
    // to sit near, so they set the width everything else is measured against.
    let mut input_x: BTreeMap<&str, i32> = BTreeMap::new();
    for (index, input) in netlist.inputs.iter().enumerate() {
        let x = placements
            .get(input)
            .map(|anchor| anchor.x)
            .unwrap_or(INPUT_COLUMN_X + index as i32 * COLUMN_PITCH);
        input_x.insert(input.as_str(), x);
    }

    // Then each gate above whatever feeds it. Filling rows left to right in
    // netlist order instead puts a gate as far from its own inputs as the
    // circuit is wide, and a route that long crosses every row between them
    // -- which is what made seven_segment unroutable rather than merely
    // large.
    let mut gate_x: Vec<i32> = vec![0; netlist.gates.len()];
    // Only a pinned gate is ever off the ground plane now: this function lays
    // one storey and separation decides whether anything leaves it.
    let mut gate_storey: Vec<i32> = vec![0; netlist.gates.len()];
    // A storey's columns are its own: two gates on different storeys may share
    // an x, and a pin is what puts one there.
    let mut taken: BTreeMap<(usize, i32), BTreeSet<i32>> = BTreeMap::new();
    let mut by_depth: Vec<Vec<usize>> = vec![Vec::new(); deepest + 1];
    for (index, &depth) in depths.iter().enumerate() {
        by_depth[depth].push(index);
    }

    for (depth, row_gates) in by_depth.iter().enumerate() {
        let row = deepest - depth;
        for &gate in row_gates {
            if let Some(anchor) = placements.get(&netlist.gates[gate].output) {
                gate_x[gate] = anchor.x;
                gate_storey[gate] = (anchor.y - PLANNER_Y) / STOREY_PITCH;
                taken
                    .entry((row, gate_storey[gate]))
                    .or_default()
                    .insert(anchor.x);
                continue;
            }
            let sources: Vec<i32> = netlist.gates[gate]
                .inputs
                .iter()
                .filter_map(|signal| {
                    producer
                        .get(signal.as_str())
                        .map(|&source| gate_x[source])
                        .or_else(|| input_x.get(signal.as_str()).copied())
                })
                .collect();
            let wanted = if sources.is_empty() {
                GATE_COLUMN_X
            } else {
                sources.iter().sum::<i32>() / sources.len() as i32
            };
            gate_x[gate] = claim_column(taken.entry((row, 0)).or_default(), wanted);
        }
    }

    let mut anchors = Vec::with_capacity(netlist.gates.len() + netlist.inputs.len());

    for (index, gate) in netlist.gates.iter().enumerate() {
        let row = deepest - depths[index];
        anchors.push(placements.get(&gate.output).unwrap_or(Anchor {
            x: gate_x[index],
            y: PLANNER_Y + gate_storey[index] * STOREY_PITCH,
            z: GROUND_ROW_Z + row as i32 * ROW_PITCH,
        }));
    }

    for input in &netlist.inputs {
        anchors.push(placements.get(input).unwrap_or(Anchor {
            x: input_x[input.as_str()],
            y: PLANNER_Y,
            z: GROUND_ROW_Z + (deepest as i32 + 1) * ROW_PITCH,
        }));
    }

    Ok(anchors)
}

/// The free column of a row nearest `wanted`, claimed.
///
/// Rows are a grid of `COLUMN_PITCH` slots; this snaps a barycentre onto one
/// and walks outwards until it finds a slot nobody has taken.
fn claim_column(taken: &mut BTreeSet<i32>, wanted: i32) -> i32 {
    let slot = ((wanted - GATE_COLUMN_X) as f64 / COLUMN_PITCH as f64).round() as i32;
    for step in 0.. {
        for candidate in [slot + step, slot - step] {
            // Never left of the first column. Walking outwards from a
            // barycentre otherwise runs off the origin -- a gate at x = -6 has
            // its blocks written outside the world, and its socket reads as
            // air however carefully the route reached it.
            if candidate < 0 {
                continue;
            }
            let x = GATE_COLUMN_X + candidate * COLUMN_PITCH;
            if taken.insert(x) {
                return x;
            }
        }
    }
    unreachable!("a row has unboundedly many columns")
}

/// How far each gate stands from the primary inputs, counted in gates.
fn gate_depths(netlist: &Netlist) -> Result<Vec<usize>, PlannerError> {
    let order = netlist
        .combinational_order()
        .ok_or(PlannerError::PhysicalInvariant(
            compile::CompileError::CyclicNetlist,
        ))?;
    let producer: BTreeMap<&str, usize> = netlist
        .gates
        .iter()
        .enumerate()
        .map(|(index, gate)| (gate.output.as_str(), index))
        .collect();

    let mut depths = vec![0usize; netlist.gates.len()];
    for &gate in &order {
        depths[gate] = netlist.gates[gate]
            .inputs
            .iter()
            .filter_map(|input| producer.get(input.as_str()))
            .map(|&source| depths[source] + 1)
            .max()
            .unwrap_or(0);
    }
    Ok(depths)
}

/// Route every net of a placed candidate, in a deterministic order.
/// How many times a net may tear up its blockers and try again before the
/// planner gives up on the whole layout.
///
/// Each round removes every route that sits where a failed net needs to go and
/// re-lays them afterwards, so a round is expensive; the point is to escape a
/// corner an earlier net walked this one into, not to search.
pub(crate) const RIP_UP_ROUNDS: usize = 64;

/// The budget [`compile::compile`] gives the planner **as a trial**, before it
/// falls back to the emitter.
///
/// # Why a smaller number, and why this one
///
/// `compile` tries the planner on every circuit, so every circuit that falls
/// back pays the trial's failure. Measured on 2026-08-16 by
/// [`tests::what_a_rip_up_budget_buys_and_what_it_costs`], `--release`, on the
/// six circuits the Stage 3 condition names:
///
/// | budget | and4 | full_adder | verilog:and4 | segment_a | seven_segment |
/// |---|---|---|---|---|---|
/// | rounds actually spent | 1 | **5** | 1 | never routes | never routes |
/// | 5 | ok 0.00s | ok 0.32s | ok 0.00s | fails 0.65s | fails 0.26s |
/// | 8 | ok 0.00s | ok 0.33s | ok 0.00s | fails **1.27s** | fails **0.33s** |
/// | 16 | ok | ok 0.32s | ok | fails 3.91s | fails 1.25s |
/// | 32 | ok | ok 0.32s | ok | fails 10.88s | fails 4.40s |
/// | 64 (`RIP_UP_ROUNDS`) | ok | ok 0.34s | ok | fails **36.71s** | fails **20.97s** |
///
/// Two things decide the value. Every circuit that routes at all routes within
/// **five** rounds, and its cost is flat from there up -- the loop returns on
/// the first round that finishes, so a bigger budget buys those circuits
/// nothing and changes their answer not at all. And a circuit that never routes
/// spends the whole budget, at a cost that grows faster than the budget does,
/// because each round starts from a more congested map than the last.
///
/// So the budget is the smallest power of two strictly above the worst
/// measured need: **8**, five rounds plus three of headroom. It admits every
/// circuit the full budget admits, bit for bit, and cuts what the three
/// fallbacks pay from 57.7s to 1.6s.
///
/// # What it costs, stated rather than hidden
///
/// A circuit that would have routed at round 9 through 64 now falls back
/// instead. No such circuit is known -- nothing in this tree has ever routed
/// past round 5, and the 64-round runs above end with the same failure the
/// 8-round runs do -- but "not known" is not "cannot exist", and raising this
/// is a one-line change whose cost the table above prices.
///
/// [`compile_planned`](compile::compile_planned) is deliberately *not* bounded
/// this way: it has no fallback, so a caller asking for the planner
/// specifically gets the router trying as hard as it can.
pub(crate) const TRIAL_RIP_UP_ROUNDS: usize = 8;

fn route_every_net(
    candidate: PlanCandidate,
    netlist: &Netlist,
    rip_up_rounds: usize,
) -> Result<PlanCandidate, PlannerError> {
    let mut order: Vec<String> = net_sinks(netlist).into_keys().collect();
    let mut congestion = Congestion::default();
    let mut last: Option<PlannerError> = None;

    for _ in 0..rip_up_rounds {
        match route_in_order(candidate.clone(), netlist, &order, &congestion) {
            Ok(routed) => return Ok(routed),
            Err(failure) => {
                let RoutingFailure {
                    blocked,
                    corridor,
                    reservation,
                    charge_outright,
                    error,
                } = *failure;
                last = Some(error);
                let charged_air = congestion.charge_cells(&charge_outright);

                // Two levers, and both are needed. Going first gets a net the
                // corridor it wants when one exists; charging what stood in
                // its way makes the others give it one when it does not.
                // Ordering alone left full_adder unroutable, and charging
                // alone left it worse -- a net promoted to the front still
                // walks into a wall nobody has any reason to avoid.
                let charged = congestion.charge(&reservation, corridor.0, corridor.1, &blocked);
                let mut promoted: Vec<String> = vec![blocked.clone()];
                promoted.extend(order.iter().filter(|name| **name != blocked).cloned());
                let reordered = promoted != order;
                order = promoted;

                if !charged && !reordered && !charged_air {
                    break;
                }
            }
        }
    }

    Err(last.expect("a failed round always records why"))
}

/// Which sinks every signal drives, in a deterministic order.
fn net_sinks(netlist: &Netlist) -> BTreeMap<String, Vec<(usize, usize)>> {
    let mut sinks: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for (gate, definition) in netlist.gates.iter().enumerate() {
        for (input_index, input) in definition.inputs.iter().enumerate() {
            sinks
                .entry(input.clone())
                .or_default()
                .push((gate, input_index));
        }
    }
    sinks
}

/// A net that could not be routed, and who was standing in its way.
struct RoutingFailure {
    blocked: String,
    /// The box the search gave up crossing: from the net's source to the sink
    /// it could not reach.
    corridor: (Anchor, Anchor),
    /// What the round had claimed by the time it gave up -- who was in the
    /// way, in other words.
    reservation: Reservation,
    /// Cells to charge outright, whoever owns them.
    ///
    /// A route that decays to nothing was not blocked by anybody: it climbed,
    /// and a climb has nowhere to put a refresh. Charging the corridor asks
    /// other nets to move, which is no answer -- what has to become expensive
    /// is the height this net chose, so the next round buys a flatter path.
    charge_outright: Vec<Anchor>,
    error: PlannerError,
}

/// Claim, for the net the netlist says drives it, the one cell every socket
/// can be entered from.
///
/// Every socket has exactly one cell a signal can enter it from -- the one
/// collinear with socket and support, because a terminal only reads from
/// directly behind itself. Which net will use it is on the netlist, so it is
/// claimed for that net now. Left free, it goes to whichever route is laid
/// first, and the net that actually needs it can never reach its own gate:
/// that is what made seven_segment unroutable, and no amount of spare room
/// above the plane fixes it, because there is no second way in.
///
/// **FORBIDDEN, not priced.** A socket approach is a property of where the
/// gate stands, not of which corridor a net chose, so no amount of negotiation
/// can move it and a net that took a stranger's would be unroutable rather
/// than expensive. [`route_negotiated`] keeps this claim hard for exactly that
/// reason -- it is one of the four constraints
/// `docs/superpowers/specs/2026-08-15-routing-at-scale.md` §5.1 names as
/// dropped by the PathFinder probe and required back.
fn preclaim_socket_approaches(
    reservation: &mut Reservation,
    candidate: &PlanCandidate,
    netlist: &Netlist,
) {
    for (gate, definition) in netlist.gates.iter().enumerate() {
        let support = candidate.anchors[gate];
        let facing = candidate.facing_of(gate);
        for (input_index, driver) in definition.inputs.iter().enumerate() {
            let socket = step(support, compile::geometry::input_directions(facing)[input_index]);
            let approach = Anchor {
                x: socket.x + (socket.x - support.x),
                y: socket.y + (socket.y - support.y),
                z: socket.z + (socket.z - support.z),
            };
            reservation.insert(approach, driver, Occupancy::Wire);
        }
    }
}

/// Where `signal` is driven from, or the failure that says nothing drives it.
fn net_source(candidate: &PlanCandidate, signal: &str) -> Result<Anchor, Box<RoutingFailure>> {
    candidate
        .primitive_nodes
        .iter()
        .find(|node| node.id == format!("gate:{signal}") || node.id == format!("input:{signal}"))
        .map(|node| node.source())
        .ok_or_else(|| {
            Box::new(RoutingFailure {
                blocked: signal.to_string(),
                corridor: (Anchor { x: 0, y: 0, z: 0 }, Anchor { x: 0, y: 0, z: 0 }),
                reservation: Reservation::new(),
                charge_outright: Vec::new(),
                error: PlannerError::UnrealisableNode {
                    id: signal.to_string(),
                    reason: "no gate or primary input drives this signal".to_string(),
                },
            })
        })
}

/// THE RING RULE (2026-08-19): a repeater in `route` whose output can reach
/// its own input cell through this route's own realised cells, or `None` when
/// the route is electrically a tree.
///
/// `emit` places every repeater pre-lit, so a route that feeds a repeater's
/// output back into that repeater's input is a latch: it comes up powered and
/// stays powered at every input vector, which is a different circuit, not a
/// dear one. Measured before this rule existed: negotiated `segment_a` at
/// `PresentSchedule::starting_at(8)` laid **nine** such rings across six of
/// its 47 nets -- g0's repeater at (93,4,110) closing through six of its own
/// cells is the diagnosed one -- and the latch oracle (every torch deleted,
/// every lever off, the real `Simulator` settled) read 751 cells still
/// powered on that plan against zero on every other buildable plan. A ring is
/// game physics, so it is a **refusal** in [`lay_net`] under both routers,
/// never a price.
///
/// The edges walked are the measured physics, not plain adjacency:
///
/// * dust to dust on the same level -- joined unconditionally
///   (`docs/derived/dust-join-relation.md`, same-layer arm);
/// * dust to dust one level up or down across a horizontal step -- joined
///   unless the pair's lid is committed stone. The closed form's step
///   conjunct holds whenever the upper cell exists at all, because
///   realisation lays a stone floor under every routed cell; the lid is
///   consulted in the reservation, and a lid that is one of this route's own
///   anchors counts as open, because a later anchor over an earlier floor
///   stays dust in the world while the books say `Stone`
///   ([`Occupancy::Stone`]'s doc);
/// * dust into the input side of a repeater directly behind it, and a
///   repeater into its output cell -- the diode's two edges, one-way.
///
/// The strongly-powered-block ramp (a repeater aiming into stone drives dust
/// on every face of that stone) adds no own-cell edge at plan time: a trunk
/// repeater aims into the next routed cell, which is dust, and a terminal
/// repeater aims into a gate support whose sides `keep_out` refuses to this
/// net's own wire -- so it is not walked here.
fn ring_closed_in(route: &Route, reservation: &Reservation) -> Option<(Anchor, BTreeSet<Anchor>)> {
    use crate::redstone::world::block::BlockKind;

    let kind_of: BTreeMap<Anchor, &BlockState> = route
        .anchors
        .iter()
        .copied()
        .zip(route.realisation.iter())
        .collect();

    let sealed = |lid: Anchor| -> bool {
        !kind_of.contains_key(&lid) && reservation.stone_owner(&lid).is_some()
    };
    let repeater_input = |cell: Anchor, state: &BlockState| -> Option<Anchor> {
        (state.kind == BlockKind::Repeater)
            .then(|| state.facing.map(|facing| step(cell, facing)))
            .flatten()
    };

    let steps_from = |cell: Anchor| -> Vec<Anchor> {
        let mut out = Vec::new();
        let Some(state) = kind_of.get(&cell) else {
            return out;
        };
        match state.kind {
            BlockKind::RedstoneWire => {
                for beside in horizontal_neighbours(cell) {
                    if let Some(neighbour) = kind_of.get(&beside) {
                        match neighbour.kind {
                            BlockKind::RedstoneWire => out.push(beside),
                            BlockKind::Repeater
                                if repeater_input(beside, neighbour) == Some(cell) =>
                            {
                                out.push(beside);
                            }
                            _ => {}
                        }
                    }
                    let up = Anchor { y: beside.y + 1, ..beside };
                    if kind_of
                        .get(&up)
                        .is_some_and(|block| block.kind == BlockKind::RedstoneWire)
                        && !sealed(Anchor { y: cell.y + 1, ..cell })
                    {
                        out.push(up);
                    }
                    let down = Anchor { y: beside.y - 1, ..beside };
                    if kind_of
                        .get(&down)
                        .is_some_and(|block| block.kind == BlockKind::RedstoneWire)
                        && !sealed(beside)
                    {
                        out.push(down);
                    }
                }
            }
            BlockKind::Repeater => {
                if let Some(facing) = state.facing {
                    let output = step(cell, facing.opposite());
                    if let Some(next) = kind_of.get(&output) {
                        match next.kind {
                            BlockKind::RedstoneWire => out.push(output),
                            BlockKind::Repeater if repeater_input(output, next) == Some(cell) => {
                                out.push(output);
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        out
    };

    for (&cell, state) in &kind_of {
        let Some(input) = repeater_input(cell, state) else {
            continue;
        };
        if !kind_of.contains_key(&input) {
            continue;
        }
        let Some(facing) = state.facing else { continue };
        let output = step(cell, facing.opposite());
        if !kind_of.contains_key(&output) {
            continue;
        }
        // Flood from the output through this route's own cells, never through
        // the repeater itself. Reaching its own input closes a ring.
        let mut seen: BTreeSet<Anchor> = BTreeSet::from([cell]);
        let mut frontier = vec![output];
        while let Some(at) = frontier.pop() {
            if !seen.insert(at) {
                continue;
            }
            if at == input {
                return Some((cell, seen));
            }
            frontier.extend(steps_from(at).into_iter().filter(|next| !seen.contains(next)));
        }
    }
    None
}

/// Lay one net: a branch per consumer, each searched from the net's source,
/// each sharing whatever prefix an earlier branch already laid.
///
/// **This is the whole of what a router does to a net, and both routers call
/// it unchanged.** It was [`route_in_order`]'s inner loop and is still line for
/// line that loop; what a router chooses is only
///
/// * what `reservation` holds when it is called -- [`route_in_order`] passes
///   the one reservation every net shares, so a foreign net's cells are
///   refusals; [`route_negotiated`] passes a reservation holding the hard
///   furniture and this net's own cells only, so a foreign net's cells are
///   invisible here and priced instead; and
/// * what `prices` charges.
///
/// Everything else -- the strength budget, the staircase guard, the terminal
/// style, the terminal guard cells, `self_obstructs` -- is the same code
/// running under both, which is the point: a difference between the two
/// routers can only come from those two arguments.
#[allow(clippy::too_many_arguments)]
fn lay_net(
    signal: &str,
    source: Anchor,
    consumers: &[(usize, usize)],
    netlist: &Netlist,
    candidate: &PlanCandidate,
    reservation: &mut Reservation,
    prices: &Prices,
) -> Result<Route, Box<RoutingFailure>> {
    let signal = signal.to_string();
    let mut route = Route::new(signal.clone(), Vec::new());
    route.owner = Some(signal.clone());
    for &(gate, input_index) in consumers {
        let support = candidate.anchors[gate];
        let facing = candidate.facing_of(gate);
        let socket = step(support, compile::geometry::input_directions(facing)[input_index]);
        // A terminal component only drives the support it faces, and only
        // reads from directly behind itself, so the last step into the
        // socket has to be collinear with socket -> support. The legacy
        // router guarantees that with a dedicated approach column; here
        // the search is aimed one cell further out and the socket is
        // appended, which is the same guarantee stated as geometry.
        //
        // `try_move` asks the same question the same way since Task 10:
        // `route_endpoints` threads each branch's `RouteSink` out and
        // `declared_socket` reads `input_directions(facing)[input_index]`
        // off it. The geometric guess `terminal_socket` makes survives only
        // for a route whose sink the netlist never declared.
        let approach = Anchor {
            x: socket.x + (socket.x - support.x),
            y: socket.y + (socket.y - support.y),
            z: socket.z + (socket.z - support.z),
        };
        let mut path = match deterministic_astar(
            source,
            approach,
            socket,
            &signal,
            reservation,
            prices,
        ) {
            Some(path) => path,
            None => {
                return Err(Box::new(RoutingFailure {
                    blocked: signal.clone(),
                    corridor: (source, approach),
                    reservation: reservation.clone(),
                    charge_outright: Vec::new(),
                    error: PlannerError::NoLocalRoute {
                        from: source,
                        to: approach,
                    },
                }));
            }
        };
        path.push(socket);
        reserve_path(reservation, &signal, &path);

        // How much of this branch the trunk already laid, and what the
        // signal is worth by the time it gets there. Planning the whole
        // path from full strength would put refreshes on trunk cells that
        // keep the first branch's blocks, so they would be planned and
        // never built.
        let shared = path
            .iter()
            .take_while(|anchor| route.anchors.contains(anchor))
            .count();
        let mut carried = crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
        let mut previous_cell = source;
        let mut trunk_repeaters = 0u64;
        for anchor in &path[..shared] {
            let index = route
                .anchors
                .iter()
                .position(|laid| laid == anchor)
                .expect("the shared prefix is by definition already laid");
            if route.realisation[index].kind
                == crate::redstone::world::block::BlockKind::Repeater
            {
                carried = crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
                trunk_repeaters += 1;
            } else {
                carried = carried.saturating_sub(1);
            }
            previous_cell = *anchor;
        }

        let laid = realise_branch_from(previous_cell, carried, &path[shared..]);
        if !laid.carries {
            return Err(Box::new(RoutingFailure {
                blocked: signal.clone(),
                corridor: (source, approach),
                reservation: reservation.clone(),
                // Every cell this branch put above the gate plane. Nothing
                // else is at fault: the climb is what left the refreshes
                // nowhere to stand.
                charge_outright: path
                    .iter()
                    .copied()
                    .filter(|cell| cell.y > PLANNER_Y)
                    .collect(),
                error: PlannerError::PhysicalInvariant(
                    compile::CompileError::CandidateMetadataViolation {
                        item: signal.clone(),
                        reason: format!(
                            "the route to {}.in[{input_index}] decays to nothing before it \
                             arrives, and no cell along it can hold a refresh",
                            netlist.gates[gate].output
                        ),
                    },
                ),
            }));
        }
        // Whether the strength budget already needs the socket cell to be
        // a refresh. If it does, no terminal-style preference may take it
        // away: dust that cannot reach is not a cheaper terminal, it is a
        // dead one.
        let budget_needs_repeater = laid
            .blocks
            .last()
            .is_some_and(|block| block.kind == crate::redstone::world::block::BlockKind::Repeater);
        for ((anchor, block), floor) in path[shared..].iter().zip(laid.blocks).zip(laid.floors)
        {
            if route.anchors.contains(anchor) {
                continue;
            }
            route.anchors.push(*anchor);
            route.realisation.push(block);
            route.floors.push(floor);
        }

        let predecessor = path
            .get(path.len().saturating_sub(2))
            .copied()
            .unwrap_or(source);

        // A branch whose every sink is the same wire merge joins that
        // merge's own dust, not a gate's support block: dust meets dust
        // and nothing has to drive anything. The same condition
        // `merge_branch_is_bare` states, read off the netlist.
        let bare_merge = netlist.gates[gate].is_merge()
            && consumers.iter().all(|&(sink, _)| sink == gate);

        let kind = if bare_merge {
            RouteTerminalKind::BareMergeDust
        } else {
            let style = if budget_needs_repeater {
                TerminalStyle::RepeaterIntoSupport
            } else {
                terminal_style(&TerminalApproach::new(
                    predecessor,
                    socket,
                    support,
                    laid.strength_before_terminal,
                    terminal_is_isolated(reservation, &signal, predecessor, socket, support),
                ))
            };
            if let Some(index) = route.anchors.iter().position(|anchor| *anchor == socket) {
                route.realisation[index] = match style {
                    TerminalStyle::RepeaterIntoSupport => compile::repeater(
                        compile::direction_from(
                            Position::new(predecessor.x, predecessor.y, predecessor.z),
                            Position::new(socket.x, socket.y, socket.z),
                        ),
                    ),
                    TerminalStyle::DirectedDustIntoSupport => compile::dust(),
                };
            }
            style.into()
        };

        // A terminal has to stay a straight line into its support, and a
        // net is otherwise free to run alongside itself -- so the next
        // branch of this very route would happily pass beside this
        // terminal and turn it into a corner that drives nothing. Claim
        // the cells around it under a name nobody routes as.
        let guard = format!("terminal:{}.in[{input_index}]", netlist.gates[gate].output);
        for neighbour in horizontal_neighbours(socket) {
            if neighbour != predecessor && neighbour != support {
                reservation.insert(neighbour, &guard, Occupancy::Solid);
            }
        }


        route.terminals.push(RouteTerminal {
            sink: RouteSink {
                gate: netlist.gates[gate].output.clone(),
                input_index,
                anchor: socket,
            },
            kind,
            repeaters: trunk_repeaters + laid.repeaters,
        });

        // THE RING RULE. A branch that closes a cycle through one of this
        // route's own repeaters has built a latch, and a latch is a different
        // circuit: pre-lit repeaters (`emit` places every one lit) keep it
        // powered at every input vector. Game physics, so a refusal under
        // both routers -- see [`ring_closed_in`] for the measured case and
        // the edges walked. Checked after every branch so the branch that
        // closed the ring is the one refused and charged.
        if let Some((repeater, ring)) = ring_closed_in(&route, reservation) {
            let mut charged: Vec<Anchor> = path[shared..]
                .iter()
                .copied()
                .filter(|cell| ring.contains(cell))
                .collect();
            if charged.is_empty() {
                // The closure always involves cells this branch laid, but if
                // bookkeeping ever disagrees, charge the whole branch rather
                // than nothing: an uncharged refusal repeats forever.
                charged = path[shared..].to_vec();
            }
            return Err(Box::new(RoutingFailure {
                blocked: signal.clone(),
                corridor: (source, approach),
                reservation: reservation.clone(),
                charge_outright: charged,
                error: PlannerError::PhysicalInvariant(
                    compile::CompileError::CandidateMetadataViolation {
                        item: signal.clone(),
                        reason: format!(
                            "the branch to {}.in[{input_index}] closes a ring: the repeater \
                             at ({}, {}, {}) reaches its own input cell through this net's \
                             own cells, and a route that feeds its own repeater input is a \
                             latch, not a wire",
                            netlist.gates[gate].output, repeater.x, repeater.y, repeater.z
                        ),
                    },
                ),
            }));
        }
    }
    Ok(route)
}

fn route_in_order(
    mut candidate: PlanCandidate,
    netlist: &Netlist,
    order: &[String],
    congestion: &Congestion,
) -> Result<PlanCandidate, Box<RoutingFailure>> {
    let mut reservation = reserve_primitives(&candidate.primitive_nodes);

    let sinks = net_sinks(netlist);

    preclaim_socket_approaches(&mut reservation, &candidate, netlist);

    let prices = Prices::RipUp(congestion);
    let mut routes = Vec::with_capacity(sinks.len());
    for signal in order {
        let consumers = sinks
            .get(signal)
            .cloned()
            .expect("the order is built from these very keys");
        let source = net_source(&candidate, signal)?;
        routes.push(lay_net(
            signal,
            source,
            &consumers,
            netlist,
            &candidate,
            &mut reservation,
            &prices,
        )?);
    }

    candidate.routes = routes;
    Ok(candidate)
}

// ---------------------------------------------------------------------------
// Negotiated congestion (PathFinder)
//
// `docs/superpowers/specs/2026-08-15-routing-at-scale.md` §5.1. The rip-up loop
// above lays each net once per round and never takes one back within a round;
// `anchor_is_free_for` refuses a contested cell outright, so a blocked net
// cannot say *which* cell it needed, and the only feedback is a flat charge on
// every foreign cell in the failed corridor's bounding box -- cells that were
// never in the way included.
//
// What follows replaces the refusal with a price and the round with an
// iteration. Every net is laid every iteration against everyone else's current
// choice; a cell two nets both want is *usable* and dear rather than forbidden;
// the price of a cell rises with how many nets want it now and with how many
// iterations it has been fought over. The loop ends when no cell is contested.
//
// # What is PRICED and what is FORBIDDEN
//
// This split is the whole of the design's safety, and getting it wrong is how a
// negotiated router produces a fast, small, wrong circuit.
//
// **PRICED -- negotiable, because it is two nets wanting the same room and one
// of them can move.** Every one of these is a relation between two *routed
// nets*, and every one of them is zero in a plan this loop is allowed to
// return:
//
// * both nets want the same cell;
// * one net's wire is inside the other's `keep_out` halo;
// * one net's floor would be laid on the other's conductor, and its mirror,
//   one net's wire standing under the other's floor;
// * one net's wire, or its floor, lands in a cell the other's staircase needs
//   to stay air.
//
// **FORBIDDEN -- physics, or the plan's own commitments, and never a price.**
// These are refusals inside `lay_net` in every iteration, exactly as they are
// in the shipping router, because they are not relations between two nets and
// no amount of negotiation can settle them:
//
// * gate footprints -- a primitive cannot move (`reserve_primitives`);
// * the socket pre-claim (`preclaim_socket_approaches`);
// * the terminal guard cells (`preclaim_terminal_guards`);
// * `self_obstructs`, the search bounds, and staircase clearance -- its riser
//   and its headroom -- *against this net's own cells and against every
//   primitive* (`deterministic_astar`, unchanged);
// * a route's floor over a conductor, the stone commitment, and `keep_out`,
//   again *against this net's own cells and against every primitive*
//   (`anchor_is_free_for`, unchanged);
// * the strength budget -- `realise_branch_from`'s repeater every <= 15 cells
//   and its refresh before every climb. A branch that decays is not a dear
//   route, it is a dead one: the iteration is refused outright and the height
//   it chose is charged, which is the one thing the rip-up loop already did
//   right (`RoutingFailure::charge_outright`).
//
// The two "against this net's own cells" clauses are where the split actually
// lives, and they are exact rather than approximate: `exclusion_zone` is the
// set of cells a *foreign* net could occupy to make `anchor_is_free_for` refuse
// this one, swept cell by cell against the rule itself in
// `the_priced_zone_is_exactly_what_a_foreign_wire_makes_anchor_is_free_for_refuse`.
// Priced is exactly the complement of forbidden; neither list is a judgement
// call about which rules feel negotiable.
//
// One caveat, measured and not smoothed over: `realise_branch_from`'s `carries`
// is the router's model of the strength budget and it is **not** the same
// predicate as `compile::verify_signal_strength`. A negotiated `full_adder`
// used to pass the first and be refused by the second; that was a defect in the
// second and it is fixed -- see
// `the_strength_verifier_follows_a_repeater_that_feeds_a_climb`. The two
// predicates are still not the same one, and nothing here says they agree in
// general.
//
// The reason the priced list is safe is not that the prices get large. It is
// that a plan with any of them non-zero is **never returned**:
// `Negotiation::contested` is the loop's exit condition, and
// `negotiation_left_nothing_shared` re-derives it from the finished routes
// before the plan leaves this function.

/// How many of a net's cells bear on one cell.
type ByNet = BTreeMap<String, u32>;

/// Every cell a wire at `cell` puts beyond a *foreign* net's reach.
///
/// It is exactly the set of cells whose occupation by another net would make
/// [`anchor_is_free_for`] refuse `cell` to this one, and nothing more:
///
/// * `cell` itself -- one owner per cell;
/// * `keep_out(cell)`'s twelve -- the two-cell conductor clearance;
/// * the cell below -- this wire's floor is laid there, and a floor over a
///   conductor deletes it;
/// * the cell above -- a wire there lays *its* floor here, which is the same
///   deletion seen from the other side, and is what [`Occupancy::Stone`] and
///   `anchor_is_free_for`'s stone arm record.
///
/// **Symmetric**, and deliberately: `b` is in `exclusion_zone(a)` exactly when
/// `a` is in `exclusion_zone(b)`, which is what lets one stamp per wire cell
/// answer both "what does this net cost others" and "what do others cost this
/// net" from a single lookup at the cell being priced.
fn exclusion_zone(cell: Anchor) -> Vec<Anchor> {
    let mut cells = Vec::with_capacity(15);
    cells.push(cell);
    cells.push(Anchor { y: cell.y + 1, ..cell });
    cells.push(Anchor { y: cell.y - 1, ..cell });
    cells.extend(keep_out(cell));
    cells
}

/// What one net currently occupies, in the terms the negotiation trades in.
#[derive(Debug, Clone, Default)]
struct NetClaim {
    /// The cells that will hold this net's dust or repeaters. Terminal sockets
    /// are **not** here: a socket is a primitive's cell, every other net is
    /// already kept out of it and its halo by `reserve_primitives`, and nothing
    /// about it is negotiable.
    wire: Vec<Anchor>,
    /// The cells this net's staircases need to stay **air** -- the cell over a
    /// climber's head and the one a descent falls past. [`reserve_path`] writes
    /// them [`Occupancy::Air`] under a `stair:` owner; they are read back out
    /// of the reservation rather than re-derived, so this cannot drift from
    /// [`staircase_clearance`].
    air: Vec<Anchor>,
}

/// The negotiation's two price terms and the bookkeeping that makes overuse
/// measurable.
///
/// `shadow`, `wire` and `air` are indexed by cell so a price is one lookup;
/// `claims` is indexed by net so a net can be ripped up and re-laid without
/// rebuilding anything.
#[derive(Debug, Default)]
struct Negotiation {
    /// Cell -> the nets whose [`exclusion_zone`] covers it.
    shadow: BTreeMap<Anchor, ByNet>,
    /// Cell -> the nets whose wire is literally here.
    wire: BTreeMap<Anchor, ByNet>,
    /// Cell -> the nets whose staircase needs this cell to stay air.
    air: BTreeMap<Anchor, ByNet>,
    /// What each net claims right now.
    claims: BTreeMap<String, NetClaim>,
    /// How many iterations each cell has ended contested. **Never decays**,
    /// which is what makes a corridor that has been fought over repeatedly
    /// unattractive to everything that has an alternative.
    history: BTreeMap<Anchor, u64>,
    /// The present term for the iteration now running.
    present: u64,
}

/// The present term at iteration `k`: **zero, then doubling, capped**.
///
/// Iteration 0 is free on purpose. Every net takes the path it would take if it
/// were the only net in the circuit, so what the first iteration measures is the
/// circuit's *real* contention rather than a contention already distorted by
/// whoever happened to be laid first -- which is the whole failure of the
/// rip-up loop, and is what a schedule starting dear would reproduce. Measured:
/// with iteration 0 priced at 4, `verilog:and4` reaches zero contested cells on
/// iteration 0 without ever having contended, so nothing negotiates and the
/// answer is the rip-up router's answer, detour and all.
///
/// From there it doubles. By iteration 12 a shared cell costs 4,096, which is
/// more than any detour a circuit this size could want, so sharing has stopped
/// being a bargain and is only a last resort.
///
/// # Why the first term is a parameter and not a constant
///
/// It is the one number in this schedule that changes *which circuits route at
/// all*, and by rule 4 of this branch's ledger a cited number needs a
/// reproducible method in the tree. `segment_a` is the case: it does not
/// converge at [`PresentSchedule::SHIPPING`] and does converge at
/// `at_zero = 8`, and that difference had until now only ever been produced by
/// editing this function and reverting it, which puts the number outside the
/// tree entirely. [`PresentSchedule`] puts it back in -- and
/// [`PresentSchedule::SHIPPING`] is what every existing caller passes, so
/// nothing about what the router does today moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PresentSchedule {
    /// What one shared cell costs on iteration 0.
    at_zero: u64,
}

impl PresentSchedule {
    /// Iteration 0 free -- the schedule every shipping and default caller uses,
    /// and the one the paragraphs above describe.
    pub(crate) const SHIPPING: Self = Self { at_zero: 0 };

    /// A schedule that charges `at_zero` on iteration 0 and is otherwise
    /// identical.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn starting_at(at_zero: u64) -> Self {
        Self { at_zero }
    }

    /// The present term at iteration `k`.
    fn term(self, iteration: usize) -> u64 {
        const CAP: u64 = 4096;
        match iteration {
            0 => self.at_zero,
            k => (1u64 << (k - 1).min(20)).saturating_mul(2).min(CAP),
        }
    }
}

impl Default for PresentSchedule {
    fn default() -> Self {
        Self::SHIPPING
    }
}

/// [`PresentSchedule::SHIPPING`]'s term at `iteration`, for the reports that
/// print the schedule they ran under.
#[cfg_attr(not(test), allow(dead_code))]
fn present_term(iteration: usize) -> u64 {
    PresentSchedule::SHIPPING.term(iteration)
}

/// What one iteration of accumulated history adds to a cell's price.
const HISTORY_WEIGHT: u64 = 8;

impl Negotiation {
    fn stamp(map: &mut BTreeMap<Anchor, ByNet>, cell: Anchor, net: &str) {
        *map.entry(cell).or_default().entry(net.to_string()).or_insert(0) += 1;
    }

    fn unstamp(map: &mut BTreeMap<Anchor, ByNet>, cell: Anchor, net: &str) {
        if let Some(by_net) = map.get_mut(&cell) {
            let spent = by_net.get_mut(net).is_some_and(|count| {
                *count -= 1;
                *count == 0
            });
            if spent {
                by_net.remove(net);
            }
            if by_net.is_empty() {
                map.remove(&cell);
            }
        }
    }

    fn foreign(map: &BTreeMap<Anchor, ByNet>, cell: &Anchor, mine: &str) -> u64 {
        map.get(cell)
            .map(|by_net| by_net.keys().filter(|net| net.as_str() != mine).count() as u64)
            .unwrap_or(0)
    }

    /// Take `net`'s cells back out of the shared occupancy, so the search that
    /// re-lays it prices everyone else's choices and not its own last one.
    ///
    /// This is the rip-up half of "every net is routed every iteration", and it
    /// happens per net rather than per iteration on purpose: a net laid late in
    /// an iteration then sees what the nets laid before it did *this* time
    /// round, not last.
    fn release(&mut self, net: &str) {
        let Some(claim) = self.claims.remove(net) else {
            return;
        };
        for cell in &claim.wire {
            Self::unstamp(&mut self.wire, *cell, net);
            for zone in exclusion_zone(*cell) {
                Self::unstamp(&mut self.shadow, zone, net);
            }
        }
        for cell in &claim.air {
            Self::unstamp(&mut self.air, *cell, net);
        }
    }

    fn claim(&mut self, net: &str, claim: NetClaim) {
        for cell in &claim.wire {
            Self::stamp(&mut self.wire, *cell, net);
            for zone in exclusion_zone(*cell) {
                Self::stamp(&mut self.shadow, zone, net);
            }
        }
        for cell in &claim.air {
            Self::stamp(&mut self.air, *cell, net);
        }
        self.claims.insert(net.to_string(), claim);
    }

    /// What laying `mine`'s wire in `cell` costs beyond the step onto it.
    ///
    /// Two terms, and both are needed. The **present** term is what makes the
    /// nets separate within an iteration -- it counts who wants this cell now,
    /// so the net with an alternative moves and the net without one keeps the
    /// cell. The **history** term is what stops them swapping places forever: a
    /// cell that has been contested before stays dear even in an iteration
    /// where it happens to be free, which is the difference between a
    /// negotiation and an oscillation.
    fn price(&self, cell: &Anchor, mine: &str) -> u64 {
        let history = self.history.get(cell).copied().unwrap_or(0);
        // Someone else's wire, or its halo, or its floor, is here.
        let mut contenders = Self::foreign(&self.shadow, cell, mine);
        // Someone else's staircase needs this cell to stay air -- and the cell
        // below it, because this wire's own floor would fill that one.
        contenders += Self::foreign(&self.air, cell, mine);
        contenders += Self::foreign(&self.air, &Anchor { y: cell.y - 1, ..*cell }, mine);
        history
            .saturating_mul(HISTORY_WEIGHT)
            .saturating_add(contenders.saturating_mul(self.present))
    }

    /// Every cell two nets want at once.
    ///
    /// **This is the loop's exit condition and the definition of a legal
    /// plan.** Empty means no pair of nets shares a cell, stands inside the
    /// other's `keep_out`, would lay a floor over the other's conductor, or
    /// would fill a cell the other's staircase needs empty.
    fn contested(&self) -> BTreeSet<Anchor> {
        let mut out = BTreeSet::new();
        for (net, claim) in &self.claims {
            for cell in &claim.wire {
                let floor = Anchor { y: cell.y - 1, ..*cell };
                if Self::foreign(&self.shadow, cell, net) > 0
                    || Self::foreign(&self.air, cell, net) > 0
                    || Self::foreign(&self.air, &floor, net) > 0
                {
                    out.insert(*cell);
                }
            }
            for cell in &claim.air {
                let lid = Anchor { y: cell.y + 1, ..*cell };
                if Self::foreign(&self.wire, cell, net) > 0
                    || Self::foreign(&self.wire, &lid, net) > 0
                {
                    out.insert(*cell);
                }
            }
        }
        out
    }

    fn charge_history(&mut self, cells: impl IntoIterator<Item = Anchor>) {
        for cell in cells {
            *self.history.entry(cell).or_insert(0) += 1;
        }
    }
}

/// The furniture no negotiation can move: gate footprints, the socket
/// pre-claims, and the terminal guard cells.
///
/// Every iteration starts from this and every net sees it whole. See the
/// FORBIDDEN list at the head of this section.
fn hard_furniture(candidate: &PlanCandidate, netlist: &Netlist) -> Reservation {
    let mut reservation = reserve_primitives(&candidate.primitive_nodes);
    preclaim_socket_approaches(&mut reservation, candidate, netlist);
    preclaim_terminal_guards(&mut reservation, candidate, netlist);
    reservation
}

/// Claim the cells beside every socket that have to stay clear of conductors.
///
/// A terminal has to be a straight line into its support, so a conductor beside
/// it other than the cell it is entered from and the support it drives turns it
/// into a corner that drives nothing. [`lay_net`] claims these one branch at a
/// time, as the shipping router always has; this claims all of them before any
/// net is laid.
///
/// **The difference is deliberate and it is a tightening.** A guard cell is
/// `horizontal_neighbours(socket)` less the approach and the support, and all
/// three of those are fixed by where the gate stands -- so a guard cell is
/// furniture, and which net gets it is not a thing to negotiate about. Claiming
/// them as they are laid leaves the answer depending on net order, and under
/// negotiation the order is not a lever any more.
fn preclaim_terminal_guards(
    reservation: &mut Reservation,
    candidate: &PlanCandidate,
    netlist: &Netlist,
) {
    for (gate, definition) in netlist.gates.iter().enumerate() {
        let support = candidate.anchors[gate];
        let facing = candidate.facing_of(gate);
        for input_index in 0..definition.inputs.len() {
            let socket = step(support, compile::geometry::input_directions(facing)[input_index]);
            let approach = Anchor {
                x: socket.x + (socket.x - support.x),
                y: socket.y + (socket.y - support.y),
                z: socket.z + (socket.z - support.z),
            };
            let guard = format!("terminal:{}.in[{input_index}]", definition.output);
            for neighbour in horizontal_neighbours(socket) {
                if neighbour != approach && neighbour != support {
                    reservation.insert(neighbour, &guard, Occupancy::Solid);
                }
            }
        }
    }
}

/// What a net just laid, read back out of the reservation it laid it into.
///
/// Nothing here re-derives a rule: the wire cells are the route's own anchors
/// less its sockets, and the mandatory-air cells are whatever [`reserve_path`]
/// wrote under this net's `stair:` guard as [`Occupancy::Air`] -- which is
/// [`staircase_clearance`]'s answer, recorded by the same function the router
/// uses.
fn claim_of(net: &str, route: &Route, reservation: &Reservation) -> NetClaim {
    let sockets: BTreeSet<Anchor> = route
        .terminals
        .iter()
        .map(|terminal| terminal.sink.anchor)
        .collect();
    NetClaim {
        wire: route
            .anchors
            .iter()
            .copied()
            .filter(|anchor| !sockets.contains(anchor))
            .collect(),
        air: reservation.mandatory_air_of(&stair_guard(net)),
    }
}

/// How many iterations [`route_negotiated`] gets.
///
/// The probe this design comes from converged `segment_a` at iteration 7 and
/// `full_adder` at 2 (spec §5.1). This is the smallest power of two comfortably
/// above that with room for a circuit that needs the present term to reach its
/// cap, which takes ten iterations on [`present_term`]'s schedule.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const NEGOTIATION_ROUNDS: usize = 32;

/// Lay every net every iteration, letting them share, until none of them do.
///
/// The loop, in full:
///
/// 1. Every net is ripped up and re-laid, in a fixed order, against a
///    reservation holding the hard furniture and *its own* cells only. Foreign
///    nets are not in that reservation at all, so nothing about them can refuse
///    a cell; they are in [`Negotiation`] instead, as a price.
/// 2. A cell two nets ended up wanting is charged to `history`, permanently.
/// 3. The present term doubles.
/// 4. When an iteration ends with **no** contested cell and every net laid, the
///    plan is checked once more from its own routes and returned.
///
/// **A plan in which any cell is shared is illegal and is never returned.**
/// Sharing is a tool this search uses between step 1 and step 4 and nothing
/// else; if the budget runs out with cells still contested, this fails in
/// exactly the way the router fails today -- an error, not a plan.
fn route_negotiated(
    candidate: PlanCandidate,
    netlist: &Netlist,
    iterations: usize,
    schedule: PresentSchedule,
) -> Result<PlanCandidate, PlannerError> {
    negotiate(candidate, netlist, iterations, schedule, &mut Vec::new())
}

/// What one iteration of [`negotiate`] ended with.
///
/// The convergence sequence is the only thing that says whether a negotiation
/// is negotiating or oscillating, and by rule 4 of this branch's ledger a cited
/// number needs a reproducible method in the tree -- so the loop records it
/// rather than a probe re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NegotiationRound {
    /// Cells two nets both wanted at the end of the iteration. Zero is the exit
    /// condition.
    pub contested: usize,
    /// Nets that could not be laid at all -- no path, or a path the strength
    /// budget refused.
    pub unlaid: usize,
    /// Wire cells laid, over every net that was laid.
    pub cells: usize,
}

/// [`route_negotiated`], recording what each iteration ended with.
fn negotiate(
    mut candidate: PlanCandidate,
    netlist: &Netlist,
    iterations: usize,
    schedule: PresentSchedule,
    trace: &mut Vec<NegotiationRound>,
) -> Result<PlanCandidate, PlannerError> {
    let sinks = net_sinks(netlist);
    let order: Vec<String> = sinks.keys().cloned().collect();
    let hard = hard_furniture(&candidate, netlist);

    let mut table = Negotiation::default();
    let mut last: Option<PlannerError> = None;

    for iteration in 0..iterations {
        table.present = schedule.term(iteration);
        let mut laid: BTreeMap<String, Route> = BTreeMap::new();
        let mut every_net_laid = true;

        for signal in &order {
            // Rip up before re-laying: what this net sees is everyone else's
            // current choice, never its own last one.
            table.release(signal);
            let source = net_source(&candidate, signal).map_err(|failure| failure.error)?;
            let consumers = sinks
                .get(signal)
                .cloned()
                .expect("the order is built from these very keys");
            let mut reservation = hard.clone();
            let outcome = {
                let prices = Prices::Negotiated {
                    table: &table,
                    mine: signal,
                };
                lay_net(
                    signal,
                    source,
                    &consumers,
                    netlist,
                    &candidate,
                    &mut reservation,
                    &prices,
                )
            };
            match outcome {
                Ok(route) => {
                    table.claim(signal, claim_of(signal, &route, &reservation));
                    laid.insert(signal.clone(), route);
                }
                Err(failure) => {
                    // The strength budget is FORBIDDEN, not priced, so this is
                    // not a contested cell and charging the neighbours would ask
                    // the wrong nets to move. What has to become expensive is
                    // the height this branch chose, which is the one thing the
                    // rip-up loop already got right.
                    every_net_laid = false;
                    table.charge_history(failure.charge_outright.iter().copied());
                    last = Some(failure.error);
                }
            }
        }

        let contested = table.contested();
        trace.push(NegotiationRound {
            contested: contested.len(),
            unlaid: order.len() - laid.len(),
            cells: laid.values().map(|route| route.anchors.len()).sum(),
        });
        if every_net_laid && contested.is_empty() {
            candidate.routes = order
                .iter()
                .map(|signal| laid.remove(signal).expect("every net was laid"))
                .collect();
            // Unreachable unless the incremental bookkeeping and the sweep
            // disagree, which is the one thing the sweep exists to catch --
            // and it is reported rather than returned, because an illegal plan
            // never leaves this function.
            negotiation_left_nothing_shared(&candidate)?;
            return Ok(candidate);
        }
        table.charge_history(contested);
    }

    Err(last.unwrap_or(PlannerError::UnrealisableNode {
        id: "netlist".to_string(),
        reason: format!("negotiation did not separate the nets within {iterations} iterations"),
    }))
}

/// Prove, from the finished routes alone, that no two nets share anything.
///
/// The loop's own [`Negotiation::contested`] is incremental -- stamps added and
/// removed as nets are ripped up and re-laid -- so it is exactly the sort of
/// bookkeeping that can be wrong in a way no assertion inside it would notice.
/// This re-derives the whole relation from the plan that is about to be
/// returned, and adds [`verify_spacing`]'s literal test on top: one cell, one
/// net.
///
/// It does **not** re-check the FORBIDDEN list. Those were refusals inside
/// [`lay_net`] in the iteration that produced these routes, under the same
/// [`anchor_is_free_for`] the shipping router uses, so a route that violated one
/// could not have been laid. What negotiation is responsible for -- and the only
/// thing a price could ever have let through -- is inter-net contention, and
/// that is what this proves absent.
fn negotiation_left_nothing_shared(candidate: &PlanCandidate) -> Result<(), PlannerError> {
    verify_spacing(candidate)?;

    let sockets: BTreeSet<Anchor> = candidate
        .routes
        .iter()
        .flat_map(|route| route.terminals.iter().map(|terminal| terminal.sink.anchor))
        .collect();
    let mut claimed: BTreeMap<Anchor, String> = BTreeMap::new();
    for route in &candidate.routes {
        for anchor in &route.anchors {
            if sockets.contains(anchor) {
                continue;
            }
            for cell in exclusion_zone(*anchor) {
                if let Some(other) = claimed.get(&cell) {
                    if other != &route.id {
                        return Err(PlannerError::PhysicalInvariant(
                            compile::CompileError::SpacingViolation {
                                cell: (cell.x, cell.y, cell.z),
                                expected_net: other.clone(),
                                found_net: Some(route.id.clone()),
                            },
                        ));
                    }
                }
            }
        }
        // The stamp goes down only after every one of this net's cells has been
        // checked against what other nets stamped, because a net's own cells are
        // inside each other's zones by construction and are not a contention.
        for anchor in &route.anchors {
            if sockets.contains(anchor) {
                continue;
            }
            claimed.insert(*anchor, route.id.clone());
        }
    }
    Ok(())
}

/// Which router lays the nets.
///
/// **Both are in the tree and only one of them ships.** [`SHIPPING_ROUTER`] is
/// [`RouterKind::RipUp`], so `compile()` and [`plan_from_netlist`] behave
/// exactly as they did before negotiation existed -- pinned by
/// `the_hand_written_circuits_keep_their_measured_size`, which does not move
/// while that constant says `RipUp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RouterKind {
    /// One pass per net, no take-backs within a pass, and a rip-up loop whose
    /// only feedback is a flat bounding-box charge. What has always shipped.
    #[default]
    RipUp,
    /// [`route_negotiated`]. Not shipping in this commit -- nothing outside a
    /// test constructs this, which is what `cfg_attr` below is for and what
    /// `the_shipping_router_is_the_rip_up_one_and_the_two_do_not_agree` pins.
    #[cfg_attr(not(test), allow(dead_code))]
    Negotiated,
}

/// One step from `anchor` towards `facing`.
fn step(anchor: Anchor, facing: crate::redstone::world::block::Facing) -> Anchor {
    let position = Position::new(anchor.x, anchor.y, anchor.z).offset(facing);
    Anchor {
        x: position.x,
        y: position.y,
        z: position.z,
    }
}

/// Extract a planner seed from the legacy emitter's explicit metadata.
///
/// This intentionally never inspects world blocks to guess route ownership:
/// `emit` recorded each primitive anchor, route owner and terminal decision at
/// the time it made the corresponding placement decision.
pub fn seed_from_legacy(
    netlist: &Netlist,
    compiled: &CompiledCircuit,
) -> Result<PlanCandidate, PlannerError> {
    let emission = compiled
        .legacy_emission()
        .ok_or(PlannerError::LegacyMetadataUnavailable)?;
    seed_from_legacy_parts(netlist, emission)
}

/// Extract a seed straight from emission metadata, before any
/// `CompiledCircuit` exists to hold it -- which is the situation `compile`
/// itself is in, since the circuit it returns is built *from* this seed.
pub(crate) fn seed_from_legacy_parts(
    netlist: &Netlist,
    emission: &LegacyEmission,
) -> Result<PlanCandidate, PlannerError> {
    if emission.netlist() != netlist {
        return Err(PlannerError::NetlistDoesNotMatchCompiledOutput);
    }

    let routes = emission
        .routes()
        .iter()
        .map(|route| {
            Route::from_legacy(
                route.owner().to_string(),
                route.anchors().to_vec(),
                route.terminals().to_vec(),
                route.blocks().to_vec(),
                route.floors().to_vec(),
            )
        })
        .collect();

    Ok(PlanCandidate::from_legacy(
        emission.primitive_anchors().to_vec(),
        emission.primitive_nodes().to_vec(),
        routes,
        emission.clone(),
    ))
}

/// Realise a candidate and run the four physical invariants against what it
/// actually built.
///
/// The candidate is the only input: no legacy compile is re-run, nothing is
/// compared against a freshly extracted seed, and no retained emission is
/// consulted.  That is the whole point -- a candidate the planner moved has
/// no legacy counterpart to be equal to, so equality was never a verification
/// strategy, only a way of checking that a seed had survived extraction.
///
/// Spacing is checked on the plan, before a block exists; the other three
/// scan the realised world.
pub fn verify_candidate(candidate: &PlanCandidate, netlist: &Netlist) -> Result<(), PlannerError> {
    realise_and_verify(candidate, netlist, candidate_world_size(candidate)).map(|_| ())
}

/// Realise a candidate at a chosen world size and verify what came out,
/// returning it.
///
/// `compile` uses this so the circuit it ships is the one the invariants
/// passed, rather than a second world built separately from the same plan.
pub fn realise_and_verify(
    candidate: &PlanCandidate,
    netlist: &Netlist,
    size: (i32, i32, i32),
) -> Result<RealisedCandidate, PlannerError> {
    verified_parts(candidate, netlist, size).map(|(realised, _reservation, _nets)| realised)
}

/// Everything [`realise_and_verify`] proved, including the two things it
/// normally consumes and drops: the ownership `verify_spacing` established and
/// the nets rebuilt from the candidate's own routes.
///
/// This exists so a **measurement** can ask its question against exactly what
/// the four invariants ran against, rather than against a second reservation
/// derived some other way -- which for this project would mean guessing
/// ownership by scanning block kinds, the one thing `Footprint`'s own doc
/// comment says must never happen. `compile::coupling` is the only consumer, and
/// like it this is `#[cfg(test)]`: what ships is [`realise_and_verify`],
/// unchanged in behaviour and now one line long.
#[cfg(test)]
pub(crate) struct VerifiedRealisation {
    pub realised: RealisedCandidate,
    pub reservation: compile::Reservation,
    pub nets: Vec<compile::Net>,
}

/// [`realise_and_verify`], keeping the ownership and nets it proved.
#[cfg(test)]
pub(crate) fn verify_and_expose(
    candidate: &PlanCandidate,
    netlist: &Netlist,
    size: (i32, i32, i32),
) -> Result<VerifiedRealisation, PlannerError> {
    verified_parts(candidate, netlist, size).map(|(realised, reservation, nets)| {
        VerifiedRealisation {
            realised,
            reservation,
            nets,
        }
    })
}

/// Exactly what [`verify_and_expose`] returns, produced **without running the
/// world-scanning invariants at all**.
///
/// [`verify_and_expose`] can only answer for a plan that already passes, which
/// makes it useless for the one question a *differential* has to ask: what does
/// the judge claim about a plan it refuses? This runs the two things the
/// invariants are given -- `verify_spacing`'s reservation and
/// `verification_nets`' nets -- and realises the world, and stops there. Every
/// caller is a measurement; `#[cfg(test)]` is what holds that to being a
/// property of the build rather than a promise in a comment, the same way
/// [`verify_and_expose`] is held to it.
///
/// Spacing is still enforced: a plan whose nets share a cell has no single
/// owner per cell, and every walk downstream of this reads ownership.
#[cfg(test)]
pub(crate) fn realise_without_verifying(
    candidate: &PlanCandidate,
    netlist: &Netlist,
    size: (i32, i32, i32),
) -> Result<VerifiedRealisation, PlannerError> {
    let reservation = verify_spacing(candidate)?;
    let nets = verification_nets(candidate, netlist)?;
    let realised = emit_candidate(candidate, netlist, size)?;
    Ok(VerifiedRealisation {
        realised,
        reservation,
        nets,
    })
}

/// The body [`realise_and_verify`] always had, returning the two values it used
/// to drop on the floor. A tuple rather than the struct above so that the
/// shipping build has no field it never reads.
fn verified_parts(
    candidate: &PlanCandidate,
    netlist: &Netlist,
    size: (i32, i32, i32),
) -> Result<(RealisedCandidate, compile::Reservation, Vec<compile::Net>), PlannerError> {
    let reservation = verify_spacing(candidate)?;
    let nets = verification_nets(candidate, netlist)?;

    let realised = emit_candidate(candidate, netlist, size)?;

    // Terminal style is a planning decision, so it is checked against what
    // realisation actually put at each sink -- a plan claiming directed dust
    // over a repeater is priced wrongly even when the circuit works.
    for (net, route) in candidate.routes.iter().enumerate() {
        for terminal in &route.terminals {
            compile::verify_route_terminal(
                &realised.world,
                &reservation,
                netlist,
                &nets,
                net,
                &route.id,
                terminal,
            )
            .map_err(PlannerError::PhysicalInvariant)?;
        }
    }

    compile::verify_realised_world(
        &realised.world,
        &reservation,
        netlist,
        &nets,
        &realised.ports.gate_output_positions,
        &realised.ports.input_positions,
        &realised.ports.output_positions,
    )
    .map_err(PlannerError::PhysicalInvariant)?;

    Ok((realised, reservation, nets))
}

/// The spacing invariant, stated over the plan rather than over blocks: every
/// routed cell belongs to exactly one route.
///
/// Returns the reservation it proved, so the world-scanning invariants read
/// the same ownership this check established rather than a second opinion.
fn verify_spacing(candidate: &PlanCandidate) -> Result<compile::Reservation, PlannerError> {
    let mut reservation = compile::Reservation::new();
    for (net, route) in candidate.routes.iter().enumerate() {
        for anchor in &route.anchors {
            let position = Position::new(anchor.x, anchor.y, anchor.z);
            // Two different nets in one cell is the violation. One net listing
            // a cell twice is a bookkeeping bug of its own, and is caught
            // where it matters: realisation would give that cell two blocks.
            if let Some(other) = reservation.insert(position, net) {
                if other != net {
                    return Err(PlannerError::PhysicalInvariant(
                        compile::CompileError::SpacingViolation {
                            cell: (position.x, position.y, position.z),
                            expected_net: candidate.routes[other].id.clone(),
                            found_net: Some(route.id.clone()),
                        },
                    ));
                }
                return Err(PlannerError::UnrealisableNode {
                    id: route.id.clone(),
                    reason: format!(
                        "cell ({}, {}, {}) is listed twice by this route",
                        position.x, position.y, position.z
                    ),
                });
            }
        }
    }

    Ok(reservation)
}

/// Rebuild the nets the invariants read from the candidate's own routes.
///
/// A route already records what a net is: which source drives it, and which
/// gate input each of its terminals lands on.  Nothing here consults the
/// legacy floorplan, which is what made the old net list impossible to
/// produce for a moved candidate.
fn verification_nets(
    candidate: &PlanCandidate,
    netlist: &Netlist,
) -> Result<Vec<compile::Net>, PlannerError> {
    let mut nets = Vec::with_capacity(candidate.routes.len());
    for route in &candidate.routes {
        let owner = route.owner.as_deref().unwrap_or(&route.id);
        let source = if let Some(gate) = netlist.gates.iter().position(|g| g.output == owner) {
            compile::Source::Gate(gate)
        } else if let Some(input) = netlist.inputs.iter().position(|name| name == owner) {
            compile::Source::Lever(input)
        } else {
            return Err(PlannerError::UnrealisableNode {
                id: route.id.clone(),
                reason: format!("no gate or primary input named {owner} drives this route"),
            });
        };

        let mut sinks = Vec::with_capacity(route.terminals.len());
        for terminal in &route.terminals {
            let gate = netlist
                .gates
                .iter()
                .position(|g| g.output == terminal.sink.gate)
                .ok_or_else(|| PlannerError::UnrealisableNode {
                    id: route.id.clone(),
                    reason: format!("terminal names unknown gate {}", terminal.sink.gate),
                })?;
            sinks.push((gate, terminal.sink.input_index));
        }

        nets.push(compile::Net::for_verification(source, sinks));
    }

    // `verify_connectivity` and friends were written downstream of
    // `build_nets`, which assigns every gate input exactly one net; they
    // assert that rather than handle its absence. A candidate is free to be
    // missing a terminal, so the precondition is re-established here instead
    // of letting an invariant panic on a plan it was never given.
    let mut covered: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (net, route) in candidate.routes.iter().enumerate() {
        for terminal in &route.terminals {
            let gate = netlist
                .gates
                .iter()
                .position(|g| g.output == terminal.sink.gate)
                .expect("terminal gates were resolved above");
            if let Some(other) = covered.insert((gate, terminal.sink.input_index), net) {
                return Err(PlannerError::UnrealisableNode {
                    id: candidate.routes[net].id.clone(),
                    reason: format!(
                        "input {} of {} is already driven by {}",
                        terminal.sink.input_index,
                        terminal.sink.gate,
                        candidate.routes[other].id
                    ),
                });
            }
        }
    }
    for (gate, definition) in netlist.gates.iter().enumerate() {
        for input_index in 0..definition.inputs.len() {
            if !covered.contains_key(&(gate, input_index)) {
                return Err(PlannerError::UnrealisableNode {
                    id: definition.output.clone(),
                    reason: format!("input {input_index} is driven by no route"),
                });
            }
        }
    }

    Ok(nets)
}

/// A world big enough to hold everything the candidate places.
pub(crate) fn candidate_world_size(candidate: &PlanCandidate) -> (i32, i32, i32) {
    // One cell of margin on every side: a primitive writes its support, its
    // torch and its output pin outside its own anchor, and a route's floor
    // sits one below.
    let mut max = (0, 0, 0);
    let mut extend = |anchor: &Anchor| {
        max.0 = max.0.max(anchor.x);
        max.1 = max.1.max(anchor.y);
        max.2 = max.2.max(anchor.z);
    };
    for node in &candidate.primitive_nodes {
        extend(&node.anchor);
    }
    for route in &candidate.routes {
        for anchor in &route.anchors {
            extend(anchor);
        }
    }

    ((max.0 + 3).max(8), (max.1 + 3).max(5), (max.2 + 3).max(8))
}

/// Integer weights for the candidate cost terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerWeights {
    pub delay: u32,
    pub wire: u32,
    pub space: u32,
    pub turns: u32,
}

impl Default for PlannerWeights {
    fn default() -> Self {
        Self {
            delay: 1,
            wire: 1,
            space: 1,
            turns: 1,
        }
    }
}

/// Reproducibility metadata for a future candidate search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerEffort {
    pub evaluations: usize,
    pub seed: u64,
}

/// The independently-derived cost of one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CostBreakdown {
    pub delay: u64,
    pub wire: u64,
    pub space: u64,
    pub turns: u64,
}

/// Exact normalised scoring could not be represented in the fixed-width score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreError {
    NormalisedNumeratorOverflow,
    NormalisedDenominatorOverflow,
    NormalisedWeightOverflow,
}

impl CostBreakdown {
    fn from_candidate(candidate: &PlanCandidate) -> Self {
        let mut cost = Self {
            delay: 0,
            wire: 0,
            space: bounding_volume(candidate),
            turns: 0,
        };

        cost.delay = critical_path_delay(candidate);
        for route in &candidate.routes {
            cost.wire = cost.wire.saturating_add(route_wire_length(route));
            cost.turns = cost.turns.saturating_add(route_turns(route));
        }

        cost
    }

    /// Return the weighted average of this cost's nonzero seed-normalised
    /// terms.  The pair is reduced and ordered entirely with integers.
    pub fn normalised_against(
        &self,
        seed: &Self,
        weights: &PlannerWeights,
    ) -> Result<NormalisedScore, ScoreError> {
        let terms = [
            (self.delay, seed.delay, weights.delay),
            (self.wire, seed.wire, weights.wire),
            (self.space, seed.space, weights.space),
            (self.turns, seed.turns, weights.turns),
        ];
        let mut numerator = 0_u128;
        let mut denominator = 1_u128;
        let mut total_weight = 0_u128;

        for (value, baseline, weight) in terms {
            if baseline == 0 || weight == 0 {
                continue;
            }

            let baseline = u128::from(baseline);
            let weighted_value = u128::from(weight)
                .checked_mul(u128::from(value))
                .ok_or(ScoreError::NormalisedNumeratorOverflow)?;
            let scaled_numerator = numerator
                .checked_mul(baseline)
                .ok_or(ScoreError::NormalisedNumeratorOverflow)?;
            let scaled_value = weighted_value
                .checked_mul(denominator)
                .ok_or(ScoreError::NormalisedNumeratorOverflow)?;
            numerator = scaled_numerator
                .checked_add(scaled_value)
                .ok_or(ScoreError::NormalisedNumeratorOverflow)?;
            denominator = denominator
                .checked_mul(baseline)
                .ok_or(ScoreError::NormalisedDenominatorOverflow)?;
            let (reduced_numerator, reduced_denominator) = reduce(numerator, denominator);
            numerator = reduced_numerator;
            denominator = reduced_denominator;
            total_weight = total_weight
                .checked_add(u128::from(weight))
                .ok_or(ScoreError::NormalisedWeightOverflow)?;
        }

        if total_weight == 0 {
            return Ok(NormalisedScore::ZERO);
        }

        let denominator = denominator
            .checked_mul(total_weight)
            .ok_or(ScoreError::NormalisedDenominatorOverflow)?;
        Ok(NormalisedScore::new(numerator, denominator))
    }
}

/// A reduced rational total normalised cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalisedScore {
    pub numerator: u128,
    pub denominator: u128,
}

impl NormalisedScore {
    pub const ZERO: Self = Self {
        numerator: 0,
        denominator: 1,
    };
    pub const ONE: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    fn new(numerator: u128, denominator: u128) -> Self {
        debug_assert_ne!(denominator, 0);
        let (numerator, denominator) = reduce(numerator, denominator);
        Self {
            numerator,
            denominator,
        }
    }
}

impl Ord for NormalisedScore {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_fractions(
            self.numerator,
            self.denominator,
            other.numerator,
            other.denominator,
        )
    }
}

impl PartialOrd for NormalisedScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An ordered score plus the original input position, which keeps ties stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CandidateOrder {
    pub normalised: NormalisedScore,
    pub cost: CostBreakdown,
    pub original_index: usize,
}

/// One scored candidate and the immutable metadata that made it reproducible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateScore {
    pub cost: CostBreakdown,
    pub normalised: NormalisedScore,
    pub effort: PlannerEffort,
    pub order: CandidateOrder,
}

/// Score candidates against one immutable seed and return them in stable order.
pub fn rank_candidates(
    candidates: &[PlanCandidate],
    seed: &PlanCandidate,
    weights: &PlannerWeights,
    effort: PlannerEffort,
) -> Result<Vec<CandidateScore>, ScoreError> {
    let mut scores = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| candidate.score_against_at(seed, weights, effort, index))
        .collect::<Result<Vec<_>, _>>()?;
    scores.sort_by_key(|score| score.order);
    Ok(scores)
}

/// Measured local routing, terminal, and variant effort for one gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateEffort {
    pub gate: usize,
    pub selected_entry: usize,
    pub route_wire: u64,
    pub route_turns: u64,
    pub terminal_repeaters: u64,
    pub variant: u8,
}

/// Search diagnostics.  The public optimiser returns only `candidate`; the
/// report keeps its hard evaluation budget directly testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimisationReport {
    pub candidate: PlanCandidate,
    pub evaluations: usize,
    pub gate_effort: Vec<GateEffort>,
}

/// Deterministically improve a candidate using only candidate reservations
/// and topology constraints.  It never invokes the legacy physical router.
pub fn optimise(
    seed: PlanCandidate,
    netlist: &Netlist,
    weights: PlannerWeights,
    effort: PlannerEffort,
) -> PlanCandidate {
    optimise_with_report(seed, netlist, weights, effort).candidate
}

fn optimise_with_report(
    seed: PlanCandidate,
    netlist: &Netlist,
    weights: PlannerWeights,
    effort: PlannerEffort,
) -> OptimisationReport {
    let baseline = seed.clone();
    let mut best = seed;
    let mut evaluations = 0;
    let mut generation = 0usize;
    loop {
        let epoch = best.clone();
        let mut improved = false;
        for change in enumerate_moves(&epoch, effort.seed) {
            if evaluations >= effort.evaluations {
                return OptimisationReport {
                    gate_effort: gate_efforts(&best),
                    candidate: best,
                    evaluations,
                };
            }
            evaluations += 1;
            generation += 1;
            let Some(proposal) = change.apply(&epoch) else {
                continue;
            };
            // Score first, then verify. Legality is still what the circuit
            // says -- `verify_candidate` builds the world and runs every
            // invariant on it -- but a proposal that does not beat the best is
            // discarded either way, and building a whole world to reject it is
            // the entire cost of this loop. Only a winner is worth proving.
            let Ok(proposal_score) =
                proposal.score_against_at(&baseline, &weights, effort, generation)
            else {
                continue;
            };
            let Ok(best_score) = best.score_against_at(&baseline, &weights, effort, generation)
            else {
                continue;
            };
            if proposal_score.order >= best_score.order {
                continue;
            }
            if verify_candidate(&proposal, netlist).is_err() {
                continue;
            }
            best = proposal;
            improved = true;
        }
        if !improved || evaluations >= effort.evaluations {
            return OptimisationReport {
                gate_effort: gate_efforts(&best),
                candidate: best,
                evaluations,
            };
        }
    }
}

/// Every local change worth trying, as instructions rather than candidates.
///
/// This used to return the candidates themselves, which meant building all of
/// them -- rerouting every incident net of every primitive in six directions
/// -- before the loop that spends the evaluation budget had run once. For
/// seven_segment that is five hundred reroutes to decide whether to keep the
/// first. The budget can only bound the work if the work happens inside it.
fn enumerate_moves(candidate: &PlanCandidate, seed: u64) -> Vec<Move> {
    const MOVES: [(i32, i32, i32); 6] = [
        (-1, 0, 0),
        (0, -1, 0),
        (0, 0, -1),
        (0, 0, 1),
        (0, 1, 0),
        (1, 0, 0),
    ];
    let rotation = (seed as usize) % MOVES.len();
    let mut moves = Vec::new();
    for primitive in 0..candidate.anchors.len() {
        let from = candidate.anchors[primitive];
        for offset in 0..MOVES.len() {
            let (x, y, z) = MOVES[(offset + rotation) % MOVES.len()];
            moves.push(Move::Shift {
                primitive,
                to: Anchor {
                    x: from.x.saturating_add(x),
                    y: from.y.saturating_add(y),
                    z: from.z.saturating_add(z),
                },
            });
        }
    }
    for gate in topology_alternatives(candidate) {
        moves.push(gate);
    }
    moves
}


/// One local change, not yet made.
#[derive(Debug, Clone, Copy)]
enum Move {
    Shift { primitive: NodeId, to: Anchor },
    Retopologise { gate: usize, entry: usize },
}

impl Move {
    /// Carry this out, or report that it cannot be.
    fn apply(self, candidate: &PlanCandidate) -> Option<PlanCandidate> {
        match self {
            Move::Shift { primitive, to } => try_move(candidate, primitive, to).ok(),
            Move::Retopologise { gate, entry } => {
                let mut alternative = candidate.clone();
                alternative.topology_entries.insert(gate, entry);
                if !candidate_allows_entry(&alternative, gate, entry) {
                    return None;
                }
                if let Some(emission) = alternative.legacy_emission.as_ref() {
                    let selection: EntrySelection = alternative.topology_entries.clone();
                    reexpand_gate(
                        emission.netlist(),
                        &Library::default_library(),
                        &selection,
                        gate,
                        entry,
                    )
                    .ok()?;
                }
                Some(alternative)
            }
        }
    }
}

fn gate_efforts(candidate: &PlanCandidate) -> Vec<GateEffort> {
    candidate
        .primitive_nodes
        .iter()
        .enumerate()
        .filter_map(|(gate, node)| {
            let name = node.id.strip_prefix("gate:")?;
            let mut route_wire: u64 = 0;
            let mut route_turns_total: u64 = 0;
            let mut terminal_repeaters: u64 = 0;
            for route in &candidate.routes {
                if route
                    .terminals
                    .iter()
                    .any(|terminal| terminal.sink.gate == name)
                {
                    route_wire = route_wire.saturating_add(route_wire_length(route));
                    route_turns_total = route_turns_total.saturating_add(route_turns(route));
                    terminal_repeaters += route
                        .terminals
                        .iter()
                        .filter(|terminal| {
                            terminal.sink.gate == name
                                && terminal.kind == RouteTerminalKind::RepeaterIntoSupport
                        })
                        .count() as u64;
                }
            }
            Some(GateEffort {
                gate,
                selected_entry: candidate.selected_entry(gate),
                route_wire,
                route_turns: route_turns_total,
                terminal_repeaters,
                variant: candidate.variant_indices.get(gate).copied().unwrap_or(0),
            })
        })
        .collect()
}

/// Propose every other library technique for each gate, and let the score
/// decide.
///
/// There used to be a predictive filter here -- `predicted_local_cost` -- that
/// added the gate's orientation *index* to its cost and charged terminal
/// repeaters only when the entry was not zero, which made entry zero cheaper
/// by construction. Since every production seed starts at entry zero, it
/// proposed nothing, ever. Two invented numbers deciding which alternatives
/// are worth measuring is worse than measuring them: proposals are verified by
/// building them now, so an alternative that is illegal is rejected and one
/// that is merely worse loses on its real score.
fn topology_alternatives(candidate: &PlanCandidate) -> Vec<Move> {
    let library = Library::default_library();
    let mut moves = Vec::new();
    for gate in gate_efforts(candidate) {
        let kind = candidate
            .legacy_emission
            .as_ref()
            .and_then(|emission| emission.netlist().gates.get(gate.gate))
            .map(|definition| definition.kind);
        let entry_count = kind.map_or(0, |kind| library.entries_for(kind).len());
        for entry in 0..entry_count {
            if entry == gate.selected_entry {
                continue;
            }
            moves.push(Move::Retopologise {
                gate: gate.gate,
                entry,
            });
        }
    }
    moves
}

fn candidate_allows_entry(candidate: &PlanCandidate, gate: usize, entry: usize) -> bool {
    if entry != 0 {
        return true;
    }
    let Some(name) = candidate
        .primitive_nodes
        .get(gate)
        .and_then(|node| node.id.strip_prefix("gate:"))
    else {
        return false;
    };
    candidate.routes.iter().all(|route| {
        !route
            .terminals
            .iter()
            .any(|terminal| terminal.sink.gate == name)
            || route
                .terminals
                .iter()
                .all(|terminal| terminal.sink.gate == name)
    })
}



fn bounding_volume(candidate: &PlanCandidate) -> u64 {
    let anchors = candidate.anchors.iter().copied().chain(
        candidate
            .routes
            .iter()
            .flat_map(|route| route.anchors.iter().copied()),
    );
    let Some(first) = anchors.clone().next() else {
        return 0;
    };
    let (mut min_x, mut max_x) = (first.x, first.x);
    let (mut min_y, mut max_y) = (first.y, first.y);
    let (mut min_z, mut max_z) = (first.z, first.z);

    for anchor in anchors {
        min_x = min_x.min(anchor.x);
        max_x = max_x.max(anchor.x);
        min_y = min_y.min(anchor.y);
        max_y = max_y.max(anchor.y);
        min_z = min_z.min(anchor.z);
        max_z = max_z.max(anchor.z);
    }

    axis_span(min_x, max_x)
        .saturating_mul(axis_span(min_y, max_y))
        .saturating_mul(axis_span(min_z, max_z))
}

fn axis_span(minimum: i32, maximum: i32) -> u64 {
    (i64::from(maximum) - i64::from(minimum) + 1) as u64
}

/// The candidate's own critical-path delay, in game ticks.
///
/// This is `timing::critical_path_settle_model_game_ticks` without the lamp:
/// one torch delay per non-merge gate on the path, one per repeater the routes
/// along it actually contain. Both are facts the candidate carries -- a node
/// says whether it is a merge, a route carries the blocks it lays -- so no
/// part of this is estimated from geometry.
///
/// A fanout route's repeaters are counted per branch, because each terminal
/// records how many stand between it and the route's source -- counted when
/// the branch was laid, not recovered afterwards.
fn critical_path_delay(candidate: &PlanCandidate) -> u64 {
    let mut is_merge: BTreeMap<&str, bool> = BTreeMap::new();
    for node in &candidate.primitive_nodes {
        if let Some(name) = node.id.strip_prefix("gate:") {
            is_merge.insert(name, node.realisation == NodeRealisation::WireMerge);
        }
    }

    let mut edges: BTreeMap<&str, Vec<(&str, u64)>> = BTreeMap::new();
    for route in &candidate.routes {
        let Some(owner) = route.owner.as_deref() else {
            continue;
        };
        for terminal in &route.terminals {
            let sink = terminal.sink.gate.as_str();
            let gate_cost = u64::from(!is_merge.get(sink).copied().unwrap_or(false));
            let repeaters = terminal.repeaters;
            edges
                .entry(owner)
                .or_default()
                .push((sink, repeaters + gate_cost));
        }
    }

    // Longest path over a DAG. `depth` memoises, and `visiting` makes a cycle
    // stop rather than recurse -- `compile` rejects combinational cycles long
    // before this, so hitting one means the candidate is malformed, and a
    // wrong number is better than a stack overflow while pricing it.
    fn longest(
        signal: &str,
        edges: &BTreeMap<&str, Vec<(&str, u64)>>,
        depth: &mut BTreeMap<String, u64>,
        visiting: &mut BTreeSet<String>,
    ) -> u64 {
        if let Some(&known) = depth.get(signal) {
            return known;
        }
        if !visiting.insert(signal.to_string()) {
            return 0;
        }
        let best = edges
            .get(signal)
            .map(|outgoing| {
                outgoing
                    .iter()
                    .map(|&(next, weight)| weight + longest(next, edges, depth, visiting))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        visiting.remove(signal);
        depth.insert(signal.to_string(), best);
        best
    }

    let mut depth = BTreeMap::new();
    let mut visiting = BTreeSet::new();
    let worst = edges
        .keys()
        .map(|signal| longest(signal, &edges, &mut depth, &mut visiting))
        .max()
        .unwrap_or(0);

    worst.saturating_mul(crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS)
}


fn route_wire_length(route: &Route) -> u64 {
    route
        .anchors
        .windows(2)
        .map(|pair| manhattan_distance(pair[0], pair[1]))
        .fold(0, u64::saturating_add)
}

fn route_turns(route: &Route) -> u64 {
    route
        .anchors
        .windows(3)
        .filter(|window| direction(window[0], window[1]) != direction(window[1], window[2]))
        .count() as u64
}

fn manhattan_distance(left: Anchor, right: Anchor) -> u64 {
    (i64::from(left.x) - i64::from(right.x)).unsigned_abs()
        + (i64::from(left.y) - i64::from(right.y)).unsigned_abs()
        + (i64::from(left.z) - i64::from(right.z)).unsigned_abs()
}

fn direction(from: Anchor, to: Anchor) -> (i8, i8, i8) {
    (
        (to.x > from.x) as i8 - (to.x < from.x) as i8,
        (to.y > from.y) as i8 - (to.y < from.y) as i8,
        (to.z > from.z) as i8 - (to.z < from.z) as i8,
    )
}

fn reduce(numerator: u128, denominator: u128) -> (u128, u128) {
    let divisor = gcd(numerator, denominator);
    (numerator / divisor, denominator / divisor)
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

/// Compare two positive-denominator fractions without multiplying them.
///
/// Equal whole-number parts can be removed before reciprocating both
/// remainders; reciprocation reverses their order.  That keeps the comparison
/// exact even when a cross multiplication would exceed `u128`.
fn compare_fractions(
    mut left_numerator: u128,
    mut left_denominator: u128,
    mut right_numerator: u128,
    mut right_denominator: u128,
) -> Ordering {
    let mut reversed = false;

    loop {
        let whole_order =
            (left_numerator / left_denominator).cmp(&(right_numerator / right_denominator));
        if whole_order != Ordering::Equal {
            return if reversed {
                whole_order.reverse()
            } else {
                whole_order
            };
        }

        let left_remainder = left_numerator % left_denominator;
        let right_remainder = right_numerator % right_denominator;
        match (left_remainder == 0, right_remainder == 0) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if reversed {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if reversed {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {
                left_numerator = left_denominator;
                left_denominator = left_remainder;
                right_numerator = right_denominator;
                right_denominator = right_remainder;
                reversed = !reversed;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuits::and4::build_and4_netlist;
    use crate::compile::Gate;

    fn local_move_fixture() -> PlanCandidate {
        let anchors = vec![
            Anchor { x: 0, y: 0, z: 0 },
            Anchor { x: 4, y: 0, z: 0 },
            Anchor { x: 0, y: 0, z: 4 },
            Anchor { x: 4, y: 0, z: 4 },
        ];
        PlanCandidate::with_primitive_nodes(
            anchors.clone(),
            anchors
                .iter()
                .enumerate()
                .map(|(id, &anchor)| PrimitiveNode {
                    id: if id == 0 {
                        "input:incident".to_string()
                    } else {
                        format!("node:{id}")
                    },
                    anchor,
                    realisation: if id == 0 {
                        NodeRealisation::Primitive(Primitive::Lever)
                    } else {
                        NodeRealisation::Primitive(Primitive::Torch)
                    },
                    footprint: Vec::new(),
                    conductors: Vec::new(),
                    pinned: false,
                    output_pin: None,
                })
                .collect(),
            vec![
                Route::unrealised(
                    "incident".to_string(),
                    vec![Anchor { x: 0, y: 0, z: 0 }, Anchor { x: 4, y: 0, z: 0 }],
                    vec![],
                ),
                Route::new(
                    "unrelated",
                    vec![
                        Anchor { x: 0, y: 0, z: 4 },
                        Anchor { x: 1, y: 0, z: 4 },
                        Anchor { x: 2, y: 0, z: 4 },
                        Anchor { x: 3, y: 0, z: 4 },
                        Anchor { x: 4, y: 0, z: 4 },
                    ],
                ),
            ],
        )
    }

    #[test]
    fn moving_one_anchor_reroutes_only_its_incident_edges() {
        let seed = local_move_fixture();
        let original_nonincident = seed.routes()[1].clone();

        let moved = try_move(&seed, 0, Anchor { x: 0, y: 1, z: 0 })
            .expect("the open local fixture must have a legal move");

        assert_eq!(
            moved.primitive_nodes()[0].anchor,
            Anchor { x: 0, y: 1, z: 0 }
        );
        assert_ne!(moved.routes()[0], seed.routes()[0]);
        assert_eq!(moved.routes()[1], original_nonincident);
    }

    #[test]
    fn local_move_routes_around_a_live_nonincident_reservation() {
        let mut seed = local_move_fixture();
        let blocker = Route::new("blocker", vec![Anchor { x: 1, y: 0, z: 0 }]);
        seed.routes.push(blocker.clone());

        let moved = try_move(&seed, 0, Anchor { x: 0, y: 1, z: 0 })
            .expect("A* must detour around the live nonincident reservation");

        assert_eq!(moved.routes()[2], blocker);
        assert!(
            !moved.routes()[0]
                .anchors()
                .contains(&Anchor { x: 1, y: 0, z: 0 }),
            "the rebuilt edge must not reuse a cell owned by the nonincident route"
        );
    }

    fn fanout_move_fixture() -> PlanCandidate {
        let source = Anchor { x: 0, y: 0, z: 0 };
        let old_moved_sink = Anchor { x: 0, y: 0, z: 4 };
        let other_sink = Anchor { x: 4, y: 0, z: 0 };
        PlanCandidate::with_primitive_nodes(
            vec![source, old_moved_sink, other_sink],
            vec![
                PrimitiveNode {
                    id: "input:source".to_string(),
                    anchor: source,
                    realisation: NodeRealisation::Primitive(Primitive::Lever),
                    footprint: Vec::new(),
                    conductors: Vec::new(),
                    pinned: false,
                    output_pin: None,
                },
                PrimitiveNode {
                    id: "gate:moved".to_string(),
                    anchor: old_moved_sink,
                    realisation: NodeRealisation::Primitive(Primitive::Torch),
                    footprint: Vec::new(),
                    conductors: Vec::new(),
                    pinned: false,
                    output_pin: None,
                },
                PrimitiveNode {
                    id: "gate:other".to_string(),
                    anchor: other_sink,
                    realisation: NodeRealisation::Primitive(Primitive::Torch),
                    footprint: Vec::new(),
                    conductors: Vec::new(),
                    pinned: false,
                    output_pin: None,
                },
            ],
            vec![Route::unrealised(
                "source".to_string(),
                vec![source, other_sink, old_moved_sink],
                vec![
                    RouteTerminal {
                        sink: RouteSink {
                            gate: "other".to_string(),
                            input_index: 0,
                            anchor: other_sink,
                        },
                        kind: RouteTerminalKind::RepeaterIntoSupport,
                        repeaters: 0,
                    },
                    RouteTerminal {
                        sink: RouteSink {
                            gate: "moved".to_string(),
                            input_index: 0,
                            anchor: old_moved_sink,
                        },
                        kind: RouteTerminalKind::RepeaterIntoSupport,
                        repeaters: 0,
                    },
                ],
            )],
        )
    }

    #[test]
    fn moving_a_primitive_reserves_its_destination_before_rerouting_fanout() {
        let seed = fanout_move_fixture();
        let destination = Anchor { x: 1, y: 0, z: 0 };
        let source = seed.primitive_nodes()[0].anchor;
        let other_sink = seed.primitive_nodes()[2].anchor;
        // The same question `try_move`'s rebuild loop now asks: the socket the
        // netlist declared for this sink, not the one the approach direction
        // implies. This fixture builds through `with_primitive_nodes`, so every
        // facing is north and the two answers coincide -- what moves is which
        // question the test predicts, not the cell it predicts.
        let other_terminal =
            seed.declared_socket(other_sink, &seed.routes()[0].terminals()[0].sink);
        let mut without_destination_reservation = seed.live_reservation(&[true]);
        without_destination_reservation.remove(&seed.primitive_nodes()[1].anchor);

        let unreserved_path = deterministic_astar(
            source,
            other_terminal,
            other_sink,
            "source",
            &without_destination_reservation,
            &Prices::RipUp(&Congestion::default()),
        )
        .expect("the direct fanout branch is routable without the destination reservation");
        assert_eq!(
            unreserved_path,
            vec![
                source,
                destination,
                Anchor { x: 2, y: 0, z: 0 },
                other_terminal,
            ],
            "without reserving the move destination before A*, the deterministic shortest path collides with it"
        );

        let moved = try_move(&seed, 1, destination)
            .expect("the fanout can detour around the moved primitive");

        assert!(
            !moved.routes()[0].anchors().contains(&destination),
            "the rebuilt fanout must avoid the moved primitive's destination during A*"
        );
    }

    fn merge_terminal_fixture(kind: RouteTerminalKind) -> PlanCandidate {
        let source = Anchor { x: 0, y: 0, z: 0 };
        let merge = Anchor { x: 4, y: 0, z: 0 };
        PlanCandidate::with_primitive_nodes(
            vec![source, merge],
            vec![
                PrimitiveNode {
                    id: "input:a".to_string(),
                    anchor: source,
                    realisation: NodeRealisation::Primitive(Primitive::Lever),
                    footprint: Vec::new(),
                    conductors: Vec::new(),
                    pinned: false,
                    output_pin: None,
                },
                PrimitiveNode {
                    id: "gate:merge".to_string(),
                    anchor: merge,
                    realisation: NodeRealisation::WireMerge,
                    footprint: Vec::new(),
                    conductors: Vec::new(),
                    pinned: false,
                    output_pin: None,
                },
            ],
            vec![Route::unrealised(
                "a".to_string(),
                vec![source, Anchor { x: 1, y: 0, z: 0 }, merge],
                vec![RouteTerminal {
                    sink: RouteSink {
                        gate: "merge".to_string(),
                        input_index: 0,
                        anchor: merge,
                    },
                    kind,
                    repeaters: 0,
                }],
            )],
        )
    }

    #[test]
    fn rerouting_a_wire_merge_keeps_its_bare_terminal_semantics() {
        for kind in [
            RouteTerminalKind::BareMergeDust,
            RouteTerminalKind::BareMergeRepeater,
        ] {
            let moved = try_move(
                &merge_terminal_fixture(kind),
                0,
                Anchor { x: 0, y: 1, z: 0 },
            )
            .expect("the merge input route can be locally rerouted");

            assert_eq!(
                moved.routes()[0].terminal_kinds(),
                vec![kind],
                "wire-merge terminals must never become NOR-support terminals"
            );
        }
    }

    #[test]
    fn a_route_touching_the_old_anchor_without_graph_incidence_is_not_rerouted() {
        let old_anchor = Anchor { x: 0, y: 0, z: 0 };
        let stale_route = Route::new(
            "stale-geometry",
            vec![old_anchor, Anchor { x: 1, y: 0, z: 0 }],
        );
        let seed = PlanCandidate::with_primitive_nodes(
            vec![old_anchor],
            vec![PrimitiveNode {
                id: "input:a".to_string(),
                anchor: old_anchor,
                realisation: NodeRealisation::Primitive(Primitive::Lever),
                footprint: Vec::new(),
                conductors: Vec::new(),
                pinned: false,
                output_pin: None,
            }],
            vec![stale_route.clone()],
        );

        let moved = try_move(&seed, 0, Anchor { x: 0, y: 1, z: 0 })
            .expect("moving an unrelated primitive leaves the stale route alone");

        assert_eq!(moved.routes()[0], stale_route);
    }

    #[test]
    fn a_straight_proven_terminal_uses_dust_but_a_corner_uses_repeater() {
        let straight = TerminalApproach::new(
            Anchor { x: 0, y: 0, z: 0 },
            Anchor { x: 1, y: 0, z: 0 },
            Anchor { x: 2, y: 0, z: 0 },
            2,
            true,
        );
        let corner = TerminalApproach::new(
            Anchor { x: 0, y: 0, z: 1 },
            Anchor { x: 1, y: 0, z: 1 },
            Anchor { x: 1, y: 0, z: 2 },
            2,
            true,
        );
        let weak = TerminalApproach::new(
            Anchor { x: 0, y: 0, z: 0 },
            Anchor { x: 1, y: 0, z: 0 },
            Anchor { x: 2, y: 0, z: 0 },
            1,
            true,
        );

        assert_eq!(
            terminal_style(&straight),
            TerminalStyle::DirectedDustIntoSupport
        );
        assert_eq!(terminal_style(&corner), TerminalStyle::RepeaterIntoSupport);
        assert_eq!(terminal_style(&weak), TerminalStyle::RepeaterIntoSupport);
    }

    /// A facing nobody chose must not be indistinguishable from one somebody
    /// did. `variant_indices` is a bare `Vec<u8>` with nothing at the type
    /// level keeping it in `0..=3`, and the lenient `unwrap_or_default()` this
    /// replaced turned both a bad index and an unknown node into north --
    /// which then builds, routes and *verifies* clean, because north is what
    /// everything else assumes too. These pin the panic so it does not get
    /// quietly relaxed back the first time it fires.
    #[test]
    #[should_panic(expected = "no facing recorded for node 3")]
    fn facing_of_refuses_a_node_it_has_no_record_for() {
        fixture_candidate().facing_of(3);
    }

    #[test]
    #[should_panic(expected = "variant index 4")]
    fn facing_of_refuses_an_index_no_facing_has() {
        let mut candidate = fixture_candidate();
        candidate.variant_indices[0] = 4;
        candidate.facing_of(0);
    }

    /// ...and the path that is not a bug is untouched: every constructor but
    /// [`PlanCandidate::with_facings`] zero-fills `variant_indices`, and zero
    /// is north. `fixture_candidate` builds through `with_primitive_nodes`,
    /// which is one of them.
    #[test]
    fn every_node_of_a_fresh_candidate_faces_north() {
        let candidate = fixture_candidate();
        for node in 0..candidate.anchors.len() {
            assert_eq!(
                candidate.facing_of(node),
                geometry::CellFacing::NORTH,
                "node {node}"
            );
        }
    }

    fn fixture_seed() -> PlanCandidate {
        PlanCandidate::new(
            vec![
                Anchor { x: 0, y: 0, z: 0 },
                Anchor { x: 2, y: 0, z: 0 },
                Anchor { x: 2, y: 0, z: 3 },
            ],
            vec![Route::new(
                "seed-route",
                vec![
                    Anchor { x: 0, y: 0, z: 0 },
                    Anchor { x: 2, y: 0, z: 0 },
                    Anchor { x: 2, y: 0, z: 3 },
                ],
            )],
        )
    }

    fn fixture_candidate() -> PlanCandidate {
        PlanCandidate::new(
            vec![
                Anchor { x: 0, y: 0, z: 0 },
                Anchor { x: 3, y: 0, z: 0 },
                Anchor { x: 3, y: 0, z: 3 },
            ],
            vec![Route::new(
                "candidate-route",
                vec![
                    Anchor { x: 0, y: 0, z: 0 },
                    Anchor { x: 3, y: 0, z: 0 },
                    Anchor { x: 3, y: 0, z: 3 },
                ],
            )],
        )
    }

    fn optimisation_fixture() -> PlanCandidate {
        let source = Anchor { x: 0, y: 0, z: 0 };
        let sink = Anchor { x: 5, y: 0, z: 0 };
        PlanCandidate::with_primitive_nodes(
            vec![source, sink],
            vec![
                PrimitiveNode {
                    id: "input:a".to_string(),
                    anchor: source,
                    realisation: NodeRealisation::Primitive(Primitive::Lever),
                    footprint: Vec::new(),
                    conductors: Vec::new(),
                    pinned: false,
                    output_pin: None,
                },
                PrimitiveNode {
                    id: "gate:y".to_string(),
                    anchor: sink,
                    realisation: NodeRealisation::Primitive(Primitive::Torch),
                    footprint: Vec::new(),
                    conductors: Vec::new(),
                    pinned: false,
                    output_pin: None,
                },
            ],
            vec![Route::unrealised(
                "a".to_string(),
                vec![
                    source,
                    Anchor { x: 1, y: 0, z: 0 },
                    Anchor { x: 2, y: 0, z: 0 },
                    Anchor { x: 3, y: 0, z: 0 },
                    Anchor { x: 4, y: 0, z: 0 },
                ],
                vec![RouteTerminal {
                    sink: RouteSink {
                        gate: "y".to_string(),
                        input_index: 0,
                        anchor: sink,
                    },
                    kind: RouteTerminalKind::RepeaterIntoSupport,
                    repeaters: 0,
                }],
            )],
        )
    }

    fn fixture_seed_with_illegal_alternative() -> PlanCandidate {
        let source = Anchor { x: 0, y: 0, z: 0 };
        let merge = Anchor { x: 4, y: 0, z: 0 };
        let shared_consumer = Anchor { x: 4, y: 0, z: 4 };
        PlanCandidate::with_topology_entries(
            PlanCandidate::with_primitive_nodes(
                vec![source, merge, shared_consumer],
                vec![
                    PrimitiveNode {
                        id: "input:a".to_string(),
                        anchor: source,
                        realisation: NodeRealisation::Primitive(Primitive::Lever),
                        footprint: Vec::new(),
                        conductors: Vec::new(),
                        pinned: false,
                        output_pin: None,
                    },
                    PrimitiveNode {
                        id: "gate:merge".to_string(),
                        anchor: merge,
                        realisation: NodeRealisation::WireMerge,
                        footprint: Vec::new(),
                        conductors: Vec::new(),
                        pinned: false,
                        output_pin: None,
                    },
                    PrimitiveNode {
                        id: "gate:other".to_string(),
                        anchor: shared_consumer,
                        realisation: NodeRealisation::Primitive(Primitive::Torch),
                        footprint: Vec::new(),
                        conductors: Vec::new(),
                        pinned: false,
                        output_pin: None,
                    },
                ],
                vec![
                    Route::unrealised(
                        "a".to_string(),
                        vec![source, Anchor { x: 1, y: 0, z: 0 }],
                        vec![RouteTerminal {
                            sink: RouteSink {
                                gate: "merge".to_string(),
                                input_index: 0,
                                anchor: merge,
                            },
                            kind: RouteTerminalKind::RepeaterIntoSupport,
                            repeaters: 0,
                        }],
                    ),
                    Route::unrealised(
                        "a".to_string(),
                        vec![source, Anchor { x: 0, y: 0, z: 1 }],
                        vec![RouteTerminal {
                            sink: RouteSink {
                                gate: "other".to_string(),
                                input_index: 0,
                                anchor: shared_consumer,
                            },
                            kind: RouteTerminalKind::RepeaterIntoSupport,
                            repeaters: 0,
                        }],
                    ),
                ],
            ),
            [(1, 1)].into_iter().collect(),
        )
    }

    #[test]
    fn fixed_seed_weights_and_effort_choose_the_same_legal_candidate() {
        let seed = optimisation_fixture();
        let effort = PlannerEffort {
            evaluations: 128,
            seed: 0x26_02,
        };

        let _ = seed;
        // A synthetic fixture cannot be realised, so optimisation is exercised
        // on a real seed: with legality now decided by building the candidate,
        // a fixture with no blocks would simply have every proposal rejected
        // and prove nothing.
        let (seed, netlist) = legacy_and4_seed_with_netlist();

        let left = optimise(seed.clone(), &netlist, PlannerWeights::default(), effort);
        let right = optimise(seed, &netlist, PlannerWeights::default(), effort);

        assert_eq!(left, right);
        verify_candidate(&left, &netlist).expect("optimisation must only return legal candidates");
    }

    #[test]
    fn optimisation_never_exceeds_its_evaluation_budget() {
        let (seed, netlist) = legacy_and4_seed_with_netlist();
        let report = optimise_with_report(
            seed,
            &netlist,
            PlannerWeights::default(),
            PlannerEffort {
                evaluations: 3,
                seed: 7,
            },
        );

        assert!(report.evaluations <= 3);
        verify_candidate(&report.candidate, &netlist)
            .expect("optimisation must only return legal candidates");
    }

    #[test]
    fn gate_effort_reports_route_terminal_and_variant_costs_by_gate() {
        let effort = optimisation_fixture().gate_effort();

        assert_eq!(effort.len(), 1);
        assert_eq!(effort[0].gate, 1);
        assert_eq!(effort[0].route_wire, 4);
        assert_eq!(effort[0].terminal_repeaters, 1);
        assert_eq!(effort[0].variant, 0);
    }


    #[test]
    fn a_rejected_topology_alternative_leaves_the_best_candidate_unchanged() {
        let seed = fixture_seed_with_illegal_alternative();
        let (_, netlist) = legacy_and4_seed_with_netlist();
        let result = optimise(
            seed.clone(),
            &netlist,
            PlannerWeights::default(),
            PlannerEffort {
                evaluations: 128,
                seed: 1,
            },
        );

        assert_eq!(result.selected_entry(1), seed.selected_entry(1));
    }

    fn run_fixture(seed: u64) -> CandidateScore {
        fixture_candidate()
            .score_against(
                &fixture_seed(),
                &PlannerWeights::default(),
                PlannerEffort {
                    evaluations: 4,
                    seed,
                },
            )
            .expect("fixture costs fit the exact score representation")
    }

    #[test]
    fn a_seed_scores_one_for_every_nonzero_normalised_term() {
        let seed = fixture_seed();
        assert_eq!(
            seed.score(&PlannerWeights::default())
                .expect("fixture costs fit the exact score representation"),
            NormalisedScore::ONE
        );
    }

    #[test]
    fn same_candidate_weights_effort_and_seed_score_identically() {
        assert_eq!(run_fixture(17), run_fixture(17));
    }

    #[test]
    fn cost_comes_from_route_geometry_and_occupied_bounding_volume() {
        assert_eq!(
            fixture_seed().cost(),
            CostBreakdown {
                // A fixture route has no owner and no realisation, so it
                // contributes no hop and no repeaters: delay is a property of
                // the circuit, not of how far its dust travels.
                delay: 0,
                wire: 5,
                space: 12,
                turns: 1,
            }
        );
    }

    #[test]
    fn ranking_keeps_input_order_for_equal_scores() {
        let seed = fixture_seed();
        let candidates = vec![fixture_candidate(), fixture_candidate()];
        let ranked = rank_candidates(
            &candidates,
            &seed,
            &PlannerWeights::default(),
            PlannerEffort {
                evaluations: 4,
                seed: 17,
            },
        )
        .expect("fixture costs fit the exact score representation");

        assert_eq!(
            ranked
                .iter()
                .map(|score| score.order.original_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn normalised_score_ordering_does_not_collapse_large_distinct_fractions() {
        let just_above_one = NormalisedScore {
            numerator: u128::MAX,
            denominator: u128::MAX - 1,
        };
        let just_below_one = NormalisedScore {
            numerator: u128::MAX - 1,
            denominator: u128::MAX,
        };

        assert!(just_above_one > just_below_one);
    }

    #[test]
    fn normalisation_rejects_two_large_terms_that_cannot_be_averaged_exactly() {
        let cost = CostBreakdown {
            delay: 1,
            wire: 1,
            space: 0,
            turns: 0,
        };
        let seed = CostBreakdown {
            delay: u64::MAX,
            wire: u64::MAX - 1,
            space: 0,
            turns: 0,
        };

        assert_eq!(
            cost.normalised_against(&seed, &PlannerWeights::default()),
            Err(ScoreError::NormalisedDenominatorOverflow)
        );
    }

    #[test]
    fn normalisation_rejects_three_large_terms_that_cannot_be_accumulated_exactly() {
        let cost = CostBreakdown {
            delay: 1,
            wire: 1,
            space: 1,
            turns: 0,
        };
        let seed = CostBreakdown {
            delay: u64::MAX,
            wire: u64::MAX - 1,
            space: u64::MAX - 2,
            turns: 0,
        };

        assert_eq!(
            cost.normalised_against(&seed, &PlannerWeights::default()),
            Err(ScoreError::NormalisedNumeratorOverflow)
        );
    }

    #[test]
    fn repeated_adjacent_anchors_do_not_add_wire_or_turns() {
        let a = Anchor { x: 0, y: 0, z: 0 };
        let b = Anchor { x: 1, y: 0, z: 0 };
        let candidate = PlanCandidate::new(vec![], vec![Route::new("degenerate", vec![a, a, b])]);

        assert_eq!(candidate.routes()[0].anchors(), &[a, b]);
        assert_eq!(
            candidate.cost(),
            CostBreakdown {
                delay: 0,
                wire: 1,
                space: 2,
                turns: 0,
            }
        );
    }

    fn legacy_and4_seed() -> PlanCandidate {
        legacy_and4_seed_with_netlist().0
    }

    fn legacy_and4_seed_with_netlist() -> (PlanCandidate, Netlist) {
        let (netlist, _) = build_and4_netlist();
        let compiled = compile::compile_legacy(&netlist).expect("and4 fixture must compile");
        let seed = seed_from_legacy(&netlist, &compiled).expect("compiled fixture must seed");
        (seed, netlist)
    }

    fn legacy_fanout_seed() -> PlanCandidate {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["left".to_string(), "right".to_string()],
            gates: vec![Gate::nor("left", &["a"]), Gate::nor("right", &["a"])],
        };
        let compiled = compile::compile_legacy(&netlist).expect("fanout fixture must compile");
        seed_from_legacy(&netlist, &compiled).expect("fanout fixture must seed")
    }

    #[test]
    fn plan_candidate_equality_includes_primitive_nodes() {
        let expected = legacy_and4_seed();
        let mut candidate = expected.clone();
        candidate.primitive_nodes[0].id.push_str("-wrong");

        assert_ne!(candidate, expected);
    }

    /// Each of these breaks the plan in one specific way. The point is not
    /// which error comes back but that realising the plan and scanning the
    /// result catches it -- the old check compared the candidate against a
    /// freshly recompiled seed, which can only ever validate a seed.
    fn rejection(candidate: &PlanCandidate, netlist: &Netlist) -> String {
        match verify_candidate(candidate, netlist) {
            Ok(()) => panic!("a corrupted candidate must not verify"),
            Err(error) => error.to_string(),
        }
    }

    /// Realisation must reproduce the legacy world for the big circuits too,
    /// not only the small fixtures the integration test covers. Separates
    /// "the planner built the wrong thing" from "a check is wrong about a
    /// thing the legacy emitter itself produced".
    #[test]
    fn realisation_reproduces_the_legacy_world_for_every_reference_circuit() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};

        let circuits = [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
        ];

        for (name, netlist) in circuits {
            let compiled = compile::compile_legacy(&netlist).expect("reference circuits compile");
            let seed = seed_from_legacy_parts(&netlist, compiled.legacy_emission().unwrap())
                .expect("compiled output must seed");
            let realised = emit_candidate(&seed, &netlist, compiled.world.size())
                .expect("a seed must be realisable");

            let mut differences = 0usize;
            for flat in 0..realised.world.cells().len() {
                let (x, y, z) = realised.world.decode(flat);
                if realised.world.get(x, y, z) != compiled.world.get(x, y, z) {
                    differences += 1;
                }
            }
            assert_eq!(differences, 0, "{name}: realisation differs from the legacy world");
        }
    }

    /// A route's recorded terminal style has to be the one the world holds.
    ///
    /// `DirectedDustIntoSupport` is a preference, not a decision:
    /// `plan_bent_path` overrides it whenever the run cannot reach the last
    /// cell without a refresh, and a repeater goes in instead. The emitter
    /// used to record what `resolve_directed_dust_terminals` asked for, so
    /// full_adder's `cin` claimed a dust terminal it never got -- a plan
    /// mispricing the very component the terminal choice exists to save.
    ///
    /// full_adder is the smallest circuit that shows it; and4 never does,
    /// which is why this went unnoticed.
    #[test]
    fn a_planned_terminal_style_is_what_the_world_holds() {
        use crate::circuits::full_adder::build_full_adder_netlist;

        let (netlist, _) = build_full_adder_netlist();
        let compiled = compile::compile_legacy(&netlist).expect("full_adder compiles");
        let seed = seed_from_legacy_parts(&netlist, compiled.legacy_emission().unwrap())
            .expect("compiled output must seed");

        verify_candidate(&seed, &netlist)
            .expect("a seed's terminals must describe its own blocks");
    }

    /// The cost model's primary term is delay, and delay is the one this
    /// project already measures.
    ///
    /// `critical_path_settle_model_game_ticks` is exact against the simulator
    /// for every reference circuit: torch delay times the gates and repeaters
    /// on the measured critical path. A candidate carries the same facts --
    /// its nodes say which are merges, its routes carry the blocks they lay --
    /// so it prices that quantity instead of summing route lengths and calling
    /// the result delay, which is what it used to do.
    ///
    /// and4's two gates and five repeaters are what `reference_circuits`
    /// prints and the simulator confirms tick for tick. If a layout change
    /// moves them this fails and they get re-measured, rather than drifting.
    #[test]
    fn candidate_delay_is_the_settle_model_the_simulator_confirms() {
        use crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS;

        let (netlist, _) = build_and4_netlist();
        let compiled = compile::compile_legacy(&netlist).expect("and4 compiles");
        let seed = seed_from_legacy_parts(&netlist, compiled.legacy_emission().unwrap())
            .expect("compiled output must seed");

        assert_eq!(seed.cost().delay, TORCH_DELAY_GAME_TICKS * (2 + 5));
    }

    /// The same assertion for a circuit with real fanout, and it is three
    /// short: 32 game ticks against a measured 38.
    ///
    /// Not a repeater-attribution problem any more. Each terminal now records
    /// the repeaters between it and its source, counted when the branch was
    /// laid, and the longest path over those weights is 9 gates and 7
    /// repeaters -- the same answer the graph walk this replaced gave, which
    /// is how we know attribution was never the cause. `timing` measures 10
    /// gates and 9 repeaters on the same circuit.
    ///
    /// So the two disagree about which path is critical, or about what an
    /// edge's repeaters include. `critical_path_repeaters` reads them out of
    /// `routing_stats`, which decomposes an edge into column, ramp, track and
    /// approach parts; reconciling that decomposition with what the emitter
    /// counts as it writes is the next step, and it needs reading, not
    /// guessing.
    #[test]
    #[ignore = "known: full_adder delay is three repeaters short"]
    fn candidate_delay_is_exact_for_a_circuit_with_fanout() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS;

        let (netlist, _) = build_full_adder_netlist();
        let compiled = compile::compile_legacy(&netlist).expect("full_adder compiles");
        let seed = seed_from_legacy_parts(&netlist, compiled.legacy_emission().unwrap())
            .expect("compiled output must seed");

        assert_eq!(seed.cost().delay, TORCH_DELAY_GAME_TICKS * (10 + 9));
    }

    /// The optimiser has to produce a circuit that is smaller, no slower, and
    /// still computes what it was asked to.
    ///
    /// Cost-model numbers are internal bookkeeping; blocks and settle ticks
    /// are what this project reports, and a truth table is what makes either
    /// worth reporting. Measured at 16 evaluations: 472 blocks and 18 game
    /// ticks become 405 and 16, with all sixteen rows correct. The tick saved
    /// is one repeater, and the cost model's delay term moves 14 -> 12 with
    /// it -- it did not, until a rerouted branch started refreshing the
    /// repeater count it reports.
    ///
    /// and4 is where this works. 30 of its 42 single-cell moves are legal;
    /// full_adder, one circuit up, has 5 of 80, and gains 4% of its wire and
    /// nothing else. The move set is what limits this, not the cost model.
    #[test]
    fn optimisation_makes_and4_smaller_without_breaking_it() {
        use crate::redstone::simulator::Simulator;
        use crate::redstone::world::block::BlockKind;

        let (netlist, _) = build_and4_netlist();
        let compiled = compile::compile_legacy(&netlist).expect("and4 compiles");
        let seed = seed_from_legacy_parts(&netlist, compiled.legacy_emission().unwrap())
            .expect("compiled output must seed");
        let best = optimise(
            seed.clone(),
            &netlist,
            PlannerWeights::default(),
            PlannerEffort {
                evaluations: 16,
                seed: 0x26_02,
            },
        );

        let measure = |candidate: &PlanCandidate| -> (usize, u64, usize) {
            let realised = realise_and_verify(candidate, &netlist, compiled.world.size())
                .expect("both candidates must be legal");
            let blocks = (0..realised.world.cells().len())
                .filter(|&flat| {
                    let (x, y, z) = realised.world.decode(flat);
                    realised.world.get(x, y, z).kind != BlockKind::Air
                })
                .count();

            let mut simulator = Simulator::new(realised.world.clone());
            simulator.run_until_stable(2000).expect("settles");
            let (mut worst, mut wrong) = (0u64, 0usize);
            for mask in 0u8..16 {
                for (bit, name) in ["a", "b", "c", "d"].iter().enumerate() {
                    let at = realised.ports.input_positions[*name];
                    let mut state = simulator.world().get(at.0, at.1, at.2).clone();
                    state.lit = (mask >> bit) & 1 == 1;
                    simulator.world_mut().set(at.0, at.1, at.2, state);
                }
                worst = worst.max(simulator.run_until_stable(2000).expect("settles"));
                let out = realised.ports.output_positions[&netlist.outputs[0]];
                if simulator.world().get(out.0, out.1, out.2).lit != (mask == 0b1111) {
                    wrong += 1;
                }
            }
            (blocks, worst, wrong)
        };

        let (seed_blocks, seed_settle, seed_wrong) = measure(&seed);
        let (best_blocks, best_settle, best_wrong) = measure(&best);

        assert_eq!(seed_wrong, 0, "the seed must compute and4");
        assert_eq!(best_wrong, 0, "optimisation must not change what the circuit computes");
        assert!(
            best_blocks < seed_blocks,
            "optimisation must save blocks: {seed_blocks} -> {best_blocks}"
        );
        assert!(
            best_settle <= seed_settle,
            "optimisation must not cost settle time: {seed_settle} -> {best_settle}"
        );
        assert!(
            best.cost().delay <= seed.cost().delay,
            "the delay term must follow the circuit it prices"
        );
    }

    /// The search has to move the way dust moves.
    ///
    /// `connectivity::dust_reach` says a dust cell reaches its four horizontal
    /// neighbours, and those neighbours one level up or one level down -- a
    /// staircase. It never reaches the cell directly above or below itself.
    /// A search that steps straight down produces exactly what it did before
    /// this: a run that descends three levels and carries nothing.
    #[test]
    fn the_search_steps_the_way_dust_does() {
        let here = Anchor { x: 4, y: 4, z: 4 };
        let steps = neighbours(here);

        assert!(
            !steps.iter().any(|step| step.x == here.x && step.z == here.z),
            "dust never reaches the cell directly above or below itself"
        );
        for step in &steps {
            let horizontal = (step.x - here.x).abs() + (step.z - here.z).abs();
            assert_eq!(horizontal, 1, "every step moves exactly one cell sideways");
            assert!(
                (step.y - here.y).abs() <= 1,
                "a staircase climbs or descends one level, never more"
            );
        }
        assert_eq!(
            steps.len(),
            12,
            "four directions, each level with the source or one step either way"
        );
    }

    /// How far the planner's own placement carries, measured rather than
    /// assumed.
    ///
    /// and4 and full_adder place, route and verify without the legacy emitter.
    /// segment_a does not, and the reason is a choice rather than a gap: a
    /// climb needs the cell above the one it leaves to be *empty*, and every
    /// route lays solid floors, so a route passing overhead seals another's
    /// way up while owning nothing that conducts. Enforcing that costs
    /// routability -- segment_a routed completely without it, and delivered
    /// nothing to six of its sinks.
    ///
    /// A router that lays dead circuits is worse than one that says it cannot,
    /// so the rule stays and the search is what has to improve.
    ///
    /// Re-run on 2026-08-14, now that placement is a relaxation: and4 and
    /// full_adder still carry, and segment_a still does not --
    /// `no safe local route from (90, 1, 109) to (96, 1, 105)`, after all
    /// sixty-four rip-up rounds. Relaxation compacted its anchor box from
    /// 29,435 cells to 8,099 and the search still cannot fill it. What did
    /// change is the wait: the whole run now fails in 77 seconds, where a
    /// single round used to be minutes.
    ///
    /// **Re-measured 2026-08-15 at this HEAD: the verdict holds and the
    /// address has moved.** segment_a now fails at `no safe local route from
    /// (99, 1, 97) to (122, 1, 89)`, reproducible run to run. Task 11 changed
    /// the placement between the two dates -- the ground plane, and the amount
    /// guard repaired by deletion -- so a different route being the one that
    /// cannot be found is what a moved placement looks like rather than a new
    /// defect. **NOT MEASURED:** that Task 11 is the cause. Nobody has re-run
    /// this against the pre-Task-11 tree; what is measured is the two
    /// addresses and their dates.
    ///
    /// This test panics on the first failure, so it stops here and has never
    /// reported seven_segment. `viewer`'s
    /// `which_reference_circuits_place_and_which_also_route` is the survey that
    /// does not stop, and it says seven_segment fails too --
    /// `no safe local route from (83, 1, 106) to (83, 1, 96)`. That is why two
    /// of the four circuits fingerprinted under wasm carry only the continuous
    /// fixture: `plan_from_netlist` never returns for them to snap.
    #[test]
    #[ignore = "known: segment_a needs a better search, not a looser rule"]
    fn how_far_the_planners_own_placement_carries() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };

        for (name, netlist) in [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
        ] {
            let candidate = plan_from_netlist(&netlist, &PortPlacements::default())
                .unwrap_or_else(|error| panic!("{name} must be placeable: {error}"));
            verify_candidate(&candidate, &netlist)
                .unwrap_or_else(|error| panic!("{name} must be legal: {error}"));
        }
    }

    /// One circuit the Task 13 condition names, and the independently-written
    /// truth table it has to match.
    ///
    /// `expected` is a function of the input bits rather than a re-run of the
    /// netlist, for the reason `tests/reference_circuits.rs` gives at the top
    /// of the file: a bug shared between the netlist builder and the checker
    /// cancels itself out. `inputs` is most-significant bit first, which is
    /// what makes `decoder_digit` below able to read a BCD digit off it.
    struct ConditionCircuit {
        name: &'static str,
        netlist: Netlist,
        inputs: &'static [&'static str],
        /// Netlist signal names, in the order `expected` returns their values.
        outputs: Vec<String>,
        expected: fn(&[bool]) -> Vec<bool>,
    }

    fn and4_expected(bits: &[bool]) -> Vec<bool> {
        vec![bits.iter().all(|&bit| bit)]
    }

    fn full_adder_expected(bits: &[bool]) -> Vec<bool> {
        let ones = bits.iter().filter(|&&bit| bit).count();
        vec![ones % 2 == 1, ones >= 2]
    }

    /// The BCD digit `bits` spells, most-significant bit first.
    fn decoder_digit(bits: &[bool]) -> usize {
        bits.iter().fold(0usize, |acc, &bit| acc * 2 + usize::from(bit))
    }

    /// Segments `a`..`g` for a digit, dark for 10..15 -- which is what both
    /// decoders in this tree are specified to do (`tests/fixtures/
    /// seven_segment.v`'s own comment, and `TRUTH_TABLE`'s ten rows).
    fn seven_segment_expected(bits: &[bool]) -> Vec<bool> {
        let digit = decoder_digit(bits);
        let table = crate::circuits::seven_segment::TRUTH_TABLE;
        (0..7).map(|segment| digit < table.len() && table[digit][segment] == 1).collect()
    }

    /// Segment `a` alone -- index 0 of `SEGMENT_NAMES`, which is the segment
    /// `build_single_segment_netlist(0)` builds.
    fn segment_a_expected(bits: &[bool]) -> Vec<bool> {
        vec![seven_segment_expected(bits)[0]]
    }

    /// Drive every input combination through the real simulator and compare
    /// each output against `expected`.
    ///
    /// `Ok(n)` is "n vectors, all of them right". Every failure is a `String`
    /// rather than a panic, because the harness below has five more circuits
    /// to report after this one.
    fn simulated_truth_table(
        compiled: &CompiledCircuit,
        inputs: &[&str],
        outputs: &[String],
        expected: fn(&[bool]) -> Vec<bool>,
    ) -> Result<usize, String> {
        const MAX_TICKS: u64 = 2000;

        let mut levers = Vec::with_capacity(inputs.len());
        for name in inputs {
            match compiled.input_positions.get(*name) {
                Some(position) => levers.push(*position),
                None => return Err(format!("compiled circuit has no lever for input `{name}`")),
            }
        }
        let mut sinks = Vec::with_capacity(outputs.len());
        for signal in outputs {
            match compiled.output_positions.get(signal) {
                Some(position) => sinks.push(*position),
                None => return Err(format!("compiled circuit has no output `{signal}`")),
            }
        }

        let mut simulator = crate::redstone::simulator::Simulator::new(compiled.world.clone());
        if let Err(error) = simulator.run_until_stable(MAX_TICKS) {
            return Err(format!("did not settle before the first reading: {error:?}"));
        }

        let mut wrong = 0usize;
        let mut first: Option<String> = None;
        let vectors = 1usize << inputs.len();
        for combination in 0..vectors {
            let bits: Vec<bool> = (0..inputs.len())
                .map(|index| (combination >> (inputs.len() - 1 - index)) & 1 == 1)
                .collect();
            for (position, &bit) in levers.iter().zip(bits.iter()) {
                let mut state = simulator.world().get(position.0, position.1, position.2).clone();
                state.lit = bit;
                simulator.world_mut().set(position.0, position.1, position.2, state);
                if let Err(error) = simulator.run_until_stable(MAX_TICKS) {
                    return Err(format!("did not settle at {bits:?}: {error:?}"));
                }
            }
            let want = expected(&bits);
            for (index, position) in sinks.iter().enumerate() {
                let got = simulator.world().get(position.0, position.1, position.2).lit;
                if got != want[index] {
                    wrong += 1;
                    first.get_or_insert(format!(
                        "{bits:?} -> `{}` expected {}, got {got}",
                        outputs[index], want[index]
                    ));
                }
            }
        }

        match first {
            None => Ok(vectors),
            Some(example) => {
                Err(format!("{wrong} readings wrong over {vectors} vectors, first: {example}"))
            }
        }
    }

    /// The six circuits Task 13's condition names, each carried as far through
    /// the shipping path as it gets -- reported, never asserted, and never
    /// stopping at the first failure.
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::the_six_condition_circuits_stage_by_stage \
    ///   -- --ignored --nocapture
    /// ```
    ///
    /// # Why six and not four
    ///
    /// The condition is "all four hand-written circuits **and both Verilog
    /// circuits** must place, route, verify, and match their truth tables".
    /// Everything that measured it before this measured four:
    /// `how_far_the_planners_own_placement_carries` above panics at segment_a
    /// and has therefore never reported seven_segment, and `viewer`'s
    /// `which_reference_circuits_place_and_which_also_route` swallows failures
    /// but still surveys the same four hand-written ones. The other two are
    /// the whole of [`crate::circuits::verilog::CIRCUITS`] -- `verilog:and4`
    /// and `verilog:seven_segment` -- and no measurement in this tree had ever
    /// run them through `plan_from_netlist` at all.
    ///
    /// They arrive by [`crate::circuits::verilog::VerilogCircuit::baked_netlist`]
    /// rather than by synthesis, so this needs no Yosys and gives the same
    /// answer on every machine; `tests/verilog_frontend.rs`'s
    /// `the_baked_netlists_match_fresh_synthesis` is what holds the baked copy
    /// equal to what Yosys produces. Each is then lowered by the same function
    /// its own acceptance test lowers it with -- `lower` for `verilog:and4`
    /// (`the_verilog_and4_matches_its_truth_table`), `lower_optimised` for
    /// `verilog:seven_segment`
    /// (`optimised_lowering_preserves_every_verilog_decoder_vector`) -- because
    /// a circuit compiled through a lowering nothing ships would answer a
    /// question nobody asked.
    ///
    /// # The four stages, reported separately
    ///
    /// Which stage a circuit reaches is the whole input to what happens next,
    /// so a row that fails says where:
    ///
    /// 1. **place** -- [`relaxed_placement`] converges and [`relax::snap`]
    ///    rounds it onto the lattice.
    /// 2. **route** -- [`plan_from_netlist`] returns a candidate, which is
    ///    stage 1 again plus `route_every_net`.
    /// 3. **verify** -- [`verify_candidate`], which *is* [`realise_and_verify`]
    ///    with the world dropped, so this covers both names.
    /// 4. **truth table** -- [`compile::compile_planned`], then the real
    ///    simulator over every input combination against a table written in
    ///    this module rather than read back out of the netlist.
    ///
    /// A stage runs only if the one before it passed; otherwise it prints
    /// `NOT REACHED`, which is a different statement from a failure and is
    /// kept a different word on purpose.
    ///
    /// # What it measured
    ///
    /// Run on 2026-08-15 at this HEAD, `--release`, whole run 54.8s:
    ///
    /// | circuit | lowered gates | bodies | place | route | verify | truth table |
    /// |---|---|---|---|---|---|---|
    /// | and4 | 7 | 11 | Ok | Ok | Ok | Ok, 16 vectors |
    /// | full_adder | 22 | 25 | Ok | Ok | Ok | Ok, 8 vectors |
    /// | segment_a | 46 | 50 | Ok 0.3s | **Err 31.2s** | NOT REACHED | NOT REACHED |
    /// | seven_segment | 84 | 88 | Ok 1.1s | **Err 20.6s** | NOT REACHED | NOT REACHED |
    /// | verilog:and4 | 9 | 13 | Ok | Ok | Ok | Ok, 16 vectors |
    /// | verilog:seven_segment | 47 | 74 | **Err 0.8s** | NOT REACHED | NOT REACHED | NOT REACHED |
    ///
    /// Three of the six go all the way. The other three fail in **two
    /// different places**, which is the thing this test was written to find
    /// out and the thing measuring only four circuits could not:
    ///
    /// * segment_a and seven_segment place and do not route, with
    ///   `no safe local route from (99, 1, 97) to (122, 1, 89)` and
    ///   `no safe local route from (83, 1, 106) to (83, 1, 96)`. That is the
    ///   frontier the ledger already knew about.
    /// * `verilog:seven_segment` **does not place at all**:
    ///   `projection deadlocked: bodies 2 and 3 cannot be 1.250 further apart
    ///   and stay welded`, out of [`relax::project`], in 0.8s. It is not the
    ///   biggest circuit here -- seven_segment has 84 gates to its 47 and
    ///   places fine -- so this is not a size wall.
    ///   `the_smallest_netlist_that_deadlocks_the_projection` below reduces it
    ///   to five gates and names the shape.
    ///
    /// Addresses are printed, not asserted: a route address that moves when
    /// placement moves is not a regression.
    ///
    /// Asserts nothing, so it can never fail and can never gate anything. It
    /// spends most of its time inside `route_every_net` failing twice over,
    /// which is why it is `#[ignore]`d.
    #[test]
    #[ignore = "measurement harness: asserts nothing, surveys all six condition circuits, takes minutes"]
    fn the_six_condition_circuits_stage_by_stage() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist, SEGMENT_NAMES,
        };
        use crate::compile::lowering::{lower, lower_optimised};
        use std::time::Instant;

        let (and4, and4_output) = build_and4_netlist();
        let (adder, adder_outputs) = build_full_adder_netlist();
        let (segment_a, segment_a_output) = build_single_segment_netlist(0);
        let (decoder, decoder_outputs) = build_seven_segment_netlist();

        let lowered_verilog = |name: &str, optimised: bool| -> (Netlist, Vec<String>) {
            let circuit = crate::circuits::verilog::find(name)
                .unwrap_or_else(|| panic!("{name} must be in the catalog"));
            let (netlist, labels) = circuit.baked_netlist();
            let lowered = if optimised { lower_optimised(&netlist) } else { lower(&netlist) }
                .unwrap_or_else(|error| panic!("{name} must lower: {error}"));
            (lowered, labels.into_iter().map(|(_, signal)| signal).collect())
        };
        let (verilog_and4, verilog_and4_outputs) = lowered_verilog("verilog:and4", false);
        let (verilog_decoder, verilog_decoder_outputs) =
            lowered_verilog("verilog:seven_segment", true);

        let cases = [
            ConditionCircuit {
                name: "and4",
                netlist: and4,
                inputs: &crate::circuits::and4::INPUT_NAMES[..],
                outputs: vec![and4_output],
                expected: and4_expected,
            },
            ConditionCircuit {
                name: "full_adder",
                netlist: adder,
                inputs: &crate::circuits::full_adder::INPUT_NAMES[..],
                outputs: vec![adder_outputs["sum"].clone(), adder_outputs["cout"].clone()],
                expected: full_adder_expected,
            },
            ConditionCircuit {
                name: "segment_a",
                netlist: segment_a,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: vec![segment_a_output],
                expected: segment_a_expected,
            },
            ConditionCircuit {
                name: "seven_segment",
                netlist: decoder,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: SEGMENT_NAMES.iter().map(|name| decoder_outputs[name].clone()).collect(),
                expected: seven_segment_expected,
            },
            ConditionCircuit {
                name: "verilog:and4",
                netlist: verilog_and4,
                inputs: &crate::circuits::and4::INPUT_NAMES[..],
                outputs: verilog_and4_outputs,
                expected: and4_expected,
            },
            ConditionCircuit {
                name: "verilog:seven_segment",
                netlist: verilog_decoder,
                // Most-significant bit first, which is `INPUT_NAMES`'s order
                // and not the order the baked file happens to declare them in.
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: verilog_decoder_outputs,
                expected: seven_segment_expected,
            },
        ];

        for case in &cases {
            let gates = case.netlist.gates.len();

            let started = Instant::now();
            let place = match relaxed_placement(
                &case.netlist,
                &PortPlacements::default(),
                SHIPPING_AXES,
            ) {
                Ok(placement) => match relax::snap(&placement) {
                    Ok(_) => Ok(format!(
                        "Ok {:.1}s ({} bodies, {} steps)",
                        started.elapsed().as_secs_f64(),
                        placement.graph.bodies.len(),
                        placement.iterations
                    )),
                    Err(error) => Err(format!(
                        "ERR after {:.1}s in snap: {error}",
                        started.elapsed().as_secs_f64()
                    )),
                },
                Err(error) => Err(format!(
                    "ERR after {:.1}s in relax: {error}",
                    started.elapsed().as_secs_f64()
                )),
            };

            let started = Instant::now();
            let (route, candidate) = match &place {
                Err(_) => ("NOT REACHED".to_string(), None),
                Ok(_) => match plan_from_netlist(&case.netlist, &PortPlacements::default()) {
                    Ok(candidate) => (
                        format!("Ok {:.1}s", started.elapsed().as_secs_f64()),
                        Some(candidate),
                    ),
                    Err(error) => (
                        format!("ERR after {:.1}s: {error}", started.elapsed().as_secs_f64()),
                        None,
                    ),
                },
            };

            let started = Instant::now();
            let verify = match &candidate {
                None => "NOT REACHED".to_string(),
                Some(candidate) => match verify_candidate(candidate, &case.netlist) {
                    Ok(()) => format!("Ok {:.1}s", started.elapsed().as_secs_f64()),
                    Err(error) => {
                        format!("ERR after {:.1}s: {error}", started.elapsed().as_secs_f64())
                    }
                },
            };

            let started = Instant::now();
            let truth = if candidate.is_none() || verify.starts_with("ERR") {
                "NOT REACHED".to_string()
            } else {
                match compile::compile_planned(&case.netlist, &PortPlacements::default()) {
                    Err(error) => format!(
                        "ERR after {:.1}s in compile_planned: {error}",
                        started.elapsed().as_secs_f64()
                    ),
                    Ok(compiled) => match simulated_truth_table(
                        &compiled,
                        case.inputs,
                        &case.outputs,
                        case.expected,
                    ) {
                        Ok(vectors) => format!(
                            "Ok {:.1}s ({vectors} vectors)",
                            started.elapsed().as_secs_f64()
                        ),
                        Err(error) => {
                            format!("ERR after {:.1}s: {error}", started.elapsed().as_secs_f64())
                        }
                    },
                }
            };

            let place = match place {
                Ok(text) | Err(text) => text,
            };
            eprintln!(
                "{}: {gates} lowered gates\n  place  {place}\n  route  {route}\n  verify {verify}\n  truth  {truth}",
                case.name
            );
        }
    }

    /// The shape that deadlocks the projection, reduced to the smallest
    /// netlist that shows it, with the reference circuits beside it as the
    /// evidence for why nothing found it until now.
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::the_smallest_netlist_that_deadlocks_the_projection \
    ///   -- --ignored --nocapture
    /// ```
    ///
    /// `the_six_condition_circuits_stage_by_stage` above found that
    /// `verilog:seven_segment` does not place: `projection deadlocked: bodies
    /// 2 and 3 cannot be 1.250 further apart and stay welded`. This is what
    /// that turned out to be.
    ///
    /// # The shape
    ///
    /// A wire merge whose branch needs isolating gets a repeater welded into
    /// the junction's socket for that branch ([`relax::Weld::AtSocket`], built
    /// in `relax::build`). [`relax::project`] exempts a **welded pair** from
    /// the separation rule, because a weld requires them to touch. Two
    /// repeaters welded into the *same* junction's two sockets are each welded
    /// to the junction and to nothing else, so `exempt` -- which asks only
    /// whether a weld relates the two bodies it is handed -- does not exempt
    /// them from each other. `satisfy` then puts them one cell either side of
    /// the junction. Measured on the minimal netlist below: junction at
    /// `[34, 1, 5]`, its two socket repeaters at `[33, 1, 5]` and `[35, 1, 5]`,
    /// each with a required separation of 3.250, and
    /// `worst_violation` reporting `{left: 3, right: 4, shortfall: 1.25}` --
    /// 2.0 apart where 3.25 is required. Neither can move without breaking its
    /// own weld. That is a contradiction, and `project` is right to report it
    /// rather than spin on it.
    ///
    /// The failure is therefore **a merge with both branches isolated**, and
    /// nothing about size. Measured on 2026-08-15 at this HEAD:
    ///
    /// | netlist | gates | merges | welds | junctions with both sockets welded | relax |
    /// |---|---|---|---|---|---|
    /// | and4 | 7 | 0 | 0 | 0 | Ok, 8 steps |
    /// | full_adder | 22 | 0 | 0 | 0 | Ok, 9 steps |
    /// | segment_a | 46 | 0 | 0 | 0 | Ok, 11 steps |
    /// | seven_segment | 84 | 0 | 0 | 0 | Ok, 11 steps |
    /// | verilog:and4 (`lower`) | 9 | 0 | 0 | 0 | Ok, 8 steps |
    /// | verilog:and4 (`lower_optimised`) | 7 | 2 | 0 | 0 | Ok, 8 steps |
    /// | verilog:seven_segment (`lower`) | 56 | 17 | 20 | 7 | **deadlocked at 14/15** |
    /// | verilog:seven_segment (`lower_optimised`) | 47 | 17 | 23 | 9 | **deadlocked at 2/3** |
    /// | minimal, below | 5 | 1 | 2 | 1 | **deadlocked at 3/4** |
    /// | control, below | 4 | 1 | 1 | 0 | Ok, 8 steps |
    ///
    /// The row that makes the column the right one is `verilog:and4` under
    /// `lower_optimised`: two merges, and it places. Merges are not the
    /// trigger; **two isolating repeaters on one junction** is. In both
    /// Verilog decoder lowerings the pair `project` names is the first
    /// double-socket junction in its list, the minimal netlist reproduces it
    /// at five gates, and the control -- the same netlist with one fan-out
    /// removed, so the merge has one isolated branch instead of two -- has one
    /// weld, no deadlock, and `compile_planned` Ok.
    ///
    /// # Why no test caught it
    ///
    /// Every hand-written circuit in this tree lowers to pure NOR -- look at
    /// the histograms in the first four rows, there is not one `merge2` among
    /// them -- and the only Verilog circuit whose lowering produces a merge
    /// that needs *isolating* is the decoder. `project`'s own
    /// `constraints_that_contradict_are_reported_rather_than_spun_on` builds
    /// this exact two-weld shape by hand and forces it with a separation of
    /// 9.0, so the *mechanism* was modelled from the start; what had never
    /// been measured is that a circuit this project ships reaches it at the
    /// real separation.
    ///
    /// # What it costs
    ///
    /// The minimal netlist compiles today: `compile()` takes the legacy
    /// row/channel/track path, which has no projection to deadlock. So does
    /// `verilog:seven_segment` -- `tests/verilog_frontend.rs`'s
    /// `optimised_lowering_preserves_every_verilog_decoder_vector` passes at
    /// this HEAD, 47 gates and 10,088 blocks and every vector. Pointing
    /// `compile()` at the planner today would turn both of those from
    /// compiling into not placing.
    ///
    /// Asserts nothing -- it is the method behind the two tables above, kept
    /// in the tree so the numbers can be re-run rather than trusted.
    #[test]
    #[ignore = "measurement: prints the deadlock's shape, asserts nothing"]
    fn the_smallest_netlist_that_deadlocks_the_projection() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };
        use crate::compile::lowering::{lower, lower_optimised};

        let circuit = crate::circuits::verilog::find("verilog:seven_segment").unwrap();
        let (decoder_source, _) = circuit.baked_netlist();
        let and4_circuit = crate::circuits::verilog::find("verilog:and4").unwrap();
        let (and4_source, _) = and4_circuit.baked_netlist();

        for (label, lowered) in [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
            ("verilog:and4 (lower)", lower(&and4_source).unwrap()),
            (
                "verilog:and4 (lower_optimised)",
                lower_optimised(&and4_source).unwrap(),
            ),
            ("verilog:seven_segment (lower)", lower(&decoder_source).unwrap()),
            (
                "verilog:seven_segment (lower_optimised)",
                lower_optimised(&decoder_source).unwrap(),
            ),
        ] {
            eprintln!("=== {label}: {} gates ===", lowered.gates.len());
            eprintln!(
                "  histogram {}",
                crate::compile::lowering::format_histogram(&lowered)
            );
            let graph = primitive_graph::expand(&lowered, &Library::default_library()).unwrap();
            let start = starting_layout(&lowered, &PortPlacements::default()).unwrap();
            let built = relax::build(&lowered, &graph, &start, &PortPlacements::default());
            match built {
                Ok(body_graph) => {
                    let mut per_junction: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
                    for weld in &body_graph.welds {
                        if let relax::Weld::AtSocket { repeater, junction, .. } = weld {
                            per_junction.entry(*junction).or_default().push(*repeater);
                        }
                    }
                    let both: Vec<_> =
                        per_junction.iter().filter(|(_, r)| r.len() >= 2).collect();
                    eprintln!(
                        "  {} bodies, {} welds over {} junctions, {} of them with BOTH sockets welded: {:?}",
                        body_graph.bodies.len(),
                        body_graph.welds.len(),
                        per_junction.len(),
                        both.len(),
                        both,
                    );
                }
                Err(error) => eprintln!("  build ERR: {error}"),
            }
            match relaxed_placement(&lowered, &PortPlacements::default(), SHIPPING_AXES) {
                Ok(placement) => eprintln!(
                    "  relax Ok: {} bodies, {} steps",
                    placement.graph.bodies.len(),
                    placement.iterations
                ),
                Err(error) => eprintln!("  relax ERR: {error}"),
            }
        }

        // Minimal reproduction: one merge, both branches isolated because both
        // its input signals fan out elsewhere.
        let minimal = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["m".to_string(), "ka".to_string(), "kb".to_string()],
            gates: vec![
                Gate::nor("na", &["a"]),
                Gate::nor("nb", &["b"]),
                Gate {
                    name: "m".to_string(),
                    inputs: vec!["na".to_string(), "nb".to_string()],
                    output: "m".to_string(),
                    kind: crate::compile::topology::GateKind::Or(2),
                },
                Gate::nor("ka", &["na"]),
                Gate::nor("kb", &["nb"]),
            ],
        };
        eprintln!("=== minimal: one merge, both branches isolated ===");
        let graph = primitive_graph::expand(&minimal, &Library::default_library()).unwrap();
        let start = starting_layout(&minimal, &PortPlacements::default()).unwrap();
        let built = relax::build(&minimal, &graph, &start, &PortPlacements::default()).unwrap();
        eprintln!("  {} bodies, welds {:?}", built.bodies.len(), built.welds);
        let required = relax::required_separations(&built);
        for index in [2usize, 3, 4] {
            eprintln!(
                "    body {index}: {:?} at {:?}, required separation {:.3}",
                built.bodies[index].what, built.bodies[index].position, required[index]
            );
        }
        eprintln!("    worst violation as built: {:?}", relax::worst_violation(&built, &required));
        match relaxed_placement(&minimal, &PortPlacements::default(), SHIPPING_AXES) {
            Ok(placement) => eprintln!("  relax Ok: {} steps", placement.iterations),
            Err(error) => eprintln!("  relax ERR: {error}"),
        }
        match compile::compile_planned(&minimal, &PortPlacements::default()) {
            Ok(_) => eprintln!("  compile_planned Ok"),
            Err(error) => eprintln!("  compile_planned ERR: {error}"),
        }
        // `compile_legacy` and not `compile`: this line is here to say that the
        // emitter builds what the planner deadlocks on, and since the hybrid
        // landed `compile` would answer that question by falling back to the
        // emitter and reporting Ok either way.
        match compile::compile_legacy(&minimal) {
            Ok(_) => eprintln!("  legacy compile Ok"),
            Err(error) => eprintln!("  legacy compile ERR: {error}"),
        }

        // And the control: the same merge with only ONE branch isolated.
        let one_branch = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["m".to_string(), "ka".to_string()],
            gates: vec![
                Gate::nor("na", &["a"]),
                Gate::nor("nb", &["b"]),
                Gate {
                    name: "m".to_string(),
                    inputs: vec!["na".to_string(), "nb".to_string()],
                    output: "m".to_string(),
                    kind: crate::compile::topology::GateKind::Or(2),
                },
                Gate::nor("ka", &["na"]),
            ],
        };
        eprintln!("=== control: one merge, one branch isolated ===");
        let graph = primitive_graph::expand(&one_branch, &Library::default_library()).unwrap();
        let start = starting_layout(&one_branch, &PortPlacements::default()).unwrap();
        let built = relax::build(&one_branch, &graph, &start, &PortPlacements::default()).unwrap();
        eprintln!("  {} bodies, welds {:?}", built.bodies.len(), built.welds);
        match relaxed_placement(&one_branch, &PortPlacements::default(), SHIPPING_AXES) {
            Ok(placement) => eprintln!("  relax Ok: {} steps", placement.iterations),
            Err(error) => eprintln!("  relax ERR: {error}"),
        }
        match compile::compile_planned(&one_branch, &PortPlacements::default()) {
            Ok(_) => eprintln!("  compile_planned Ok"),
            Err(error) => eprintln!("  compile_planned ERR: {error}"),
        }
    }

    /// Corridors exist: a relaxed placement is not merely legal but routable.
    ///
    /// This is what the routing reservation claims, and it is the term with no
    /// precedent to lean on -- legacy reserves routing space by construction and
    /// this does it by a number that was guessed.
    ///
    /// "Could reach from the old placement" rather than "every sink" because
    /// segment_a and above do not route today whatever places them, and this test
    /// is about placement.
    #[test]
    fn relaxation_routes_everything_the_old_placement_could() {
        use crate::circuits::full_adder::build_full_adder_netlist;

        for (name, netlist) in [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
        ] {
            let candidate = plan_from_netlist(&netlist, &PortPlacements::default())
                .unwrap_or_else(|error| panic!("{name} must place: {error}"));
            verify_candidate(&candidate, &netlist)
                .unwrap_or_else(|error| panic!("{name} must be legal: {error}"));
        }
    }

    /// Better than what it replaced, on both counts.
    ///
    /// Both, because rows and barycentres are already smaller than they were and
    /// slower than the emitter -- beating one by giving up the other is not an
    /// improvement, it is a different trade.
    ///
    /// **Both baselines were re-measured on 2026-08-14 against the layout this
    /// task deleted**, by rebuilding it (`rows_and_barycentres`, below: the
    /// same `starting_layout` anchors, every node north, through the same
    /// `route_every_net`) and pricing both candidates the same way in
    /// `measure_and4_both_ways`. Rows and barycentres: **572 blocks, delay 22**,
    /// wire 268, space 12,285. Relaxation: **232 blocks, delay 10**, wire 98,
    /// space 3,105.
    ///
    /// The delay bound is 22 and not the 24 this task was set. The 24 is a
    /// *simulated* worst settle -- what `a_self_placed_and4_computes_and4`
    /// prints, over its two sixteen-step sweeps -- and `cost().delay` is the
    /// plan's own critical-path term, which was already 22 before relaxation
    /// touched anything. Bounded at 24 this assertion passes against the layout
    /// it is supposed to beat, which is a test that cannot fail against the
    /// defect it is named for; bounded at 22 it fails against it by 12.
    ///
    /// **On the simulated number, relaxation appeared to lose: 24 game ticks
    /// became 28.** It did not. That reading was an artefact of the sweep it
    /// came from, and re-measuring settled it the other way: over every ordered
    /// transition, each timed from a state that has actually settled, rows and
    /// barycentres take **26** and relaxation takes **14**, both at the same
    /// transition. `a_self_placed_and4_computes_and4` measures that now and its
    /// doc carries the working; the short version is that chaining transitions
    /// on one long-lived simulator reports a number the circuit does not have,
    /// and the pair the chained run called 38 takes 4 when timed properly.
    ///
    /// So the simulated figure agrees in direction with `cost().delay`'s 22 to
    /// 10 rather than contradicting it. This is still not
    /// `timing::summarize_worst_case`, which also reports glitches and which
    /// neither layout has been through.
    #[test]
    fn relaxation_places_and4_smaller_and_faster_than_rows_and_barycentres() {
        // planner.rs imports `BlockState`, not `BlockKind`; the two existing tests
        // that count non-air cells import it per function, and so does this one.
        use crate::redstone::world::block::BlockKind;

        let (netlist, _) = build_and4_netlist();
        let candidate = plan_from_netlist(&netlist, &PortPlacements::default()).expect("places");
        let realised = realise_and_verify(&candidate, &netlist, candidate_world_size(&candidate))
            .expect("is legal");

        // Counted the way `the_hand_written_circuits_keep_their_measured_size`
        // counts, so the numbers mean the same thing as the 472 it pins.
        let (size_x, size_y, size_z) = realised.world.size();
        let mut blocks = 0usize;
        for x in 0..size_x {
            for y in 0..size_y {
                for z in 0..size_z {
                    if realised.world.get(x, y, z).kind != BlockKind::Air {
                        blocks += 1;
                    }
                }
            }
        }
        // The candidate's own delay, in game ticks -- the term
        // `measure_optimisation_at_scale` prints.
        let settle = candidate.cost().delay;

        // Rows and barycentres, re-measured 2026-08-14 by the method the doc
        // above states.
        assert!(blocks < 572, "relaxation placed {blocks} blocks against 572");
        assert!(settle < 22, "relaxation's delay term is {settle} against 22");
    }

    /// The row-and-barycentre candidate this task replaced, rebuilt so the
    /// two can be measured against each other on the same day.
    fn rows_and_barycentres(netlist: &Netlist) -> PlanCandidate {
        let anchors = starting_layout(netlist, &PortPlacements::default()).expect("lays");
        let mut nodes = Vec::new();
        for (index, gate) in netlist.gates.iter().enumerate() {
            let anchor = anchors[index];
            let (footprint, conductors, output_pin) = compile::gate_footprint(
                (anchor.x, anchor.y, anchor.z),
                gate,
                geometry::CellFacing::NORTH,
            );
            nodes.push(PrimitiveNode {
                id: format!("gate:{}", gate.output),
                anchor,
                realisation: if gate.is_merge() {
                    NodeRealisation::WireMerge
                } else {
                    NodeRealisation::Primitive(Primitive::Torch)
                },
                footprint,
                conductors,
                pinned: false,
                output_pin: Some(output_pin),
            });
        }
        for (index, input) in netlist.inputs.iter().enumerate() {
            let anchor = anchors[netlist.gates.len() + index];
            let (cells, pin) =
                compile::lever_footprint(anchor, compile::geometry::CellFacing::NORTH);
            nodes.push(PrimitiveNode {
                id: format!("input:{input}"),
                anchor,
                realisation: NodeRealisation::Primitive(Primitive::Lever),
                footprint: cells.clone(),
                conductors: cells,
                pinned: false,
                output_pin: Some(pin),
            });
        }
        route_every_net(
            PlanCandidate::with_primitive_nodes(anchors, nodes, Vec::new()),
            netlist,
            RIP_UP_ROUNDS,
        )
        .expect("rows and barycentres place and4")
    }

    /// The method behind every before/after number this branch cites for and4:
    /// what rows and barycentres built, and what relaxation builds, measured
    /// side by side on one run.
    ///
    /// Ignored because it asserts nothing, not because it is slow -- it takes a
    /// second. It exists because the standard here is that a cited number has a
    /// reproducible method, and its four `expect`s are real guards: that the
    /// old placement still builds, that the new one still places, and that both
    /// still realise and verify.
    ///
    /// Its settle column used to be a 32-mask sweep on one long-lived
    /// simulator, and printed 24 against 28 -- a figure `8c0b70d` showed is not
    /// a property of the circuit at all. It times what
    /// `a_self_placed_and4_computes_and4` times now, so the two agree: every
    /// ordered transition, each from a state that has settled, on a simulator
    /// that has seen nothing else.
    #[test]
    #[ignore = "measurement harness: asserts nothing, prints the cited numbers"]
    fn measure_and4_both_ways() {
        use crate::redstone::simulator::Simulator;
        use crate::redstone::world::block::BlockKind;

        let (netlist, _) = build_and4_netlist();
        for (name, candidate) in [
            ("rows+barycentres", rows_and_barycentres(&netlist)),
            (
                "relaxation",
                plan_from_netlist(&netlist, &PortPlacements::default()).expect("places"),
            ),
        ] {
            let realised =
                realise_and_verify(&candidate, &netlist, candidate_world_size(&candidate))
                    .expect("legal");
            let blocks = (0..realised.world.cells().len())
                .filter(|&flat| {
                    let (x, y, z) = realised.world.decode(flat);
                    realised.world.get(x, y, z).kind != BlockKind::Air
                })
                .count();

            let set_inputs = |simulator: &mut Simulator, mask: u8| {
                for (bit, port) in ["a", "b", "c", "d"].iter().enumerate() {
                    let at = realised.ports.input_positions[*port];
                    let mut state = simulator.world().get(at.0, at.1, at.2).clone();
                    state.lit = (mask >> bit) & 1 == 1;
                    simulator.world_mut().set(at.0, at.1, at.2, state);
                }
            };
            let mut worst = 0u64;
            let mut worst_at = (0u8, 0u8);
            for from in 0u8..16 {
                for to in 0u8..16 {
                    if from == to {
                        continue;
                    }
                    let mut simulator = Simulator::new(realised.world.clone());
                    set_inputs(&mut simulator, from);
                    simulator.run_until_stable(2000).expect("settles at from");
                    set_inputs(&mut simulator, to);
                    let ticks = simulator.run_until_stable(2000).expect("settles at to");
                    if ticks > worst {
                        worst = ticks;
                        worst_at = (from, to);
                    }
                }
            }

            let cost = candidate.cost();
            eprintln!(
                "{name}: {blocks} blocks | cost delay {} wire {} space {} turns {} | \
                 worst settle {worst} over 240 transitions, at {:04b} -> {:04b}",
                cost.delay, cost.wire, cost.space, cost.turns, worst_at.0, worst_at.1
            );
        }
    }

    /// What relaxation does to each reference circuit's anchor bounding box,
    /// start against finish. The method behind the four before/after pairs the
    /// ledger cites; like `measure_and4_both_ways` it asserts nothing, and it
    /// swallows a `relax` failure rather than panicking so one circuit that
    /// cannot place does not hide the other three.
    #[test]
    #[ignore = "measurement harness: asserts nothing, prints the cited numbers"]
    fn measure_anchor_boxes() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };
        use crate::compile::primitive_graph::expand;

        let box_of = |anchors: &[Anchor]| -> (i32, i32, i64) {
            let mut min = (i32::MAX, i32::MAX);
            let mut max = (i32::MIN, i32::MIN);
            for a in anchors {
                min = (min.0.min(a.x), min.1.min(a.z));
                max = (max.0.max(a.x), max.1.max(a.z));
            }
            let (w, d) = (max.0 - min.0 + 1, max.1 - min.1 + 1);
            (w, d, w as i64 * d as i64)
        };

        for (name, netlist) in [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
        ] {
            let start = starting_layout(&netlist, &PortPlacements::default()).unwrap();
            let (sw, sd, sa) = box_of(&start);
            let graph = expand(&netlist, &Library::default_library()).unwrap();
            let placement = relax::relax(
                &netlist,
                &graph,
                &start,
                &PortPlacements::default(),
                relax::Axes::IN_PLANE,
                relax::RelaxEffort::default(),
            );
            match placement {
                Ok(placement) => {
                    let snapped = relax::snap(&placement).expect("rounds");
                    let anchors: Vec<Anchor> = snapped.iter().map(|node| node.anchor).collect();
                    let (rw, rd, ra) = box_of(&anchors);
                    let turned = snapped
                        .iter()
                        .filter(|node| node.facing != geometry::CellFacing::NORTH)
                        .count();
                    eprintln!(
                        "{name}: start {sw}x{sd}={sa} -> relaxed {rw}x{rd}={ra} \
                         in {} step(s), {turned}/{} nodes turned",
                        placement.iterations,
                        snapped.len()
                    );
                }
                Err(error) => eprintln!("{name}: start {sw}x{sd}={sa} -> relax failed: {error}"),
            }
        }
    }

    /// How far a converged placement may drift before
    /// [`placement_fingerprint`] notices -- the method behind the "about a
    /// fortieth of a cell" that function's doc cites, and the reason
    /// [`continuous_placement_fingerprint`] exists next to it.
    ///
    /// Two margins, because there are two ways a fingerprint can change:
    ///
    /// - **rounding.** `snap` rounds each solved axis, so a coordinate must
    ///   move to within half a cell of the boundary before the printed integer
    ///   moves. `0.5 - |p - round(p)|` per body per solved axis, smallest
    ///   reported. Y is skipped: `SHIPPING_AXES` never solves it, so its margin
    ///   is a constant that would hide the two real ones.
    /// - **the facing argmin.** `choose_facings` picks each body's facing as a
    ///   strict `<` over four energies, and *that* has no boundary to cross --
    ///   a last-bit difference between the best and the runner-up flips the
    ///   fifth column outright. Smallest `(runner_up - best)` over all bodies,
    ///   absolute and relative to `best`, plus a count of exact ties (a tie
    ///   goes to the lower index on both toolchains, so it is safe, but it is
    ///   the configuration worth knowing about).
    ///
    /// Asserts nothing, like its three sibling harnesses, and swallows a
    /// `relax` failure so one circuit that cannot place does not hide the rest.
    #[test]
    #[ignore = "measurement harness: asserts nothing, prints the cited numbers"]
    fn measure_snapped_fingerprint_slack() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };

        for (name, netlist) in [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
        ] {
            let placement =
                match relaxed_placement(&netlist, &PortPlacements::default(), SHIPPING_AXES) {
                    Ok(placement) => placement,
                    Err(error) => {
                        eprintln!("{name}: relax failed: {error}");
                        continue;
                    }
                };

            let mut rounding = f64::INFINITY;
            for body in &placement.graph.bodies {
                for axis in SHIPPING_AXES.iter() {
                    let p = body.position[axis];
                    rounding = rounding.min(0.5 - (p - p.round()).abs());
                }
            }

            let mut graph = placement.graph.clone();
            let (mut absolute, mut relative, mut ties) = (f64::INFINITY, f64::INFINITY, 0usize);
            for body in 0..graph.bodies.len() {
                let was = graph.bodies[body].facing;
                let mut energies = Vec::with_capacity(4);
                for index in 0..4u8 {
                    graph.bodies[body].facing =
                        geometry::CellFacing::from_index(index).expect("0..4 is horizontal");
                    energies.push(relax::incident_energy_for_test(&graph, body));
                }
                graph.bodies[body].facing = was;
                energies.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a converged graph"));
                let (best, runner_up) = (energies[0], energies[1]);
                if runner_up == best {
                    ties += 1;
                }
                absolute = absolute.min(runner_up - best);
                if best != 0.0 {
                    relative = relative.min((runner_up - best) / best.abs());
                }
            }

            eprintln!(
                "{name}: {} bodies | tightest rounding margin {rounding:.6} cell(s) | \
                 tightest facing margin {absolute:.6e} absolute, {relative:.6e} relative | \
                 {ties} exact tie(s) | f64::EPSILON {:.6e}",
                placement.graph.bodies.len(),
                f64::EPSILON
            );
        }
    }

    /// What `Axes::ALL` does to every reference circuit, end to end: whether it
    /// places, whether the placement is legal, how many storeys it spent, and
    /// what it cost.
    ///
    /// **The way to re-run `relax::VERTICAL_CLEARANCE`'s table.** Set that one
    /// constant and run this; each row of the table is one value of it. It goes
    /// through `plan_with_axes`, which is `plan_from_netlist`'s whole body --
    /// not a copy of it, because a copy is a thing that drifts and this harness
    /// exists to say what the shipping path would do.
    ///
    /// Both axis sets on every circuit, so a failure under `ALL` can be read
    /// against what the shipping path does with the same netlist -- otherwise
    /// `segment_a` and `seven_segment`, which have never routed from this
    /// planner's own placement either way, read as regressions of this task.
    ///
    /// Like the other two harnesses it asserts nothing and swallows failures,
    /// so one circuit that cannot place does not hide the rest. `six_gates` is
    /// `six_independent_gates` itself -- the netlist `crowding_produces_height`
    /// is written against -- so the row above and the ignored test cannot drift
    /// apart about what "crowded" means.
    #[test]
    #[ignore = "measurement harness: asserts nothing, prints the cited numbers, and takes about 75 seconds"]
    fn measure_axes_all_against_the_reference_circuits() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };

        eprintln!(
            "VERTICAL_CLEARANCE = {}, GROUND = {}",
            relax::VERTICAL_CLEARANCE,
            relax::GROUND
        );
        for (name, netlist) in [
            ("six_gates", six_independent_gates()),
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
        ] {
            // What the horizontal requirement actually ranges over, because the
            // first version of this task's finding asserted it was "under 4 for
            // every degree in these circuits" without measuring it, and it is
            // not: degree reaches 9 and the requirement reaches 5.25. Y is
            // charged VERTICAL_CLEARANCE flat, so the count of pairs requiring
            // more than that is the count `cheapest_axis` could prefer Y for.
            if let Ok(start) = starting_layout(&netlist, &PortPlacements::default()) {
                if let Ok(graph) = primitive_graph::expand(&netlist, &Library::default_library())
                    .map_err(|e| e.to_string())
                    .and_then(|graph| {
                        relax::build(&netlist, &graph, &start, &PortPlacements::default())
                    })
                {
                    let required = relax::required_separations(&graph);
                    let low = required.iter().copied().fold(f64::INFINITY, f64::min);
                    let high = required.iter().copied().fold(0.0, f64::max);
                    let dearer =
                        required.iter().filter(|&&r| r > relax::VERTICAL_CLEARANCE).count();
                    eprintln!(
                        "         {name}: {} bodies, requirement {low}..{high}, \
                         {dearer} dearer than VERTICAL_CLEARANCE",
                        required.len()
                    );
                }
            }

            for (axes, label) in
                [(relax::Axes::IN_PLANE, "IN_PLANE"), (relax::Axes::ALL, "ALL     ")]
            {
                match plan_with_axes(
                    &netlist,
                    &PortPlacements::default(),
                    axes,
                    RIP_UP_ROUNDS,
                    SHIPPING_ROUTER,
                    PresentSchedule::SHIPPING,
                ) {
                    Ok(candidate) => {
                        let (x, y, z) = extent(&candidate);
                        let legal = match verify_candidate(&candidate, &netlist) {
                            Ok(()) => "legal".to_string(),
                            Err(error) => format!("ILLEGAL {error}"),
                        };
                        let cost = candidate.cost();
                        // The levels bodies actually sit on, and how many are
                        // off the lowest one. `extent`'s Y is a bounding box
                        // that counts the empty clearance gap, so a pair two
                        // apart reads as height 3 with nothing stacked -- which
                        // is how the first reading of this table came to say
                        // "stacks to 3 storeys" about a layout whose only
                        // raised body was a lever.
                        let mut levels: Vec<i32> =
                            candidate.anchors().iter().map(|anchor| anchor.y).collect();
                        levels.sort_unstable();
                        let floor = levels[0];
                        levels.dedup();
                        // Named, not counted. Whether the raised body is a gate
                        // or a lever is the difference between "crowding stacks
                        // logic" and "crowding lifts a port", and the two read
                        // identically in a count.
                        let raised: Vec<String> = candidate
                            .primitive_nodes()
                            .iter()
                            .filter(|node| node.anchor.y > floor)
                            .map(|node| format!("{}@{}", node.id, node.anchor.y))
                            .collect();
                        eprintln!(
                            "{label} {name}: placed {x}x{y}x{z}, delay {} wire {} -- {legal}; \
                             levels {levels:?}, off the floor: {raised:?}",
                            cost.delay, cost.wire
                        );
                    }
                    Err(error) => eprintln!("{label} {name}: FAILED {error}"),
                }
            }
        }
    }

    /// A rebuilt branch aims at the socket the netlist declared, not at the one
    /// the approach direction implies.
    ///
    /// Until Task 10 nothing could tell those two apart: every gate faced
    /// north, so `input_directions(NORTH)[input_index]` and `terminal_socket`'s
    /// source-to-support delta named the same cell for every sink in the tree,
    /// and `try_move` rebuilt to the right place by coincidence. Relaxation
    /// turns gates, and then they name different cells -- while `route_in_order`
    /// still lays dust to the declared one and `equivalence` still checks that
    /// one. A rebuild aiming at the guess reroutes into a face the gate does
    /// not read.
    ///
    /// Both halves are load-bearing. The `assert_eq!` is the claim; the
    /// disagreement count is what stops this being a test that would pass
    /// against `terminal_socket` just as happily, and it is asserted rather
    /// than assumed because whether the two differ at all depends on what
    /// relaxation happened to turn. Every fixture in this module that builds
    /// through `with_primitive_nodes` faces north throughout and would report
    /// zero.
    #[test]
    fn a_rebuilt_branch_aims_at_the_socket_the_netlist_declared() {
        let (netlist, _) = build_and4_netlist();
        let candidate = plan_from_netlist(&netlist, &PortPlacements::default()).expect("places");

        let mut disagreements = 0usize;
        let mut checked = 0usize;
        for route in candidate.routes() {
            let owner = route.owner().expect("every routed net names its source");
            let source = candidate
                .primitive_nodes()
                .iter()
                .find(|node| node.id == format!("gate:{owner}") || node.id == format!("input:{owner}"))
                .map(|node| node.source())
                .expect("and that source is a node");
            for terminal in route.terminals() {
                let support = candidate
                    .node_for_gate(&terminal.sink.gate)
                    .expect("a terminal's sink is a gate");
                checked += 1;
                assert_eq!(
                    candidate.declared_socket(support, &terminal.sink),
                    terminal.sink.anchor,
                    "{owner} -> {}.in[{}]: the rebuild would aim somewhere the router did not lay",
                    terminal.sink.gate,
                    terminal.sink.input_index,
                );
                if terminal_socket(source, support) != terminal.sink.anchor {
                    disagreements += 1;
                }
            }
        }

        assert!(checked > 0, "and4 has terminals, so something went wrong finding them");
        assert!(
            disagreements > 0,
            "the geometric guess agreed with the declared socket at all {checked} terminal(s), \
             so this test cannot tell the two apart"
        );
    }

    /// ...and `try_move` is what has to *use* that answer, which is a separate
    /// claim from `declared_socket` computing it.
    ///
    /// The test above pins the lookup; this one pins the plumbing --
    /// `route_endpoints` carrying each branch's socket out beside its support,
    /// because the rebuild loop has no `RouteSink` to derive one from. Reverting
    /// that arm to `terminal_socket(source, support)` leaves every assertion in
    /// this suite standing except this one: measured 2026-08-14, the whole
    /// `--lib` run still passed, and took 103 seconds instead of 2 because A*
    /// was aiming into faces the gates do not read.
    ///
    /// The move is the turned gate's own, one cell along Z, because a gate's
    /// own move is what rebuilds both the routes into it and the route out.
    ///
    /// It asserts where the terminals landed and stops there, deliberately: this
    /// particular move produces an illegal candidate for a reason that has
    /// nothing to do with sockets and predates this task -- `try_move` rebuilds
    /// only *incident* routes, so a non-incident one keeps a terminal style the
    /// new geometry has invalidated (`g2 -> g3.in[2]`, style
    /// `RepeaterIntoSupport` against a realised `RedstoneWire`).
    /// `a_moved_candidate_can_be_built_and_verified` is the test that asks for
    /// legality, and it searches with `movable_target` for a move that has it.
    #[test]
    fn a_move_on_a_turned_placement_reroutes_to_the_declared_socket() {
        let (netlist, _) = build_and4_netlist();
        let candidate = plan_from_netlist(&netlist, &PortPlacements::default()).expect("places");
        let turned = (0..netlist.gates.len())
            .find(|&gate| candidate.facing_of(gate) != geometry::CellFacing::NORTH)
            .expect("relaxation turns something in and4, or this test proves nothing");

        let to = Anchor { z: candidate.anchors()[turned].z - 1, ..candidate.anchors()[turned] };
        let moved = try_move(&candidate, turned, to).expect("one cell north is a legal move");

        let mut rebuilt = 0usize;
        for route in moved.routes() {
            for terminal in route.terminals() {
                let support = moved
                    .node_for_gate(&terminal.sink.gate)
                    .expect("a terminal's sink is a gate");
                assert_eq!(
                    terminal.sink.anchor,
                    moved.declared_socket(support, &terminal.sink),
                    "{}.in[{}] was rebuilt to a cell the gate does not read",
                    terminal.sink.gate,
                    terminal.sink.input_index,
                );
                rebuilt += 1;
            }
        }
        assert!(rebuilt > 0, "the move rebuilt nothing, so nothing was checked");
    }

    /// A gate owns the cell above its torch, because a lit torch strongly
    /// powers it and a strongly powered block drives every dust beside it.
    ///
    /// Nothing else keeps a route off it. `gate_footprint` finds a gate's cells
    /// by realising it into a scratch world, and that cell is air there;
    /// `verify_spacing` proves ownership of *routed* cells and this one is not
    /// routed. So a route standing on it -- realisation writes its floor there
    /// as stone -- reads 15 out of a gate it never connected to, and every
    /// invariant passes.
    ///
    /// Found by relaxation, not invented for it: rows sixteen cells apart never
    /// needed to fly over a gate. See `gate_footprint`'s own comment for the
    /// full_adder measurement.
    ///
    /// Asserted as a *conductor* rather than merely occupied, because that is
    /// what makes `anchor_is_free_for` refuse it: its floor test asks for a
    /// conductor below, and an inert claim would still let a route stand on it.
    #[test]
    fn a_gate_owns_the_cell_above_its_torch() {
        for index in 0..4u8 {
            let facing = geometry::CellFacing::from_index(index).expect("0..4 is horizontal");
            let origin = (20, 1, 20);
            let gate = Gate::nor("y", &["a"]);
            let (footprint, conductors, _) = compile::gate_footprint(origin, &gate, facing);

            let torch = step(
                Anchor { x: origin.0, y: origin.1, z: origin.2 },
                geometry::output_direction(facing),
            );
            let above = Anchor { y: torch.y + 1, ..torch };
            assert!(
                footprint.contains(&above),
                "{facing:?}: nothing claims {above:?}, the cell above the torch at {torch:?}"
            );
            assert!(
                conductors.contains(&above),
                "{facing:?}: {above:?} is claimed but inert, so a route may still stand on it"
            );
        }
    }

    /// And a lever owns the cell above it, for the same reason and by the same
    /// measurement -- see `compile::lever_footprint`, which also records why
    /// this is a claim against *this* simulator's rules rather than Minecraft's.
    ///
    /// Asserted on the three builders together rather than on the helper alone,
    /// because the defect this replaced was not a wrong helper: it was the same
    /// three lines written out three times with the cell missing from all of
    /// them. A test that only opened `lever_footprint` would go on passing the
    /// day somebody writes a fourth.
    #[test]
    fn a_lever_owns_the_cell_above_it() {
        for index in 0..4u8 {
            let facing = geometry::CellFacing::from_index(index).expect("0..4 is horizontal");
            let anchor = Anchor { x: 20, y: 1, z: 20 };
            let (cells, pin) = compile::lever_footprint(anchor, facing);
            let above = Anchor { y: anchor.y + 1, ..anchor };
            assert!(
                cells.contains(&above),
                "{facing:?}: nothing claims {above:?}, the cell above the lever at {anchor:?}"
            );
            assert_eq!(pin, step(anchor, geometry::output_direction(facing)));
        }

        // Every builder, not just the helper. `conductors` and not merely
        // `footprint`: an inert claim passes `owner` and fails
        // `conductor_owner`, which is what `anchor_is_free_for`'s floor test
        // asks, so a route would still stand on it.
        let (netlist, _) = build_and4_netlist();
        let planned = plan_from_netlist(&netlist, &PortPlacements::default())
            .expect("and4 places by relaxation");
        let compiled = crate::compile::compile_legacy(&netlist).expect("and4 compiles the legacy way");
        let legacy = seed_from_legacy(&netlist, &compiled).expect("and4 seeds from legacy");
        let rows = rows_and_barycentres(&netlist);

        for (what, candidate) in [
            ("relaxation", &planned),
            ("legacy seed", &legacy),
            ("rows and barycentres", &rows),
        ] {
            let mut levers = 0;
            for node in candidate.primitive_nodes() {
                if node.realisation != NodeRealisation::Primitive(Primitive::Lever) {
                    continue;
                }
                let above = Anchor { y: node.anchor.y + 1, ..node.anchor };
                assert!(
                    node.occupied().contains(&above),
                    "{what}: {} does not claim {above:?}, the cell above its lever",
                    node.id
                );
                assert_eq!(
                    node.occupancy_of(above),
                    Occupancy::GateConductor,
                    "{what}: {} claims {above:?} but inertly, so a route may still stand on it",
                    node.id
                );
                levers += 1;
            }
            assert_eq!(levers, netlist.inputs.len(), "{what}: not every input is a lever");
        }
    }

    /// Every cell a node calls a conductor conducts in `live_reservation` too.
    ///
    /// It did not. An anchor sweep ran first and wrote `Occupancy::Solid`, and
    /// `Reservation::insert` is `or_insert_with`, so the node's own declaration
    /// arrived at a cell already spoken for and was dropped. Every NOR support
    /// and every lever -- the two things here that conduct *and* have an anchor
    /// -- read as inert to the only caller, `try_move`: 11 of and4's 11 and 25
    /// of full_adder's 25.
    ///
    /// An inert claim is not a harmless one. `owner` still answers, so nothing
    /// gets written on top; `conductor_owner` does not, and that is what
    /// `anchor_is_free_for`'s floor test and `keep_out` ask. So `try_move` was
    /// offered cells the router itself refuses -- among them dust laid directly
    /// beside a lit lever, reading 15, which is a hazard vanilla Minecraft has
    /// too and not an artefact of this simulator's isotropic lever.
    ///
    /// Asserted on the *node's own* declaration rather than on a list of cells,
    /// so this keeps holding when a node starts claiming something new -- which
    /// is exactly what `lever_footprint` just did.
    #[test]
    fn live_reservation_keeps_every_conductor_conducting() {
        let (netlist, _) = build_and4_netlist();
        let candidate = plan_from_netlist(&netlist, &PortPlacements::default())
            .expect("and4 places by relaxation");

        let idle = vec![false; candidate.routes().len()];
        let live = candidate.live_reservation(&idle);

        let mut checked = 0;
        for node in candidate.primitive_nodes() {
            for &cell in node.occupied() {
                if node.occupancy_of(cell) != Occupancy::GateConductor {
                    continue;
                }
                assert!(
                    live.conductor_owner(&cell).is_some(),
                    "{} calls {cell:?} a conductor and live_reservation calls it inert",
                    node.id
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no conductor cells, so nothing was checked");
    }

    /// The two records of a gate's facing -- the candidate's `variant_indices`
    /// and the compiled circuit's `gate_facings` -- have to agree, because the
    /// verifiers read the second to check what the first built.
    #[test]
    fn a_planned_circuit_reports_the_facings_it_was_built_at() {
        let (netlist, _) = build_and4_netlist();
        let candidate = plan_from_netlist(&netlist, &PortPlacements::default())
            .expect("and4 places by relaxation");
        let compiled = crate::compile::compile_planned(&netlist, &PortPlacements::default())
            .expect("and4 compiles through the planner");

        let expected: Vec<_> = (0..netlist.gates.len()).map(|g| candidate.facing_of(g)).collect();
        assert_eq!(compiled.gate_facings, expected);
        assert!(
            expected.iter().any(|&facing| facing != geometry::CellFacing::NORTH),
            "relaxation turns something in and4, or this test proves nothing"
        );
    }

    /// A port has no position until somebody gives it one.
    ///
    /// Fixing where the levers and lamps go before planning starts is what
    /// stops a layout ever being as small as it could be: the planner can
    /// compact everything between the ports and nothing about the ports
    /// themselves. So a placement is an input, it defaults to empty, and what
    /// nobody pinned the planner decides.
    #[test]
    fn an_unpinned_port_is_placed_by_the_planner_and_a_pinned_one_is_not() {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![Gate::nor("y", &["a"])],
        };

        let free = plan_from_netlist(&netlist, &PortPlacements::default())
            .expect("an unpinned circuit must place");
        verify_candidate(&free, &netlist).expect("and be legal");

        let elsewhere = Anchor {
            x: free.port_anchor("a").expect("the lever is a port").x + COLUMN_PITCH,
            ..free.port_anchor("a").expect("the lever is a port")
        };
        let mut placements = PortPlacements::default();
        placements.pin("a", elsewhere);

        let pinned = plan_from_netlist(&netlist, &placements)
            .expect("a pinned circuit must place too");
        assert_eq!(
            pinned.port_anchor("a"),
            Some(elsewhere),
            "a pinned port goes exactly where it was pinned"
        );
        verify_candidate(&pinned, &netlist).expect("and be legal");
    }

    /// What was pinned stays pinned: optimisation may move anything else.
    #[test]
    fn optimisation_never_moves_a_pinned_port() {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let free = plan_from_netlist(&netlist, &PortPlacements::default()).expect("places");
        let where_it_went = free.port_anchor("a").expect("the lever is a port");

        let mut placements = PortPlacements::default();
        placements.pin("a", where_it_went);
        let pinned = plan_from_netlist(&netlist, &placements).expect("places");

        let best = optimise(
            pinned,
            &netlist,
            PlannerWeights::default(),
            PlannerEffort { evaluations: 64, seed: 3 },
        );

        assert_eq!(best.port_anchor("a"), Some(where_it_went));
    }

    /// Six independent gates: six bodies with every reason to sit on one
    /// signal and no room to.
    fn six_independent_gates() -> Netlist {
        Netlist {
            inputs: vec!["a".to_string()],
            outputs: (0..6).map(|index| format!("g{index}")).collect(),
            gates: (0..6)
                .map(|index| Gate::nor(format!("g{index}"), &["a"]))
                .collect(),
        }
    }

    fn extent(candidate: &PlanCandidate) -> (i32, i32, i32) {
        let mut min = (i32::MAX, i32::MAX, i32::MAX);
        let mut max = (i32::MIN, i32::MIN, i32::MIN);
        for anchor in candidate.anchors() {
            min = (min.0.min(anchor.x), min.1.min(anchor.y), min.2.min(anchor.z));
            max = (max.0.max(anchor.x), max.1.max(anchor.y), max.2.max(anchor.z));
        }
        (max.0 - min.0 + 1, max.1 - min.1 + 1, max.2 - min.2 + 1)
    }

    /// A pair a storey apart in Y is already separated, and the projection
    /// moves neither -- while a pair any closer is pushed to exactly that.
    ///
    /// Two cells, not the safety condition's one. That condition has no
    /// pure-vertical case -- every case of `dust_reach` takes a horizontal
    /// cardinal step -- but `dust_reach` is the join mechanism, and power
    /// arriving from the dust above a block is a different one nobody has
    /// derived here. So the vertical requirement is `CONDUCTOR_CLEARANCE`
    /// applied to an axis, which is what `offence` enforces and what the spec's
    /// test 8 asks for.
    ///
    /// **Both halves, because the requirement is read in two places.**
    /// `unseparated` decides whether a pair offends and `offence` decides what
    /// clearing it along Y costs, and a projection is only safe if those are
    /// the same number. Since Task 11 they are the same *constant* --
    /// `relax::VERTICAL_CLEARANCE` -- which makes the hazard structural rather
    /// than a comment, and these two assertions are what would catch a
    /// re-introduced second number: the exempt half reads `unseparated`'s test,
    /// the pushed half reads `offence`'s charge, and a margin added to one and
    /// not the other moves a pair to a gap it still calls a violation.
    ///
    /// It is still far cheaper than the horizontal requirement, which carries
    /// the routing reservation *and* [`relax::SNAP_MARGIN`] on top -- and that
    /// gap is the mechanism `crowding_produces_height` spends, tested here
    /// where it can be seen rather than inferred from a layout. Why the
    /// vertical requirement carries no margin of its own is
    /// `a_vertical_gap_at_the_requirement_survives_rounding`, in `snap.rs`; what
    /// the cheapness costs the router is `relax::VERTICAL_CLEARANCE`.
    #[test]
    fn two_bodies_in_one_column_are_left_where_they_are() {
        use crate::compile::relax::{project, project_for_test, Axes, SETTLED, VERTICAL_CLEARANCE};

        let storey = VERTICAL_CLEARANCE;
        let required = vec![9.0, 9.0];

        let mut clear =
            project_for_test::two_free_bodies([10.0, 1.0, 10.0], [10.0, 1.0 + storey, 10.0]);
        project(&mut clear, &required, Axes::ALL).expect("already separated");
        assert_eq!(clear.bodies[0].position, [10.0, 1.0, 10.0]);
        assert_eq!(clear.bodies[1].position, [10.0, 1.0 + storey, 10.0]);

        // Half a storey apart, so `unseparated` sees it and `offence` has to
        // charge the same number the exempt case above was let through at.
        let mut crowded =
            project_for_test::two_free_bodies([10.0, 1.0, 10.0], [10.0, 1.0 + storey / 2.0, 10.0]);
        project(&mut crowded, &required, Axes::ALL).expect("two bodies always fit");
        let dy = (crowded.bodies[0].position[1] - crowded.bodies[1].position[1]).abs();
        assert!(
            (dy - storey).abs() <= SETTLED,
            "a crowded pair is pushed to the vertical requirement, and this is {dy}"
        );
    }

    /// Height is earned by crowding rather than requested.
    ///
    /// Six gates that all consume one signal have every reason to sit near it
    /// and no room to. Spreading sideways costs the full horizontal
    /// requirement, reservations and all; stacking costs `VERTICAL_CLEARANCE`.
    /// So they should stack.
    ///
    /// **Ignored because the production entry point is flat, and that is the
    /// only reason.** `plan_from_netlist` passes `Axes::IN_PLANE`, so this
    /// netlist never leaves one level and the test fails on its own assertion
    /// with `height` of 1. It does not fail on a climb -- an earlier ignore
    /// string here named one, and this netlist never builds a climb to fail on.
    ///
    /// Switch that one word and this netlist does place, route and verify
    /// legal. What it does *not* do is stack a gate: the single body that
    /// leaves the plane is `input:a`, a lever, which is why the assertion this
    /// test wants is about gates and the harness names the bodies it lifts
    /// rather than counting them. What the flip waits on is full_adder's one
    /// lost route, `(38,1,124)` to `(40,3,124)`. The table, and the derivation
    /// it refuted, are on `relax::VERTICAL_CLEARANCE`.
    ///
    /// This replaces `a_tall_preference_uses_height_where_a_wide_one_uses_floor`
    /// and claims less than it did: not "ask for tall and get tall", but "crowd
    /// it and it stacks". Task 11 deleted `Shape`, `TALL_COLUMN_LIMIT` and
    /// `plan_from_netlist_shaped`, so there is no longer a knob standing in for
    /// the answer -- only the answer, and a measurement of what it costs.
    #[test]
    #[ignore = "known: plan_from_netlist passes Axes::IN_PLANE, so this is flat and fails at height 1; the flip waits on full_adder's (38,1,124)->(40,3,124) route"]
    fn crowding_produces_height() {
        let netlist = six_independent_gates();
        let candidate = plan_from_netlist(&netlist, &PortPlacements::default())
            .expect("six gates on one signal must place");
        verify_candidate(&candidate, &netlist).expect("must be legal");

        let (_, height, _) = extent(&candidate);
        assert!(height > 1, "six crowded gates stayed on one level");
    }

    /// Nothing is placed below the ground plane, however hard separation is
    /// asked to spend height.
    ///
    /// `separate` splits a move half each way, so the moment axis 1 is
    /// available the lower body of every stacked pair walks *down*. There is no
    /// ground under it: `PLANNER_Y` is 1 because a cell stands on a floor one
    /// level below and the world's lowest level is 0, so a gate at `y = 0`
    /// writes its floor outside the world and its socket reads as air however
    /// carefully a route reached it. `claim_column` already carries this guard
    /// for X -- "never left of the first column" -- and Y had no counterpart
    /// until Task 11 gave it one.
    ///
    /// **Driven at `Axes::ALL` rather than through `plan_from_netlist`**, which
    /// passes `Axes::IN_PLANE` and so never writes a Y at all: read through the
    /// shipping entry point this test could not fail against the defect it is
    /// named for. Measured on 2026-08-14 with the floor removed: this netlist
    /// puts a gate at `y = 0` and full_adder puts two there, and
    /// `verify_candidate` refuses both with "realised sink block Air at
    /// (55, -1, 7)".
    #[test]
    fn nothing_is_placed_below_the_ground_plane() {
        use crate::circuits::full_adder::build_full_adder_netlist;

        assert_eq!(
            f64::from(PLANNER_Y),
            relax::GROUND,
            "the planner's storey and the projection's floor are one number"
        );

        for (name, netlist) in [
            ("six_independent_gates", six_independent_gates()),
            ("full_adder", build_full_adder_netlist().0),
        ] {
            let start = starting_layout(&netlist, &PortPlacements::default())
                .unwrap_or_else(|error| panic!("{name} lays out: {error}"));
            let graph = primitive_graph::expand(&netlist, &Library::default_library())
                .unwrap_or_else(|error| panic!("{name} expands: {error}"));
            let placement = relax::relax(
                &netlist,
                &graph,
                &start,
                &PortPlacements::default(),
                relax::Axes::ALL,
                relax::RelaxEffort::default(),
            )
            .unwrap_or_else(|error| panic!("{name} relaxes: {error}"));
            let snapped = relax::snap(&placement)
                .unwrap_or_else(|error| panic!("{name} snaps: {error}"));

            assert!(
                snapped.iter().any(|node| node.anchor.y > PLANNER_Y),
                "{name} never left the ground, so the floor was never asked about"
            );
            for node in &snapped {
                assert!(
                    node.anchor.y >= PLANNER_Y,
                    "{name} put node {} at {:?}, below the ground plane",
                    node.node,
                    node.anchor
                );
            }
        }
    }

    /// full_adder, placed and routed by the planner alone, computes a full
    /// adder.
    ///
    /// The first circuit past and4 to survive without the legacy emitter, and
    /// it took the routing to stop breaking its own geometry: a path lays a
    /// floor under every cell it occupies, so one that doubles back on itself
    /// in Y either drops past a gap its own floor has filled or lands a floor
    /// on the head of a cell it climbed out of. Both were silent -- the
    /// circuit was structurally connected and electrically dead.

    #[test]
    fn a_self_placed_full_adder_computes_a_full_adder() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::redstone::simulator::Simulator;

        // The declared outputs carry generated signal names; the builder's map
        // is what says which is `sum` and which is `cout`.
        let (netlist, ports) = build_full_adder_netlist();
        let candidate =
            plan_from_netlist(&netlist, &PortPlacements::default()).expect("full_adder places");
        let realised = realise_and_verify(&candidate, &netlist, candidate_world_size(&candidate))
            .expect("full_adder is legal");

        let mut simulator = Simulator::new(realised.world.clone());
        simulator.run_until_stable(4000).expect("settles");

        let inputs = ["a", "b", "cin"];
        for mask in 0u8..8 {
            for (bit, name) in inputs.iter().enumerate() {
                let at = realised.ports.input_positions[*name];
                let mut state = simulator.world().get(at.0, at.1, at.2).clone();
                state.lit = (mask >> bit) & 1 == 1;
                simulator.world_mut().set(at.0, at.1, at.2, state);
            }
            simulator.run_until_stable(4000).expect("settles");

            let count = (0..3).filter(|bit| (mask >> bit) & 1 == 1).count();
            let read = |name: &str| {
                let at = realised.ports.output_positions[&ports[name]];
                simulator.world().get(at.0, at.1, at.2).lit
            };
            assert_eq!(read("sum"), count % 2 == 1, "sum for inputs {mask:03b}");
            assert_eq!(read("cout"), count >= 2, "cout for inputs {mask:03b}");
        }
    }

    /// The same router, the same netlist, two placements: does the wall move?
    ///
    /// This is the experiment `2026-08-15-routing-at-scale.md` calls "the
    /// single most load-bearing open question in the document, because it
    /// decides whether the fix belongs in the router or is shared with the
    /// placer" -- and the one neither diagnosis ran.
    ///
    /// Two diagnoses reached opposite conclusions from compatible data. One
    /// ran a single pass with rip-up disabled, found `segment_a` refusing 23
    /// branches from the relaxed *and* the legacy anchors alike, and concluded
    /// the router "has never routed any circuit larger than `full_adder` on
    /// **any** layout -- including the legacy emitter's own". The other ran the
    /// full loop and concluded the relaxed placement is the input that breaks a
    /// router legacy's placement does not.
    ///
    /// **Measured, and the second one is right.** The legacy emitter's own
    /// anchors, with its routes thrown away and every net re-laid by this
    /// router, route `segment_a` in about 110 seconds. The relaxed placement
    /// never does, at 64 rounds or at 256. So the one-pass census was measuring
    /// something real -- both placements are hard on pass one -- and drawing a
    /// conclusion its own method could not support: rip-up *rescues* legacy and
    /// does not rescue the relaxed layout.
    ///
    /// What that settles: the router is not independently broken at this size,
    /// and the relaxation is not innocent. It halved the anchor box (`segment_a`
    /// 29,435 -> 8,099) and the negotiation that sufficed at the old density
    /// does not suffice at the new one. The fix is shared, which is why the
    /// spec asks for a negotiating router rather than a looser placer -- but it
    /// is now a measured reason rather than a preference.
    ///
    /// `segment_a` only, and deliberately: it is the smallest circuit that
    /// separates the two hypotheses, and `seven_segment` at this cost would put
    /// the harness past ten minutes.
    #[test]
    #[ignore = "measurement harness: asserts nothing, routes segment_a twice, takes about two minutes"]
    fn measure_whether_the_legacy_placement_routes_through_this_router() {
        use crate::circuits::seven_segment::build_single_segment_netlist;

        let (netlist, _) = build_single_segment_netlist(0);

        let compiled = compile::compile_legacy(&netlist).expect("segment_a compiles the legacy way");
        let emission = compiled.legacy_emission().expect("legacy metadata");
        let seed = seed_from_legacy_parts(&netlist, emission).expect("segment_a seeds");

        // The legacy anchors with the legacy routes discarded, so what is
        // measured is this router on that placement rather than the routes the
        // emitter laid itself.
        let bare = PlanCandidate::with_primitive_nodes(
            seed.anchors().to_vec(),
            seed.primitive_nodes().to_vec(),
            Vec::new(),
        );
        match route_every_net(bare, &netlist, RIP_UP_ROUNDS) {
            Ok(_) => eprintln!("legacy anchors: segment_a ROUTES through this router"),
            Err(error) => eprintln!("legacy anchors: segment_a FAILS: {error}"),
        }

        match plan_from_netlist(&netlist, &PortPlacements::default()) {
            Ok(_) => eprintln!("relaxed anchors: segment_a ROUTES"),
            Err(error) => eprintln!("relaxed anchors: segment_a FAILS: {error}"),
        }
    }

    /// A route lays dust where the plan committed stone -- three times on
    /// `full_adder`, and each time over stone it committed itself.
    ///
    /// This documents a defect rather than asserting correctness, which is why
    /// it pins the exact cells: if the count moves, either the defect spread or
    /// somebody fixed it, and both deserve to be read rather than absorbed.
    /// `Occupancy::Stone`'s doc carries the mechanism -- `reserve_path` runs
    /// after a path is chosen, so a route cannot see its own floors while it
    /// searches, and `Reservation::insert` is `or_insert`.
    ///
    /// It matters because `stone_owner` is what the exact lid rule consults.
    /// Sound across nets, wrong within one.
    #[test]
    fn a_route_lays_dust_on_its_own_committed_stone() {
        use crate::circuits::full_adder::build_full_adder_netlist;

        let (netlist, _) = build_full_adder_netlist();
        let candidate =
            plan_from_netlist(&netlist, &PortPlacements::default()).expect("full_adder routes");

        let mut reservation = reserve_primitives(candidate.primitive_nodes());
        for route in candidate.routes() {
            reserve_path(&mut reservation, &route.id, &route.anchors);
        }

        let mut clashes = 0usize;
        for route in candidate.routes() {
            for anchor in &route.anchors {
                if let Some(owner) = reservation.stone_owner(anchor) {
                    clashes += 1;
                    eprintln!(
                        "dust of route {} at {anchor:?} sits on committed stone of {owner}",
                        route.id
                    );
                }
            }
        }
        assert_eq!(
            clashes, 3,
            "full_adder lays dust on committed stone at exactly three cells;              this moved, so read the list above rather than re-pinning the number"
        );
    }

    /// Every plan-time promise against the block actually built.
    ///
    /// The reservation is a promise about a world that does not exist yet, and
    /// until this harness nothing compared it to the world that got built. It
    /// finds the three cells `a_route_lays_dust_on_its_own_committed_stone`
    /// pins, and on the planner path it finds nothing else:
    ///
    /// ```text
    /// and4:       232 built | 242 claimed | 188 checkable | 0 mismatched | 5 unclaimed (2%)
    /// full_adder: 1065 built | 1127 claimed | 956 checkable | 3 mismatched | 5 unclaimed (0%)
    /// ```
    ///
    /// **Two of the four `Occupancy` values make no checkable prediction.**
    /// `GateConductor` may be realised as stone (a NOR support), as *air* (the
    /// cell above a torch) or as dust (an output pin); `Solid` is the
    /// catch-all. So 78% of and4's claims and 85% of full_adder's are
    /// checkable, and the rest are unfalsifiable by construction. That is a
    /// property of the type, not a gap in this harness.
    ///
    /// The unclaimed cells are lamps and a few floors -- **the coverage is not
    /// where the risk lives.** The 41 couplings
    /// `docs/derived/realised-graph-extras.md` records all cross through cells
    /// the reservation *does* own; what was missing was a check of the
    /// relation between them, not ownership of them.
    #[test]
    #[ignore = "measurement harness: asserts nothing, prints the reconciliation"]
    fn measure_plan_promises_against_the_realised_world() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::build_single_segment_netlist;
        use crate::redstone::world::block::BlockKind;

        for (name, netlist) in [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
        ] {
            let Ok(candidate) = plan_from_netlist(&netlist, &PortPlacements::default()) else {
                eprintln!("RECON {name}: planner path cannot build it, skipped");
                continue;
            };
            let realised =
                match realise_and_verify(&candidate, &netlist, candidate_world_size(&candidate)) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("RECON {name}: does not realise: {e}");
                        continue;
                    }
                };

            let mut reservation = reserve_primitives(candidate.primitive_nodes());
            for route in candidate.routes() {
                reserve_path(&mut reservation, &route.id, &route.anchors);
            }

            let (sx, sy, sz) = realised.world.size();
            let mut world_cells = 0usize;
            for flat in 0..(sx * sy * sz) {
                let (x, y, z) = realised.world.decode(flat as usize);
                if realised.world.get(x, y, z).kind != BlockKind::Air {
                    world_cells += 1;
                }
            }

            let mut claimed = 0usize;
            let mut checkable = 0usize;
            let mut mismatched = 0usize;
            let mut by_kind: std::collections::BTreeMap<String, usize> = Default::default();
            for (cell, (owner, occ)) in reservation.cells.iter() {
                claimed += 1;
                let got = realised.world.get(cell.x, cell.y, cell.z).kind;
                let ok = match occ {
                    Occupancy::Wire => {
                        checkable += 1;
                        matches!(got, BlockKind::RedstoneWire | BlockKind::Repeater)
                    }
                    Occupancy::Stone => {
                        checkable += 1;
                        got == BlockKind::Solid
                    }
                    // A mandatory-air commitment predicts exactly air; the lid
                    // rule exists so nothing can write over it.
                    Occupancy::Air => {
                        checkable += 1;
                        got == BlockKind::Air
                    }
                    // Predicts nothing checkable: a GateConductor may be stone,
                    // AIR (the cell above a torch), or dust.
                    Occupancy::GateConductor | Occupancy::Solid => true,
                };
                if !ok {
                    mismatched += 1;
                    *by_kind.entry(format!("{occ:?} promised, {got:?} built")).or_default() += 1;
                    if mismatched <= 6 {
                        eprintln!("RECON {name}: {cell:?} owner={owner} {occ:?} -> {got:?}");
                    }
                }
            }
            // The other half, and the one the 41 extra edges lived in: how much
            // of the built world did nobody promise anything about?
            let mut unclaimed: std::collections::BTreeMap<String, usize> = Default::default();
            let mut unclaimed_total = 0usize;
            for flat in 0..(sx * sy * sz) {
                let (x, y, z) = realised.world.decode(flat as usize);
                let kind = realised.world.get(x, y, z).kind;
                if kind == BlockKind::Air {
                    continue;
                }
                if reservation.cells.contains_key(&Anchor { x, y, z }) {
                    continue;
                }
                unclaimed_total += 1;
                *unclaimed.entry(format!("{kind:?}")).or_default() += 1;
            }
            eprintln!(
                "RECON {name}: world {world_cells} solid | claimed {claimed} |                  checkable {checkable} | MISMATCHED {mismatched} | {by_kind:?}"
            );
            eprintln!(
                "RECON {name}: UNCLAIMED {unclaimed_total} of {world_cells} built cells                  ({:.0}%) {unclaimed:?}",
                100.0 * unclaimed_total as f64 / world_cells as f64
            );
        }
    }

    /// What optimisation costs and what it buys, per circuit.
    ///
    /// The one harness here whose ignore is paid for by its runtime: a minute,
    /// against a second for the other two.
    #[test]
    #[ignore = "measurement harness: asserts nothing, and takes about a minute"]
    fn measure_optimisation_at_scale() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };

        for (name, netlist) in [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
        ] {
            let compiled = compile::compile_legacy(&netlist).expect("compiles");
            let seed = seed_from_legacy_parts(&netlist, compiled.legacy_emission().unwrap())
                .expect("seeds");

            let moves = enumerate_moves(&seed, 0x26_02);
            let mut legal = 0usize;
            let mut checked = 0usize;
            for change in moves.iter().take(24) {
                checked += 1;
                if change
                    .apply(&seed)
                    .is_some_and(|proposal| verify_candidate(&proposal, &netlist).is_ok())
                {
                    legal += 1;
                }
            }

            let before = seed.cost();
            let report = optimise_with_report(
                seed,
                &netlist,
                PlannerWeights::default(),
                PlannerEffort { evaluations: 24, seed: 0x26_02 },
            );
            let after = report.candidate.cost();

            eprintln!(
                "{name}: {} gates | {} proposals, {legal}/{checked} legal | \
                 delay {}->{} wire {}->{} space {}->{} turns {}->{}",
                netlist.gates.len(),
                moves.len(),
                before.delay, after.delay,
                before.wire, after.wire,
                before.space, after.space,
                before.turns, after.turns,
            );
        }
    }

    /// Keep-out is about conductors, not about everything a primitive owns.
    ///
    /// The channel-safety spec derives the rule from `dust_reach`: two
    /// *conductor* cells of different nets need clearance. A gate's floor
    /// stone is not one. Treating it as one makes the cell in front of every
    /// gate unroutable, which is why a route could not reach and4's sockets
    /// however far apart the gates were placed.
    #[test]
    fn keep_out_ignores_another_owner_s_plain_floor() {
        let cell = Anchor { x: 5, y: 1, z: 5 };
        let neighbour = Anchor { x: 6, y: 1, z: 5 };

        let mut floor_below = Reservation::new();
        floor_below.insert(Anchor { y: 0, ..neighbour }, "other", Occupancy::Solid);
        assert!(
            anchor_is_free_for(cell, cell, cell, cell, "mine", &floor_below),
            "a solid floor one level down is not a conductor"
        );

        let mut dust_below = Reservation::new();
        dust_below.insert(Anchor { y: 0, ..neighbour }, "other", Occupancy::Wire);
        assert!(
            !anchor_is_free_for(cell, cell, cell, cell, "mine", &dust_below),
            "another net's dust one level down is exactly what keep-out is for"
        );
    }

    /// [`keep_out`] is exactly the twelve offsets `docs/derived/
    /// dust-join-relation.md` scores, and `tests/dust_join_relation.rs`
    /// mirrors that list because this function is private to this module.
    ///
    /// Without this assertion the mirror could drift and the derived table
    /// would silently start scoring a different rule than the one that ships.
    #[test]
    fn keep_out_is_exactly_the_twelve_offsets_the_derived_table_scores() {
        const MIRRORED: [(i32, i32, i32); 12] = [
            (0, -1, -1),
            (0, 0, -1),
            (0, 1, -1),
            (0, -1, 1),
            (0, 0, 1),
            (0, 1, 1),
            (1, -1, 0),
            (1, 0, 0),
            (1, 1, 0),
            (-1, -1, 0),
            (-1, 0, 0),
            (-1, 1, 0),
        ];
        let anchor = Anchor { x: 40, y: 3, z: 40 };
        let measured: BTreeSet<(i32, i32, i32)> = keep_out(anchor)
            .into_iter()
            .map(|cell| (cell.x - anchor.x, cell.y - anchor.y, cell.z - anchor.z))
            .collect();
        let mirrored: BTreeSet<(i32, i32, i32)> = MIRRORED.into_iter().collect();
        assert_eq!(
            measured, mirrored,
            "tests/dust_join_relation.rs scores this exact list; update both or \
             the derived table stops describing the shipping rule"
        );
        assert_eq!(keep_out(anchor).len(), 12, "and with no duplicates");
    }

    /// **The crux, resolved: the reservation can now tell a stone riser from a
    /// cell that has to stay air.**
    ///
    /// This test used to assert the opposite, and was right to. `docs/derived/
    /// dust-join-relation.md` derives that a vertical pair joins or not
    /// according to one cell -- the one directly above the lower conductor --
    /// and specifically according to `supports_dust_step` and `is_conductive`,
    /// both false for air and both true for stone. [`Occupancy`] had two values
    /// and [`reserve_path`] wrote **both** cells of a climb's
    /// [`staircase_clearance`] as `Solid` under the same owner: one is the
    /// stone riser `to` stands on, the other is the cell over the climber's
    /// head, which has to stay air or the climb does not happen. Two opposite
    /// futures, one indelible entry -- so no function of the reservation alone
    /// could decide the join rule's question.
    ///
    /// [`Occupancy::Stone`] is that missing third value, and this asserts the
    /// two cells now differ. **What it does not say is that the exact rule is
    /// therefore shippable** -- it is not, for a reason that has nothing to do
    /// with this one; see [`keep_out_against`] and
    /// `a_stone_lid_seals_a_dust_pair_and_does_not_seal_a_repeater`.
    #[test]
    fn the_reservation_tells_a_stone_riser_from_a_mandatory_air_cell() {
        let from = Anchor { x: 10, y: 1, z: 10 };
        let to = Anchor { x: 11, y: 2, z: 10 };
        let clearance = staircase_clearance(from, to);
        let riser = Anchor { y: from.y, ..to };
        let headroom = Anchor { y: from.y + 1, ..from };
        assert_eq!(
            clearance,
            vec![riser, headroom],
            "a climb claims the block it steps onto and the cell over its own head"
        );

        let mut reservation = Reservation::new();
        reserve_path(&mut reservation, "net", &[from, to]);

        // Both are still claimed under a name nobody routes as, and neither
        // conducts -- that part is unchanged.
        assert_eq!(reservation.owner(&riser), Some("stair:net"));
        assert_eq!(reservation.owner(&headroom), Some("stair:net"));
        assert_eq!(reservation.conductor_owner(&riser), None);
        assert_eq!(reservation.conductor_owner(&headroom), None);

        // What changed: the riser is committed stone and the headroom is not.
        assert_eq!(
            reservation.cells.get(&riser).map(|(_, o)| *o),
            Some(Occupancy::Stone),
            "the riser is what `to` stands on, so realisation writes stone there"
        );
        assert_eq!(
            reservation.cells.get(&headroom).map(|(_, o)| *o),
            Some(Occupancy::Air),
            "the cell over the climber's head has to stay air, and `Air` is \
             that commitment by name -- the lid rule reads it"
        );
        assert_ne!(
            reservation.cells.get(&riser).map(|(_, o)| *o),
            reservation.cells.get(&headroom).map(|(_, o)| *o),
            "the two opposite futures are no longer one indelible entry"
        );
        // And the headroom is exactly case-1's lid cell, so this is the cell
        // the join rule most needs classified.
        assert_eq!(headroom, join_lid(from, to).expect("a climb has a lid"));
        assert_eq!(
            reservation.stone_owner(&headroom),
            None,
            "so the exact rule reads `not sealed` there, which is the truth"
        );
    }

    /// The half of the old crux that survives: the lid cell is frequently not
    /// in the reservation *at all* when [`anchor_is_free_for`] is asked, and
    /// whether it ever gets filled depends on a net that has not been routed
    /// yet.
    ///
    /// [`reserve_path`] claims a floor under every cell of every route, so a
    /// cell that is open when net A is routed can be committed stone once net B
    /// is. The answer an exact rule gives therefore depends on routing order.
    ///
    /// **This one is survivable, and the direction is why.** Open reads as "not
    /// sealed", which refuses; a lid that becomes stone later only means a
    /// route was refused that need not have been. That is incompleteness of the
    /// search, not a short. It is the *other* order-dependence -- the one in
    /// `a_stone_lid_seals_a_dust_pair_and_does_not_seal_a_repeater` -- that
    /// runs the unsafe way.
    #[test]
    fn the_lid_cell_can_be_open_when_asked_and_stone_one_net_later() {
        // `mine` wants to climb from (10,1,10) to (11,2,10). Its lid cell --
        // the cell above the lower conductor -- is (10,2,10).
        let lower = Anchor { x: 10, y: 1, z: 10 };
        let lid = Anchor { x: 10, y: 2, z: 10 };

        let mut reservation = Reservation::new();
        reserve_path(&mut reservation, "mine", &[lower, Anchor { x: 9, ..lower }]);
        assert_eq!(
            reservation.owner(&lid),
            None,
            "nothing in the plan mentions the lid cell yet"
        );
        assert_eq!(
            reservation.stone_owner(&lid),
            None,
            "so the exact rule reads `not sealed`, and refuses"
        );

        // Some later net runs one storey up, straight over it. Its floor lands
        // on the lid cell, and stone there kills both the climb and the descend.
        let over = Anchor { x: 10, y: 3, z: 10 };
        reserve_path(&mut reservation, "later", &[over, Anchor { x: 11, ..over }]);
        assert_eq!(
            reservation.owner(&lid),
            Some("later"),
            "the same cell is a floor once a second net is routed"
        );
        assert_eq!(
            reservation.stone_owner(&lid),
            Some("later"),
            "and the reservation can now say it is stone -- which the two-value \
             `Occupancy` could not, so the same query answered `None` before \
             and after"
        );
        assert_eq!(
            reservation.conductor_owner(&lid),
            None,
            "a floor still does not conduct, so `keep_out`'s own query is \
             unaffected either way"
        );
    }

    /// The world a reservation implies, written out so a plan-time rule can be
    /// judged by the simulator rather than by another plan-time rule.
    ///
    /// Nothing here guesses: [`Occupancy::Wire`] is what `emit_routes` writes
    /// as dust, [`Occupancy::Stone`] is what it writes as `compile::stone()`,
    /// and every wire cell gets the stone floor `realise_branch_from` gives it.
    /// `Solid` and unclaimed cells are air, which is the whole point -- a cell
    /// nobody claims is air in the emitted world.
    fn world_the_reservation_implies(
        reservation: &Reservation,
        size: (i32, i32, i32),
    ) -> crate::redstone::world::storage::World {
        let mut world = crate::redstone::world::storage::World::new(size.0, size.1, size.2);
        for (cell, (_, occupancy)) in &reservation.cells {
            if matches!(occupancy, Occupancy::Stone) {
                world.set(cell.x, cell.y, cell.z, compile::stone());
            }
        }
        for (cell, (_, occupancy)) in &reservation.cells {
            if matches!(occupancy, Occupancy::Wire) {
                world.set(cell.x, cell.y - 1, cell.z, compile::stone());
                world.set(cell.x, cell.y, cell.z, compile::dust());
            }
        }
        world
    }

    /// Are two dust cells joined, by the same walk `verify_connectivity` makes?
    fn the_simulator_joins(
        world: &crate::redstone::world::storage::World,
        a: Anchor,
        b: Anchor,
    ) -> bool {
        use crate::redstone::simulator::connectivity::dust_connections;
        use crate::redstone::simulator::position::HORIZONTAL;
        let reaches = |from: Anchor, to: Anchor| {
            HORIZONTAL.into_iter().any(|direction| {
                dust_connections(world, Position::new(from.x, from.y, from.z), direction)
                    .iter()
                    .any(|cell| (cell.x, cell.y, cell.z) == (to.x, to.y, to.z))
            })
        };
        // Either direction merges two nets: `verify_connectivity` walks
        // `dust_connections` forward from every dust cell, so a one-way edge is
        // still a short.
        reaches(a, b) || reaches(b, a)
    }

    /// **The exact rule, judged by the simulator, on all twelve offsets and
    /// both states of the lid.**
    ///
    /// For each cell [`keep_out`] refuses, the reservation is built the way
    /// [`reserve_path`] builds it, the world it implies is written out, and
    /// [`keep_out_against`]'s verdict is compared against what
    /// `dust_connections` -- the walk `verify_connectivity` itself makes --
    /// says about the pair. Refused must mean joined and admitted must mean
    /// apart, on every row.
    ///
    /// This is what "exact" is allowed to mean here, and it is a real
    /// constraint in both directions: keeping a cell the simulator leaves apart
    /// is the over-claim the routing spec suspected, and dropping one the
    /// simulator joins is a cross-net short.
    ///
    /// **Three lid states, and the middle one is the whole reason
    /// [`Occupancy::Stone`] exists.** Unclaimed is air. `Stone` is a
    /// commitment. `Solid` is "claimed by something that is not stone" -- a
    /// gate's torch, a terminal guard, and above all the cell over a climber's
    /// head, which has to stay *air*. The two-value `Occupancy` could not tell
    /// the last two apart, and a rule that admitted a pair on a `Solid` lid
    /// would be admitting one the simulator joins.
    #[test]
    fn the_exact_rule_matches_the_simulator_on_every_keep_out_offset() {
        #[derive(Clone, Copy)]
        enum Lid {
            Unclaimed,
            ClaimedNotStone,
            Stone,
        }
        let anchor = Anchor { x: 10, y: 2, z: 10 };
        let mut rows = 0usize;
        let mut admitted = 0usize;
        for neighbour in keep_out(anchor) {
            for lid in [Lid::Unclaimed, Lid::ClaimedNotStone, Lid::Stone] {
                let held = join_lid(anchor, neighbour);
                if held.is_none() && !matches!(lid, Lid::Unclaimed) {
                    // A same-layer pair has no lid; its row runs once.
                    continue;
                }
                let mut reservation = Reservation::new();
                // The two conductors, each with the floor realisation gives it.
                reserve_path(&mut reservation, "mine", &[anchor]);
                reserve_path(&mut reservation, "other", &[neighbour]);
                if let Some(cell) = held {
                    match lid {
                        Lid::Unclaimed => {}
                        Lid::ClaimedNotStone => {
                            reservation.insert(cell, "third", Occupancy::Solid)
                        }
                        Lid::Stone => reservation.insert(cell, "third", Occupancy::Stone),
                    }
                }

                let refused = keep_out_against(anchor, &reservation).contains(&neighbour);
                let world = world_the_reservation_implies(&reservation, (24, 8, 24));
                let joined = the_simulator_joins(&world, anchor, neighbour);
                assert_eq!(
                    refused,
                    joined,
                    "offset ({}, {}, {}) with the lid {}: the rule says {}, the \
                     simulator says {}",
                    neighbour.x - anchor.x,
                    neighbour.y - anchor.y,
                    neighbour.z - anchor.z,
                    match lid {
                        Lid::Unclaimed => "unclaimed",
                        Lid::ClaimedNotStone => "claimed, not stone",
                        Lid::Stone => "stone",
                    },
                    if refused { "refuse" } else { "admit" },
                    if joined { "joined" } else { "apart" },
                );
                rows += 1;
                if !refused {
                    admitted += 1;
                }
            }
        }
        assert_eq!(
            rows, 28,
            "four same-layer offsets once, and eight vertical ones in each of \
             the three lid states"
        );
        assert_eq!(
            admitted, 8,
            "exactly the eight vertical cells are admitted, and only on their \
             stone-lid row -- without this count the rule could stop doing \
             anything at all and every row assertion would still pass"
        );
    }

    /// **Why [`keep_out_against`] is not wired into the router: the lid seals
    /// dust and does nothing whatever for a repeater.**
    ///
    /// The derived relation is `dust_connections`, and `dust_connections` is
    /// dust against dust. A repeater is wire too -- `realise_branch_from` puts
    /// one wherever the strength budget asks -- and it reaches the cell one
    /// step across and one level up by a route the join relation never
    /// describes: it *strongly powers the block in front of it*, and that block
    /// is the floor the other cell stands on. A stone lid over the repeater is
    /// irrelevant to that path.
    ///
    /// Measured here electrically, differenced against a control with the
    /// driver removed, in the exact shape the planner realises. Both halves are
    /// asserted, so this cannot pass by being blind:
    ///
    /// * with **dust** in the lower cell the stone lid really does seal it --
    ///   the derivation is not wrong; and
    /// * with a **repeater** in the same cell, aimed at the upper cell's floor,
    ///   the upper cell reads full strength through the lid.
    ///
    /// The last assertion is the one that ties it to the shipping code:
    /// [`anchor_is_free_for`] refuses that cell. Wire [`keep_out_against`] into
    /// it and this test goes red on a configuration whose truth table is wrong.
    #[test]
    fn a_stone_lid_seals_a_dust_pair_and_does_not_seal_a_repeater() {
        use crate::redstone::simulator::Simulator;
        use crate::redstone::world::block::{BlockKind, BlockState};
        use crate::redstone::world::storage::World;

        // Q, the lower conductor, at (10,1,10); P, dust, at (11,2,10) on its
        // own stone floor (11,1,10). The lid over Q is (10,2,10).
        let q = Anchor { x: 10, y: 1, z: 10 };
        let p = Anchor { x: 11, y: 2, z: 10 };
        let lid = join_lid(p, q).expect("a vertical pair has a lid");
        assert_eq!(lid, Anchor { x: 10, y: 2, z: 10 });

        let read_p = |lid_is_stone: bool, q_is_repeater: bool, driven: bool| -> u8 {
            let mut world = World::new(24, 8, 24);
            world.set(q.x, q.y - 1, q.z, compile::stone());
            world.set(p.x, p.y - 1, p.z, compile::stone());
            world.set(p.x, p.y, p.z, compile::dust());
            if lid_is_stone {
                world.set(lid.x, lid.y, lid.z, compile::stone());
            }
            if q_is_repeater {
                // Pointing straight at P's floor block, which is the one thing
                // the lid cannot do anything about.
                world.set(
                    q.x,
                    q.y,
                    q.z,
                    compile::repeater(compile::direction_from(
                        Position::new(q.x - 1, q.y, q.z),
                        Position::new(q.x, q.y, q.z),
                    )),
                );
            } else {
                world.set(q.x, q.y, q.z, compile::dust());
            }
            if driven {
                // A three-cell feed run heading away from P, ending on a
                // redstone block -- the one source with no block power of its
                // own, so nothing leaks through the shared floor.
                for step in 1..4 {
                    world.set(q.x - step, q.y - 1, q.z, compile::stone());
                    world.set(q.x - step, q.y, q.z, compile::dust());
                }
                let mut source = BlockState::air();
                source.kind = BlockKind::RedstoneBlock;
                source.name = "minecraft:redstone_block".to_string();
                world.set(q.x - 4, q.y, q.z, source);
            }
            let mut simulator = Simulator::new(world);
            simulator.run_until_stable(400).expect("settles");
            simulator.world().get(p.x, p.y, p.z).power
        };

        // Every reading is differenced against the same world with the driver
        // gone, so a stray light is a contaminated rig and not a join.
        for q_is_repeater in [false, true] {
            for lid_is_stone in [false, true] {
                assert_eq!(
                    read_p(lid_is_stone, q_is_repeater, false),
                    0,
                    "control: nothing drives P when the source is removed \
                     (repeater {q_is_repeater}, stone lid {lid_is_stone})"
                );
            }
        }

        assert_eq!(
            read_p(false, false, true),
            11,
            "positive control: dust at Q, no lid -- the climb is open and P lights"
        );
        assert_eq!(
            read_p(true, false, true),
            0,
            "the derivation is right about DUST: a stone lid seals the pair"
        );
        assert_eq!(
            read_p(true, true, true),
            15,
            "and it is silent about the same cell holding a REPEATER: that \
             powers P's floor block, and a powered floor drives the dust \
             standing on it -- a path with no lid in it at all"
        );

        // The shipping rule refuses it, which is the only reason this
        // configuration is not reachable.
        let mut reservation = Reservation::new();
        reserve_path(&mut reservation, "other", &[q]);
        reservation.insert(lid, "third", Occupancy::Stone);
        assert!(
            !keep_out_against(p, &reservation).contains(&q),
            "the exact rule would admit it -- that is the defect this names"
        );
        assert!(
            !anchor_is_free_for(p, p, p, p, "mine", &reservation),
            "and `anchor_is_free_for` refuses it, because it asks `keep_out` \
             for all twelve; wire `keep_out_against` in and this goes red"
        );
    }

    /// A cell the plan has committed to stone may not later hold wire -- not
    /// even the wire of the net that committed it.
    ///
    /// `emit_routes` writes floor-then-block per anchor, in route order, so an
    /// anchor that lands on an earlier anchor's floor simply overwrites it: the
    /// cell one storey up is left standing on dust. Nothing caught that before,
    /// because `Reservation::insert` is `or_insert` -- the second claim on the
    /// cell is silently dropped, so the reservation goes on reporting a floor
    /// while the world would get wire.
    #[test]
    fn wire_may_not_be_laid_where_the_plan_committed_stone() {
        let upper = Anchor { x: 10, y: 2, z: 10 };
        let floor = Anchor { x: 10, y: 1, z: 10 };

        let mut reservation = Reservation::new();
        reserve_path(&mut reservation, "mine", &[upper]);
        assert_eq!(
            reservation.stone_owner(&floor),
            Some("mine"),
            "the floor under a routed cell is committed stone"
        );

        for owner in ["mine", "stranger"] {
            assert!(
                !anchor_is_free_for(floor, floor, floor, floor, owner, &reservation),
                "`{owner}` must not be offered a cell committed to stone: the \
                 cell above stands on it, and the join rule reads it"
            );
        }

        // A cell one step over at the same level is still free, so the refusal
        // is about the commitment and not about the whole storey.
        let beside = Anchor { x: 12, y: 1, z: 10 };
        assert!(
            anchor_is_free_for(beside, beside, beside, beside, "stranger", &reservation),
            "nothing else moved"
        );
    }

    /// The planner has to be able to place a circuit itself, not only improve
    /// one the legacy router already placed.
    ///
    /// Every candidate before this came from `seed_from_legacy`, which bounds
    /// the planner to what the row/channel/track emitter could lay down first.
    /// These four shapes are the ones that broke the legacy round-trip when it
    /// was written: a lone NOR, a two-level cone, a fanout, and a bare merge.
    #[test]
    fn a_netlist_places_and_routes_without_the_legacy_emitter() {
        let circuits: [(&str, Netlist); 4] = [
            (
                "lone nor",
                Netlist {
                    inputs: vec!["a".to_string()],
                    outputs: vec!["y".to_string()],
                    gates: vec![Gate::nor("y", &["a"])],
                },
            ),
            (
                "two level",
                Netlist {
                    inputs: vec!["a".to_string(), "b".to_string()],
                    outputs: vec!["y".to_string()],
                    gates: vec![
                        Gate::nor("na", &["a"]),
                        Gate::nor("nb", &["b"]),
                        Gate::nor("y", &["na", "nb"]),
                    ],
                },
            ),
            (
                "fanout",
                Netlist {
                    inputs: vec!["a".to_string()],
                    outputs: vec!["left".to_string(), "right".to_string()],
                    gates: vec![Gate::nor("left", &["a"]), Gate::nor("right", &["a"])],
                },
            ),
            (
                "bare merge",
                Netlist {
                    inputs: vec!["a".to_string(), "b".to_string()],
                    outputs: vec!["y".to_string()],
                    gates: vec![Gate::merge("y", &["a", "b"])],
                },
            ),
        ];

        for (name, netlist) in circuits {
            let candidate = plan_from_netlist(&netlist, &PortPlacements::default())
                .unwrap_or_else(|error| panic!("{name} must be placeable: {error}"));
            verify_candidate(&candidate, &netlist)
                .unwrap_or_else(|error| panic!("{name} must be legal: {error}"));
        }
    }

    /// Legal is not enough: a circuit the planner placed itself has to compute
    /// what it was asked to.
    ///
    /// and4 is the first real circuit this places end to end without the
    /// legacy emitter. Measured on 2026-08-14, after placement became a
    /// relaxation: **232 blocks and 14 game ticks, against rows and
    /// barycentres' 572 and 26.**
    ///
    /// Both halves of that took correcting, and the second one twice.
    ///
    /// An earlier reading had the settle going the *wrong* way, 24 to 28, and
    /// it was an artefact of how it was measured. The old figure came from a
    /// 32-mask sweep driven on one long-lived simulator, and a settle measured
    /// that way is not a property of the circuit: the same 240 ordered
    /// transitions give 38 on a shared simulator and 14 on a fresh one, and the
    /// pair the shared run called 38 (`1001` to `0101`) takes 4 when timed from
    /// a state that has actually settled -- confirmed by a second
    /// `run_until_stable` at `from` returning 0.
    ///
    /// So the loop below times every ordered transition from a settled state,
    /// which is 240 of them rather than 32, and reports the real worst. On that
    /// measure relaxation nearly halves the circuit's critical transition
    /// rather than lengthening it, and both layouts find their worst at the
    /// same one, `0100` to `1011`. The cost model's own delay term moved 22 to
    /// 10 over the same change, which now agrees in direction with what the
    /// simulator says instead of contradicting it.
    ///
    /// It was 656 blocks when first measured, then 572. Nothing set out to
    /// shrink it -- the routing fixes that followed, floors a route owns and
    /// staircases it cannot break, each removed cells that were being wasted on
    /// repair.
    ///
    /// This is still not `timing::summarize_worst_case`, which is how
    /// `reference_circuits` measures the emitter's own layouts and which
    /// reports glitches as well as ticks. What it is, is a number two layouts
    /// can be compared on.
    ///
    /// It took the routing to become three-dimensional. Everything before this
    /// searched one plane, where the nets simply block each other; a staircase
    /// lets one cross over another, which is what channels and tracks exist to
    /// arrange and what this planner now does without them.
    #[test]
    fn a_self_placed_and4_computes_and4() {
        use crate::redstone::simulator::Simulator;
        use crate::redstone::world::block::BlockKind;

        let (netlist, _) = build_and4_netlist();
        let candidate = plan_from_netlist(&netlist, &PortPlacements::default()).expect("and4 must be placeable");
        let realised = realise_and_verify(&candidate, &netlist, candidate_world_size(&candidate))
            .expect("and4 must be legal");

        let blocks = (0..realised.world.cells().len())
            .filter(|&flat| {
                let (x, y, z) = realised.world.decode(flat);
                realised.world.get(x, y, z).kind != BlockKind::Air
            })
            .count();

        let set_inputs = |simulator: &mut Simulator, mask: u8| {
            for (bit, name) in ["a", "b", "c", "d"].iter().enumerate() {
                let at = realised.ports.input_positions[*name];
                let mut state = simulator.world().get(at.0, at.1, at.2).clone();
                state.lit = (mask >> bit) & 1 == 1;
                simulator.world_mut().set(at.0, at.1, at.2, state);
            }
        };

        // Every ordered transition, each timed from a state that has settled,
        // on a simulator that has seen nothing else. All three of those matter:
        // a sweep visits 32 of the 240, chaining them on one simulator reports
        // a number the circuit does not have, and timing from an unsettled
        // state charges the previous transition to this one.
        let out = realised.ports.output_positions[&netlist.outputs[0]];
        let mut worst = 0u64;
        let mut worst_at = (0u8, 0u8);
        for from in 0u8..16 {
            for to in 0u8..16 {
                if from == to {
                    continue;
                }
                let mut simulator = Simulator::new(realised.world.clone());
                set_inputs(&mut simulator, from);
                simulator.run_until_stable(2000).expect("settles at from");
                set_inputs(&mut simulator, to);
                let ticks = simulator.run_until_stable(2000).expect("settles at to");
                if ticks > worst {
                    worst = ticks;
                    worst_at = (from, to);
                }
                assert_eq!(
                    simulator.world().get(out.0, out.1, out.2).lit,
                    to == 0b1111,
                    "self-placed and4 is wrong for inputs {to:04b}, reached from {from:04b}"
                );
            }
        }

        // Rows and barycentres measure 26 here, on this same loop, at this same
        // transition. Bounded rather than pinned: the point is that relaxation
        // did not make the circuit slower, which is what an earlier reading of
        // a different measure appeared to say.
        //
        // The 26 was first measured in a scratch worktree at `3c63495`, which
        // is a method nobody can re-run. `measure_and4_both_ways` runs this
        // same loop over both placements in the tree as it stands, and prints
        // 572 blocks / 26 against 232 / 14.
        assert!(
            worst < 26,
            "relaxation settles in {worst}, no better than rows and barycentres"
        );

        eprintln!(
            "self-placed and4: {blocks} blocks, worst settle {worst} game ticks \
             over 240 transitions, at {:04b} -> {:04b}",
            worst_at.0, worst_at.1
        );
    }

    /// The point of the whole optimiser: a candidate it produced must be
    /// buildable and legal, not merely scoreable.
    ///
    /// `try_move` used to record anchors and nothing else, so a moved route
    /// had no blocks and no floors: `optimise` had never produced anything
    /// anyone could emit, let alone verify or paste. Turning that on is what
    /// this test is for, and getting here took eight real router defects,
    /// each of which the invariants caught and
    /// `validate_candidate_reservation` -- no spacing, no strength, no
    /// torch-merge, `isolation_proven` hardcoded true -- never would have:
    ///
    /// 1. a rerouted branch was never realised at all;
    /// 2. its terminal strength was invented (`16 - path length`) because
    ///    there was no realisation to read it from;
    /// 3. a fanout's shared trunk was appended once per branch, so one cell
    ///    got two blocks;
    /// 4. a terminal's recorded sink cell stayed where the old branch ended;
    /// 5. the A* keep-out saw only the four horizontal neighbours, while dust
    ///    climbs and descends a step;
    /// 6. it reserved one cell per primitive, when a NOR cell is a support, a
    ///    torch, its sockets and its pin;
    /// 7. a repeater needs a horizontal facing, so a cell reached by a step in
    ///    Y can only be dust -- this used to panic;
    /// 8. a net was routed from its producer's support block instead of its
    ///    output pin, laying dust on the support and unsupporting the torch.
    #[test]
    fn a_moved_candidate_can_be_built_and_verified() {
        let (seed, netlist) = legacy_and4_seed_with_netlist();

        let (primitive, to) = movable_target(&seed);
        let moved = try_move(&seed, primitive, to).expect("a legal local move must exist");

        verify_candidate(&moved, &netlist)
            .expect("a moved candidate must realise into a legal world");
    }

    /// The first node this seed can legally shift, and where to.
    fn movable_target(seed: &PlanCandidate) -> (NodeId, Anchor) {
        for primitive in 0..seed.anchors().len() {
            for delta in [2, -2, 4, -4] {
                let anchor = Anchor {
                    z: seed.anchors()[primitive].z + delta,
                    ..seed.anchors()[primitive]
                };
                if try_move(seed, primitive, anchor).is_ok() {
                    return (primitive, anchor);
                }
            }
        }
        panic!("and4 must admit at least one legal local move");
    }

    /// and4 comes out of the hybrid `compile` placed by relaxation, and
    /// `planner_kind` says so.
    ///
    /// This asserted `Unified3d` before the hybrid as well -- but back then
    /// `compile` stamped `Unified3d` on every circuit it ever returned, so the
    /// assertion was a constant dressed as an observation. Now the enum names
    /// the **placer**, and this can be false: if relaxation stops placing and4,
    /// or the router stops routing it inside [`TRIAL_RIP_UP_ROUNDS`], or
    /// `realise_and_verify` starts refusing the result, `compile` falls back to
    /// the emitter without saying a word and this goes red.
    ///
    /// The other half -- that a fallback still ships a world the planner
    /// realised and verified -- is
    /// `compile::tests::the_fallback_ships_the_planners_realisation_too`.
    #[test]
    fn compile_places_and4_by_relaxation() {
        let (netlist, _) = build_and4_netlist();
        let compiled = compile::compile(&netlist).expect("and4 compiles");

        assert_eq!(compiled.planner_kind(), compile::PlannerKind::Unified3d);
    }

    #[test]
    fn verify_candidate_rejects_a_corrupted_primitive_anchor() {
        let (mut candidate, netlist) = legacy_and4_seed_with_netlist();
        candidate.anchors[0].x += 1;
        candidate.primitive_nodes[0].anchor.x += 1;

        rejection(&candidate, &netlist);
    }

    #[test]
    fn verify_candidate_rejects_disagreeing_anchor_stores() {
        let (mut candidate, netlist) = legacy_and4_seed_with_netlist();
        candidate.anchors[0].x += 1;

        assert!(matches!(
            verify_candidate(&candidate, &netlist),
            Err(PlannerError::UnrealisableNode { .. })
        ));
    }

    #[test]
    fn verify_candidate_rejects_a_corrupted_route_cell() {
        let (mut candidate, netlist) = legacy_and4_seed_with_netlist();
        candidate.routes[0].anchors[0] = Anchor { x: 25, y: 1, z: 49 };

        rejection(&candidate, &netlist);
    }

    #[test]
    fn verify_candidate_rejects_a_corrupted_directed_dust_terminal_choice() {
        let (mut candidate, netlist) = legacy_and4_seed_with_netlist();
        let route = candidate
            .routes
            .iter_mut()
            .find(|route| {
                route
                    .terminals
                    .iter()
                    .any(|terminal| terminal.kind == RouteTerminalKind::DirectedDustIntoSupport)
            })
            .expect("and4 includes a directed-dust terminal");
        route.terminals[0].kind = RouteTerminalKind::RepeaterIntoSupport;

        rejection(&candidate, &netlist);
    }

    #[test]
    fn verify_candidate_rejects_a_bare_merge_terminal_that_claims_a_nor_support() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![Gate::merge("y", &["a", "b"])],
        };
        let compiled = compile::compile_legacy(&netlist).expect("merge fixture must compile");
        let mut candidate = seed_from_legacy(&netlist, &compiled).expect("merge fixture must seed");
        candidate.routes[0].terminals[0].kind = RouteTerminalKind::RepeaterIntoSupport;

        rejection(&candidate, &netlist);
    }

    #[test]
    fn verify_candidate_rejects_a_corrupted_route_owner() {
        let (mut candidate, netlist) = legacy_and4_seed_with_netlist();
        candidate.routes[0].owner = Some("not-a".to_string());

        assert!(matches!(
            verify_candidate(&candidate, &netlist),
            Err(PlannerError::UnrealisableNode { .. })
        ));
    }

    #[test]
    fn verify_candidate_rejects_a_terminal_attached_to_the_wrong_sink_identity() {
        let (mut candidate, netlist) = legacy_and4_seed_with_netlist();
        candidate.routes[0].terminals[0].sink.input_index = 99;

        rejection(&candidate, &netlist);
    }

    #[test]
    fn verify_candidate_rejects_a_fanout_with_a_missing_sink_terminal() {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["left".to_string(), "right".to_string()],
            gates: vec![Gate::nor("left", &["a"]), Gate::nor("right", &["a"])],
        };
        let mut candidate = legacy_fanout_seed();
        let route = candidate
            .routes
            .iter_mut()
            .find(|route| route.id == "a")
            .expect("fanout source route must be present");
        assert_eq!(route.terminals.len(), 2, "fixture must expose both sinks");
        route.terminals.pop();

        rejection(&candidate, &netlist);
    }

    // ---------------------------------------------------------------------
    // §5.7 of `docs/superpowers/specs/2026-08-15-routing-at-scale.md`:
    // congestion-driven placement, as a probe.
    // ---------------------------------------------------------------------

    /// What one instrumented run of the shipping rip-up loop found.
    ///
    /// [`route_every_net`] keeps none of this. It returns the **last** round's
    /// error and drops the congestion map on the floor, which is why §1.1 of the
    /// spec had to establish the recurring-failure tally by hand. Everything
    /// here is a counter around that same loop.
    struct Harvest {
        /// The routed candidate, if a round ever finished.
        routed: Option<PlanCandidate>,
        /// How many rip-up rounds the loop actually spent.
        rounds: usize,
        /// The most nets any one round laid before it was blocked, out of
        /// [`Harvest::nets`]. The same quantity the spec cites as "deepest round
        /// laid 36 of 47".
        deepest_laid: usize,
        nets: usize,
        /// The congestion price map as the loop left it: which cells were
        /// charged, and how many times. This is the router's own record of
        /// where nets fought, and it is the whole input to the inflation below.
        charged: BTreeMap<Anchor, u64>,
        /// Which `(net, corridor)` was blocked how often -- §5.4's fix, as a
        /// measurement rather than a code change.
        tally: BTreeMap<(String, (Anchor, Anchor)), usize>,
        /// Who was standing where the search died, cell by cell, summed over
        /// every failed round. See [`refusal_heat`] -- this is the sharper of
        /// the two signals and the one §5.7 asks for by name.
        refused: BTreeMap<Anchor, u64>,
        /// What the shipping error would have printed.
        last: Option<PlannerError>,
    }

    impl Harvest {
        /// Distinct cells ever charged, and the total charge over them.
        fn contention(&self) -> (usize, u64) {
            (self.charged.len(), self.charged.values().sum())
        }

        /// The cell map a given heat source reads.
        fn map_for(&self, source: HeatSource) -> &BTreeMap<Anchor, u64> {
            match source {
                HeatSource::Charge => &self.charged,
                HeatSource::Refusal => &self.refused,
            }
        }

        /// The `(net, corridor)` that recurred most, which §1.1 measured is not
        /// the one [`route_every_net`] reports.
        fn recurring(&self) -> Option<(&str, (Anchor, Anchor), usize)> {
            self.tally
                .iter()
                .max_by_key(|(_, count)| **count)
                .map(|((net, corridor), count)| (net.as_str(), *corridor, *count))
        }
    }

    fn address(corridor: (Anchor, Anchor)) -> String {
        format!(
            "({}, {}, {})->({}, {}, {})",
            corridor.0.x, corridor.0.y, corridor.0.z, corridor.1.x, corridor.1.y, corridor.1.z
        )
    }

    /// [`route_every_net`], with the counters kept.
    ///
    /// **This is that function line for line**, including the `!charged &&
    /// !reordered && !charged_air` stop and the promotion order, because a probe
    /// that measures a different router measures nothing. The two have to be
    /// edited together; the check is that `rounds = RIP_UP_ROUNDS` and no
    /// inflation reproduces `plan_from_netlist`'s own answer, which iteration 1
    /// of every table below is.
    fn harvest_routing(candidate: PlanCandidate, netlist: &Netlist, rounds: usize) -> Harvest {
        let mut order: Vec<String> = net_sinks(netlist).into_keys().collect();
        let nets = order.len();
        let mut congestion = Congestion::default();
        let mut harvest = Harvest {
            routed: None,
            rounds: 0,
            deepest_laid: 0,
            nets,
            charged: BTreeMap::new(),
            tally: BTreeMap::new(),
            refused: BTreeMap::new(),
            last: None,
        };

        for round in 0..rounds {
            harvest.rounds = round + 1;
            match route_in_order(candidate.clone(), netlist, &order, &congestion) {
                Ok(routed) => {
                    harvest.deepest_laid = nets;
                    harvest.routed = Some(routed);
                    break;
                }
                Err(failure) => {
                    let RoutingFailure {
                        blocked,
                        corridor,
                        reservation,
                        charge_outright,
                        error,
                    } = *failure;
                    // Where in the order the blocked net sits *is* how many nets
                    // this round laid before it stopped: `route_in_order` walks
                    // `order` and returns on the first refusal.
                    let laid = order.iter().position(|name| *name == blocked).unwrap_or(0);
                    harvest.deepest_laid = harvest.deepest_laid.max(laid);
                    *harvest.tally.entry((blocked.clone(), corridor)).or_insert(0) += 1;
                    harvest.last = Some(error);

                    // The reservation is the plane as it stood when the search
                    // gave up, so the frontier can be replayed against it.
                    for (cell, blame) in
                        refusal_heat(corridor.0, corridor.1, &blocked, &reservation)
                    {
                        *harvest.refused.entry(cell).or_insert(0) += blame;
                    }

                    let charged_air = congestion.charge_cells(&charge_outright);
                    let charged =
                        congestion.charge(&reservation, corridor.0, corridor.1, &blocked);
                    let mut promoted: Vec<String> = vec![blocked.clone()];
                    promoted.extend(order.iter().filter(|name| **name != blocked).cloned());
                    let reordered = promoted != order;
                    order = promoted;

                    if !charged && !reordered && !charged_air {
                        break;
                    }
                }
            }
        }

        harvest.charged = congestion.charged.clone();
        harvest
    }

    /// Which cell map the inflation reads.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum HeatSource {
        /// [`Congestion::charged`] -- the router's own record, and the blunt
        /// one. [`Congestion::charge`] prices every foreign-owned cell in the
        /// failed corridor's **bounding box**, cells that were never in the way
        /// included, and `Reservation::cells_within` filters on x and z only so
        /// a charge covers the whole column. §2 of the spec says as much.
        Charge,
        /// [`refusal_heat`] -- who was actually standing where the search died.
        Refusal,
    }

    /// Where the frontier died, and who was standing there.
    ///
    /// §5.7 asks for "which cells were refused and for whom, where the A\*
    /// frontier died". [`Congestion`] does not know: it prices a bounding box.
    /// This replays the blocked branch against the reservation as it stood when
    /// the search gave up, floods outward from the source under the router's own
    /// rules -- [`neighbours`], [`within_bounds`], [`anchor_is_free_for`],
    /// [`staircase_clearance`] -- and for every step the flood could not take,
    /// blames **the cell that refused it** rather than the cell it wanted. That
    /// is a per-cell contention record: the wall around the pocket, weighted by
    /// how many times it was pushed against.
    ///
    /// **Three deliberate departures from [`deterministic_astar`], each of which
    /// makes this flood reach at least as far as the real search:**
    ///
    /// 1. `self_obstructs` is not consulted. It reads the search's single global
    ///    `previous` map, and a flood has no one path; skipping it means the
    ///    flood never refuses itself, so every cell it blames is blamed on
    ///    somebody else's account.
    /// 2. Congestion pricing is ignored. A price is not a refusal -- it changes
    ///    which way the search goes, not where it may go -- and this is asking
    ///    where it *may* go.
    /// 3. `terminal_support` is passed as `goal`. The real value is the socket
    ///    one cell further in, which `route_in_order` has and `RoutingFailure`
    ///    does not carry. It costs one exemption at the goal cell only.
    ///
    /// So this over-states reach and therefore under-states the wall. It is a
    /// diagnostic, and it is not the router.
    fn refusal_heat(
        start: Anchor,
        goal: Anchor,
        owner: &str,
        reservation: &Reservation,
    ) -> BTreeMap<Anchor, u64> {
        // The same box `deterministic_astar` bounds itself with.
        let margin = manhattan_distance(start, goal).saturating_add(2) as i32;
        const CLIMB: i32 = 3;
        let min = Anchor {
            x: start.x.min(goal.x).saturating_sub(margin),
            y: start.y.min(goal.y),
            z: start.z.min(goal.z).saturating_sub(margin),
        };
        let max = Anchor {
            x: start.x.max(goal.x).saturating_add(margin),
            y: start.y.max(goal.y).saturating_add(CLIMB),
            z: start.z.max(goal.z).saturating_add(margin),
        };

        let mut blamed: BTreeMap<Anchor, u64> = BTreeMap::new();
        let mut reached = BTreeSet::from([start]);
        let mut queue = std::collections::VecDeque::from([start]);

        while let Some(at) = queue.pop_front() {
            for next in neighbours(at) {
                if !within_bounds(next, min, max) || reached.contains(&next) {
                    continue;
                }
                let free = anchor_is_free_for(next, start, goal, goal, owner, reservation);
                let stairs: Vec<Anchor> = staircase_clearance(at, next)
                    .into_iter()
                    .filter(|cell| {
                        let is_riser = next.y > at.y && cell.y == at.y;
                        if is_riser {
                            let foreign = reservation.owner(cell).is_some_and(|by| {
                                by != owner && by != stair_guard(owner)
                            });
                            foreign || reservation.conductor_owner(cell).is_some()
                        } else {
                            reservation.owner(cell).is_some()
                        }
                    })
                    .collect();

                if free && stairs.is_empty() {
                    reached.insert(next);
                    queue.push_back(next);
                    continue;
                }

                // Who refused it. Every arm of `anchor_is_free_for`, asked
                // again so the answer names a cell rather than a boolean.
                if next != start
                    && next != goal
                    && reservation.owner(&next).is_some_and(|by| by != owner)
                {
                    *blamed.entry(next).or_insert(0) += 1;
                }
                let below = Anchor { y: next.y - 1, ..next };
                if reservation.conductor_owner(&below).is_some() {
                    *blamed.entry(below).or_insert(0) += 1;
                }
                for neighbour in keep_out(next) {
                    if neighbour == start || neighbour == goal {
                        continue;
                    }
                    if reservation
                        .conductor_owner(&neighbour)
                        .is_some_and(|by| by != owner)
                    {
                        *blamed.entry(neighbour).or_insert(0) += 1;
                    }
                }
                for cell in stairs {
                    *blamed.entry(cell).or_insert(0) += 1;
                }
            }
        }

        blamed
    }

    /// How a measured congestion map is turned back into per-body room.
    ///
    /// Every rule below is **cumulative**: the requirement vector carries
    /// forward between iterations, so a body in a region that stays hot keeps
    /// growing. None of them ever shrinks a requirement, so the placement's area
    /// is monotone in the iteration -- which is what makes the area column
    /// readable as a price.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Inflation {
        /// Nothing. Iteration 1 of every run is this, and it is exactly
        /// `plan_from_netlist`; a run *entirely* of this is the do-nothing
        /// control that says the loop's repetition alone changes nothing.
        Nothing,
        /// The hottest `share` of nodes get their body's requirement multiplied
        /// by `growth`.
        HotShare { share: f64, growth: f64 },
        /// Every node's body gains `gain * heat / hottest`.
        Proportional { gain: f64 },
        /// Every body gains `gain`, hot or not.
        ///
        /// **The control that decides what the answer means.** Uniform 2x
        /// scaling of the anchors is already refuted (§3.2), but uniform
        /// *separation* is the thing an inflation rule has to beat: if this does
        /// as well as `HotShare` at the same area, nothing about the measurement
        /// was congestion-driven.
        Uniform { gain: f64 },
    }

    impl Inflation {
        fn parse(text: &str) -> Option<Inflation> {
            let (head, rest) = text.split_once(':').unwrap_or((text, ""));
            let numbers: Vec<f64> = rest
                .split(',')
                .filter(|piece| !piece.is_empty())
                .map(|piece| piece.parse().expect("an inflation parameter is a number"))
                .collect();
            match (head, numbers.as_slice()) {
                ("nothing", []) => Some(Inflation::Nothing),
                ("hot", [share, growth]) => {
                    Some(Inflation::HotShare { share: *share, growth: *growth })
                }
                ("proportional", [gain]) => Some(Inflation::Proportional { gain: *gain }),
                ("uniform", [gain]) => Some(Inflation::Uniform { gain: *gain }),
                _ => None,
            }
        }

        fn label(&self) -> String {
            match self {
                Inflation::Nothing => "nothing".to_string(),
                Inflation::HotShare { share, growth } => {
                    format!("hot:{share},{growth}")
                }
                Inflation::Proportional { gain } => format!("proportional:{gain}"),
                Inflation::Uniform { gain } => format!("uniform:{gain}"),
            }
        }

        /// Apply this rule to `required`, in place.
        ///
        /// `heat` is indexed by candidate node and `required` by body, so
        /// `anchor_body` is the map between them. A body no node anchors -- a
        /// merge's welded repeater -- is reachable only by [`Inflation::Uniform`]
        /// and is left alone by the other two. Neither `segment_a` nor
        /// `seven_segment` has one (§7's table: 0 merges, 0 welds in both), so
        /// nothing measured here depends on that choice; the harness prints the
        /// body and node counts so a reader can see it for themselves.
        fn apply(&self, required: &mut [f64], heat: &[u64], anchor_body: &[usize]) {
            match *self {
                Inflation::Nothing => {}
                Inflation::HotShare { share, growth } => {
                    let mut ranked: Vec<usize> = (0..heat.len()).filter(|n| heat[*n] > 0).collect();
                    // Hottest first, ties by node index so the same map always
                    // inflates the same bodies.
                    ranked.sort_by(|left, right| {
                        heat[*right].cmp(&heat[*left]).then(left.cmp(right))
                    });
                    let take = ((heat.len() as f64) * share).ceil() as usize;
                    for node in ranked.into_iter().take(take) {
                        required[anchor_body[node]] *= growth;
                    }
                }
                Inflation::Proportional { gain } => {
                    let hottest = heat.iter().copied().max().unwrap_or(0);
                    if hottest == 0 {
                        return;
                    }
                    for (node, &here) in heat.iter().enumerate() {
                        required[anchor_body[node]] += gain * here as f64 / hottest as f64;
                    }
                }
                Inflation::Uniform { gain } => {
                    for entry in required.iter_mut() {
                        *entry += gain;
                    }
                }
            }
        }
    }

    /// The X by Z bounding box of a set of node anchors, inclusive -- the metric
    /// `measure_anchor_boxes` states and the one every area in the spec is in
    /// (relaxed `segment_a` 8,099; legacy 23,220).
    fn anchor_box(anchors: &[Anchor]) -> (i32, i32, i64) {
        let mut min = (i32::MAX, i32::MAX);
        let mut max = (i32::MIN, i32::MIN);
        for anchor in anchors {
            min = (min.0.min(anchor.x), min.1.min(anchor.z));
            max = (max.0.max(anchor.x), max.1.max(anchor.z));
        }
        let (width, depth) = (max.0 - min.0 + 1, max.1 - min.1 + 1);
        (width, depth, width as i64 * depth as i64)
    }

    /// One circuit, one inflation rule, N iterations of trial-route → measure →
    /// inflate → re-place.
    ///
    /// **It does not stop at the first iteration that routes**, and the first
    /// version of it did. That version reported `segment_a` ROUTED at iteration
    /// 6 and returned -- over a candidate that fails [`verify_candidate`] with
    /// `signal-strength violation: net g4 never delivers a non-zero signal to
    /// gate g23`. Routing is not the acceptance condition (§10: "and4 and
    /// full_adder must keep routing **and verifying**"), and a harness that
    /// stops on the weaker of the two would have reported a win that is not one.
    /// So it runs the whole budget and reports the first iteration that routes
    /// and, separately, the first that also verifies.
    fn congestion_driven_placement(
        name: &str,
        netlist: &Netlist,
        rule: Inflation,
        source: HeatSource,
        iterations: usize,
        rounds: usize,
        radius: i32,
    ) {
        use crate::compile::primitive_graph::expand;
        use std::time::Instant;

        let placements = PortPlacements::default();
        let start = starting_layout(netlist, &placements).expect("lays out");
        let graph = expand(netlist, &Library::default_library()).expect("expands");
        let bodies = relax::build(netlist, &graph, &start, &placements).expect("builds bodies");
        let anchor_body = bodies.anchor_body.clone();
        let mut required = relax::required_separations(&bodies);

        eprintln!(
            "== {name}: {} gates, {} bodies, {} nodes | rule {} | heat {source:?} | \
             radius {radius} | {rounds} rip-up rounds ==",
            netlist.gates.len(),
            required.len(),
            anchor_body.len(),
            rule.label(),
        );

        let mut first_routed: Option<(usize, i64)> = None;
        let mut first_verified: Option<(usize, i64)> = None;

        for iteration in 1..=iterations {
            let started = Instant::now();
            let placement = match relax::relax_with_required(
                netlist,
                &graph,
                &start,
                &placements,
                relax::Axes::IN_PLANE,
                relax::RelaxEffort::default(),
                &required,
            ) {
                Ok(placement) => placement,
                Err(error) => {
                    eprintln!("  iter {iteration:2}: RELAX FAILED: {error}");
                    break;
                }
            };
            let snapped = match relax::snap(&placement) {
                Ok(snapped) => snapped,
                Err(error) => {
                    eprintln!("  iter {iteration:2}: SNAP FAILED: {error}");
                    break;
                }
            };
            let anchors: Vec<Anchor> = snapped.iter().map(|node| node.anchor).collect();
            let (width, depth, area) = anchor_box(&anchors);

            let candidate = candidate_from_snapped(netlist, &placements, &snapped);
            let harvest = harvest_routing(candidate, netlist, rounds);
            let (cells, charge) = harvest.contention();

            let lowest = required.iter().copied().fold(f64::INFINITY, f64::min);
            let highest = required.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mean = required.iter().sum::<f64>() / required.len() as f64;

            let recurring = match harvest.recurring() {
                Some((net, corridor, count)) => {
                    format!("{net} x{count} {}", address(corridor))
                }
                None => "-".to_string(),
            };
            let last = match &harvest.last {
                Some(error) => error.to_string(),
                None => "-".to_string(),
            };

            eprintln!(
                "  iter {iteration:2}: box {width}x{depth}={area} | laid {}/{} | \
                 {cells} contested cells, charge {charge} | {} refused cells, blame {} | \
                 rounds {} | req {lowest:.2}..{highest:.2} mean {mean:.2} | {:.1}s",
                harvest.deepest_laid,
                harvest.nets,
                harvest.refused.len(),
                harvest.refused.values().sum::<u64>(),
                harvest.rounds,
                started.elapsed().as_secs_f64(),
            );
            eprintln!("            recurring: {recurring}");
            eprintln!("            last:      {last}");

            if let Some(routed) = &harvest.routed {
                // Routing is not the acceptance condition. A candidate that
                // routes and does not verify has bought nothing, and two
                // placement prototypes on this branch turned route failures into
                // signal-strength and torch-merge violations -- so the probe
                // asks the same question §10 asks, and keeps going either way.
                let cost = routed.cost();
                first_routed.get_or_insert((iteration, area));
                match verify_candidate(routed, netlist) {
                    Ok(()) => {
                        first_verified.get_or_insert((iteration, area));
                        eprintln!(
                            "            ROUTED and VERIFIES | wire {} delay {} turns {}",
                            cost.wire, cost.delay, cost.turns
                        );
                    }
                    Err(error) => eprintln!("            ROUTED, DOES NOT VERIFY: {error}"),
                }
            }

            // Heat: the charge on every cell within `radius` of a node's anchor,
            // Chebyshev in plan. A body's "region" is the ground it and the
            // corridors reaching it stand on, and the relaxed layout's
            // nearest-neighbour anchor distance is a median 7 (§3.2), so a
            // radius near that is one body's neighbourhood rather than the whole
            // plane. It is a guess; the harness sweeps it.
            let mut heat = vec![0u64; anchors.len()];
            for (cell, charge) in harvest.map_for(source) {
                for (node, anchor) in anchors.iter().enumerate() {
                    if (cell.x - anchor.x).abs() <= radius && (cell.z - anchor.z).abs() <= radius {
                        heat[node] += charge;
                    }
                }
            }
            let hot = heat.iter().filter(|value| **value > 0).count();
            eprintln!(
                "            heat: {hot}/{} nodes touched, hottest {}",
                heat.len(),
                heat.iter().copied().max().unwrap_or(0)
            );

            rule.apply(&mut required, &heat, &anchor_body);
        }

        // Completed after the probe was interrupted mid-edit: the body had
        // accumulated `first_routed`/`first_verified` and then reported "never
        // routed" unconditionally, which would have called a success a failure.
        // Area is carried alongside because routing at a size larger than the
        // legacy placement's 23,220 is not a win, and the headline has to say so.
        match (first_routed, first_verified) {
            (None, _) => eprintln!("  {name}: {iterations} iterations, never routed"),
            (Some((round, area)), None) => eprintln!(
                "  {name}: routed at iteration {round}, anchor box {area} -- NEVER VERIFIED"
            ),
            (Some((round, area)), Some((ok, verified_area))) => eprintln!(
                "  {name}: routed at iteration {round} (box {area}), \
                 verified at {ok} (box {verified_area}); legacy routes at 23,220"
            ),
        }
    }

    /// **§5.7.** Does congestion-driven placement route `segment_a`?
    ///
    /// The idea this tests, in the spec's own words: the relaxation already *is*
    /// a router -- `pulls` are the nets and minimising spring energy is the
    /// continuous relaxation of "make the wires short" -- and it already models
    /// routing space, since `required_separations` charges every body
    /// `CONDUCTOR_CLEARANCE + reservation(d) + SNAP_MARGIN`. What it gets wrong
    /// is the *shape* of that allowance: an isotropic ring per body, which
    /// cannot see that two nets' corridors cross. So rather than replace the
    /// router, make the placer hear it.
    ///
    /// The loop, which is textbook congestion-driven analytical placement:
    ///
    /// 1. relax, snap, and hand the result to the shipping `route_every_net`;
    /// 2. on failure, harvest the router's own congestion map;
    /// 3. inflate `required[body]` for the bodies in the hot regions;
    /// 4. re-relax against the inflated vector, and repeat.
    ///
    /// Step 3 needs no new mechanism: [`relax::required_separations`] already
    /// returns a per-body `Vec<f64>` and [`relax::project`] already takes one as
    /// a parameter. The one thing missing was a way to hand `relax` a vector it
    /// did not derive, which is [`relax::relax_with_required`] and is
    /// `#[cfg(test)]`.
    ///
    /// **Iteration 1 is the control, in every run.** It relaxes against
    /// `required_separations` unmodified, so it *is* `plan_from_netlist`: if its
    /// row does not read `box 89x91=8099 | laid 36/47` for `segment_a`, the
    /// probe has stopped measuring the shipping path and nothing below it means
    /// anything.
    ///
    /// # What it measured
    ///
    /// Run on 2026-08-15 at this HEAD, `--release`. See the ledger for the
    /// table; the summary is that **no rule tried routes `segment_a`**, and the
    /// one that gets furthest does so by growing the plane.
    ///
    /// # Re-running it, and sweeping it
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::measure_whether_congestion_driven_placement_routes \
    ///   -- --ignored --nocapture
    /// ```
    ///
    /// Five environment variables, each with the default that produced the
    /// reported numbers, so the default run is the reproduction and an override
    /// is a sweep:
    ///
    /// | variable | default | what it is |
    /// |---|---|---|
    /// | `REDA_PROBE_CIRCUIT` | `segment_a` | `and4`, `full_adder`, `segment_a`, `seven_segment`, or `all` |
    /// | `REDA_PROBE_RULES` | `hot:0.25,1.4 proportional:2 uniform:0.5` | space-separated; `nothing` is the do-nothing control |
    /// | `REDA_PROBE_ITERATIONS` | `10` | §5.7 asks for 8-12 |
    /// | `REDA_PROBE_ROUNDS` | `64` | rip-up rounds, `RIP_UP_ROUNDS` |
    /// | `REDA_PROBE_RADIUS` | `6` | the Chebyshev plan radius a node collects charge over |
    ///
    /// Asserts nothing. A probe that gates something is a probe somebody will
    /// tune until it goes green.
    #[test]
    #[ignore = "measurement harness: asserts nothing, re-places and re-routes segment_a ten times, takes many minutes"]
    fn measure_whether_congestion_driven_placement_routes() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };

        let setting = |name: &str, fallback: &str| -> String {
            std::env::var(name).unwrap_or_else(|_| fallback.to_string())
        };

        let wanted = setting("REDA_PROBE_CIRCUIT", "segment_a");
        let rules: Vec<Inflation> = setting(
            "REDA_PROBE_RULES",
            "hot:0.25,1.4 proportional:2 uniform:0.5",
        )
        .split_whitespace()
        .map(|text| Inflation::parse(text).expect("an inflation rule this harness knows"))
        .collect();
        let iterations: usize = setting("REDA_PROBE_ITERATIONS", "12").parse().expect("a count");
        let rounds: usize = setting("REDA_PROBE_ROUNDS", "64").parse().expect("a count");
        let radius: i32 = setting("REDA_PROBE_RADIUS", "6").parse().expect("a radius");
        let source = match setting("REDA_PROBE_HEAT", "charge").as_str() {
            "charge" => HeatSource::Charge,
            "refusal" => HeatSource::Refusal,
            other => panic!("REDA_PROBE_HEAT is charge or refusal, not {other}"),
        };

        let circuits: Vec<(&str, Netlist)> = [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
        ]
        .into_iter()
        .filter(|(name, _)| wanted == "all" || wanted == *name)
        .collect();
        assert!(!circuits.is_empty(), "REDA_PROBE_CIRCUIT names no circuit");

        for (name, netlist) in &circuits {
            for rule in &rules {
                congestion_driven_placement(
                    name, netlist, *rule, source, iterations, rounds, radius,
                );
            }
        }
    }

    // ---------------------------------------------------------------------
    // The growth probe: wire-first constructive compilation.
    //
    // Not a section of `docs/superpowers/specs/2026-08-15-routing-at-scale.md`.
    // Every candidate in there moves the router or the placer while keeping the
    // other's shape; this moves the boundary between them. Wires are ~45% of
    // the blocks and gates 3%, so wires are the primary state here and gate
    // positions are derived: nets grow as real dust trees in topological order,
    // and a gate lands at the argmin of its input nets' arrival-cost fields.
    //
    // What that is meant to delete is a measured failure, not a suspected one.
    // `segment_a`'s `g32` dies walled into a 256-cell pocket 24 manhattan from a
    // goal 31 away, and 0 of 120 net orderings escape it -- because the goal is
    // FIXED. Here the goal is an output, so "no route to the socket" cannot be
    // said: the socket goes where a route already is.
    //
    // Under the ruling that only game mechanics are hard rules, growth needs no
    // `relax::project::reservation` ring -- wires are laid before the gate
    // exists, so nothing needs room saved for it -- and no `SNAP_MARGIN`, since
    // everything is born on the lattice. What remains is the game's: conductor
    // clearance (`keep_out`), a floor under every cell, the climb rules
    // (`staircase_clearance`, `self_obstructs`), and 15-cell decay
    // (`realise_branch_from`). Every one of those is reached through the
    // shipping router's own function rather than restated.
    //
    // **The paragraph above is the premise, not a finding.** What this harness
    // measured is in `measure_whether_growth_places_and_routes`'s own doc, and
    // on the output side it goes the other way: every wedge measured is a
    // gate's *output* pin sealed by gates placed after it, which is the same
    // ring asked for from the other direction.
    // ---------------------------------------------------------------------

    /// The offset every footprint this probe measures is taken about.
    const GROWTH_ORIGIN: Anchor = Anchor { x: 0, y: 0, z: 0 };

    /// SplitMix64's finaliser over `(seed, index)`: a deterministic scramble
    /// with no crate behind it and no state to thread.
    ///
    /// The order sweep needs ~20 *different* orders that are the same on every
    /// machine and every re-run. Anything drawn from a real RNG would make the
    /// completion rate a number nobody else can reproduce, which rule 4 refuses.
    fn scrambled(seed: u64, index: u64) -> u64 {
        let mut z = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(index.wrapping_mul(0xBF58_476D_1CE4_E5B9));
        z ^= z >> 30;
        z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z ^= z >> 27;
        z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Every cell the reservation has a claim on. The diff of this before and
    /// after a branch is laid is exactly what that branch added, which is what
    /// makes rip-up remove what it put down and nothing else.
    fn claimed_cells(reservation: &Reservation) -> BTreeSet<Anchor> {
        reservation.cells.keys().copied().collect()
    }

    /// One net's dust tree as it grows, plus what a later branch of the same
    /// net needs to know about the cells earlier ones laid.
    ///
    /// `route_in_order` needs neither map: a star's every branch starts at the
    /// source, so it recovers the carried strength by walking the shared prefix
    /// from full strength. A tree has no single prefix to walk, which is what
    /// §5.2 calls "the real work" of routing a net as a tree.
    #[derive(Debug, Clone, Default)]
    struct NetTree {
        anchors: Vec<Anchor>,
        realisation: Vec<BlockState>,
        floors: Vec<BlockState>,
        /// Strength carried *out of* each laid cell.
        strength: BTreeMap<Anchor, u8>,
        /// Repeaters between the producer's pin and each laid cell, inclusive.
        /// [`RouteTerminal::repeaters`] is this at the branch point plus
        /// whatever the branch itself lays -- the sum `route_in_order` spells
        /// `trunk_repeaters + laid.repeaters`.
        repeaters: BTreeMap<Anchor, u64>,
        terminals: Vec<RouteTerminal>,
    }

    impl NetTree {
        /// Every cell a new branch of this net may start from: everything laid,
        /// plus the producer's pin, which the first branch lays and every later
        /// one finds already there.
        fn seeds(&self, pin: Anchor) -> BTreeSet<Anchor> {
            let mut seeds: BTreeSet<Anchor> = self.anchors.iter().copied().collect();
            seeds.insert(pin);
            seeds
        }
    }

    /// A multi-source Dijkstra over free space: what one net can reach from
    /// everything it has already laid, and by which parent.
    struct Flood {
        travelled: BTreeMap<Anchor, u64>,
        previous: BTreeMap<Anchor, Anchor>,
    }

    impl Flood {
        fn cost(&self, at: Anchor) -> Option<u64> {
            self.travelled.get(&at).copied()
        }

        /// The cheapest path to `goal`, ending there and beginning at whichever
        /// laid cell it grew from.
        ///
        /// [`reconstruct_path`] verbatim, and it needs no adapting: a seed is
        /// entered at `travelled = 0` and every step costs at least one, so the
        /// relaxation test never gives a seed a parent, and the walk stops at
        /// exactly the cell this branch hangs off.
        fn path_to(&self, goal: Anchor) -> Option<Vec<Anchor>> {
            self.travelled
                .contains_key(&goal)
                .then(|| reconstruct_path(self.previous.clone(), goal))
        }
    }

    /// Flood outward from everything a net has laid, under the shipping
    /// router's own step legality.
    ///
    /// **Every refusal below is `deterministic_astar`'s, arm for arm**:
    /// [`self_obstructs`] against this flood's own parent map (the same kind of
    /// object the search keeps -- one global `previous`), [`within_bounds`],
    /// [`anchor_is_free_for`], and the [`staircase_clearance`] riser rule with
    /// [`stair_guard`]. Three things differ and each is stated rather than
    /// absorbed:
    ///
    /// 1. **Many sources, one cost map.** §5.2's proposal, which growth needs
    ///    anyway: a branch grows off the trunk wherever the trunk is nearest.
    /// 2. **No congestion price.** There is no rip-up loop here to charge
    ///    anything, and a price nothing reads is a number that cannot be wrong
    ///    in an interesting way.
    /// 3. **No heuristic**, so `estimate == travelled`. A field has no goal to
    ///    estimate towards, and the branch search is the same function so the
    ///    path it finds is the one the field priced. That is also what makes
    ///    stopping when `aim`'s goal is popped exact rather than a cutoff:
    ///    under plain Dijkstra a popped cell's cost is already final.
    ///
    /// `aim` is `Some((approach, socket))` when a particular socket is being
    /// reached for, and it is passed straight through to `anchor_is_free_for`
    /// as its `goal` and `terminal_support` -- the two exemptions
    /// `route_in_order` relies on to let a branch land one cell out from a
    /// gate's input. `None` is a field: no exemption, so a field is never
    /// cheerier than the search that follows it.
    fn flood_from(
        seeds: &BTreeSet<Anchor>,
        pin: Anchor,
        aim: Option<(Anchor, Anchor)>,
        owner: &str,
        reservation: &Reservation,
        window: (Anchor, Anchor),
    ) -> Flood {
        let (min, max) = window;
        let (goal, support) = aim.unwrap_or((pin, pin));
        let mut travelled: BTreeMap<Anchor, u64> =
            seeds.iter().map(|&anchor| (anchor, 0)).collect();
        let mut previous: BTreeMap<Anchor, Anchor> = BTreeMap::new();
        let mut frontier: BTreeSet<SearchState> = seeds
            .iter()
            .map(|&anchor| SearchState { estimate: 0, travelled: 0, anchor })
            .collect();

        while let Some(state) = frontier.iter().next().copied() {
            frontier.remove(&state);
            if travelled.get(&state.anchor) != Some(&state.travelled) {
                continue;
            }
            if aim.is_some() && state.anchor == goal {
                break;
            }
            for next in neighbours(state.anchor) {
                if self_obstructs(&previous, state.anchor, next) {
                    continue;
                }
                if !within_bounds(next, min, max)
                    || !anchor_is_free_for(next, pin, goal, support, owner, reservation)
                    || staircase_clearance(state.anchor, next).into_iter().any(|cell| {
                        let foreign = reservation.owner(&cell).is_some_and(|occupied_by| {
                            occupied_by != owner && occupied_by != stair_guard(owner)
                        });
                        let is_riser = next.y > state.anchor.y && cell.y == state.anchor.y;
                        if is_riser {
                            return foreign || reservation.conductor_owner(&cell).is_some();
                        }
                        reservation.owner(&cell).is_some()
                    })
                {
                    continue;
                }
                // `deterministic_astar`'s own step cost with its `goal.y` read
                // as [`PLANNER_Y`]: every landing this probe considers is on the
                // gate plane, so "closer in y" is "coming back down", which is
                // what that arm means there too.
                const CLIMB_COST: u64 = 3;
                let closer_in_y =
                    (next.y - PLANNER_Y).abs() < (state.anchor.y - PLANNER_Y).abs();
                let step_cost = if next.y == state.anchor.y || closer_in_y {
                    1
                } else {
                    CLIMB_COST
                };
                let next_travelled = state.travelled.saturating_add(step_cost);
                if travelled
                    .get(&next)
                    .is_some_and(|&known| known <= next_travelled)
                {
                    continue;
                }
                travelled.insert(next, next_travelled);
                previous.insert(next, state.anchor);
                frontier.insert(SearchState {
                    estimate: next_travelled,
                    travelled: next_travelled,
                    anchor: next,
                });
            }
        }

        Flood { travelled, previous }
    }

    /// The socket a declared input arrives in, and the one cell it may arrive
    /// from.
    ///
    /// `route_in_order` states this twice; said once here. A terminal only
    /// reads from directly behind itself, so the approach is collinear with
    /// socket and support -- which is also what makes the enumeration below
    /// invertible.
    fn socket_and_approach(
        anchor: Anchor,
        facing: geometry::CellFacing,
        input_index: usize,
    ) -> (Anchor, Anchor) {
        let socket = step(anchor, geometry::input_directions(facing)[input_index]);
        let approach = Anchor {
            x: socket.x + (socket.x - anchor.x),
            y: socket.y + (socket.y - anchor.y),
            z: socket.z + (socket.z - anchor.z),
        };
        (socket, approach)
    }

    fn shifted(origin: Anchor, offset: Anchor) -> Anchor {
        Anchor {
            x: origin.x + offset.x,
            y: origin.y + offset.y,
            z: origin.z + offset.z,
        }
    }

    /// The box a gate's growth is searched in: everything its input nets have
    /// laid, grown by `margin` in plan and by `deterministic_astar`'s own
    /// `CLIMB` above.
    ///
    /// One window for all of a gate's input nets rather than one each, because
    /// what has to be non-empty is their *intersection*: a landing needs every
    /// socket's approach reachable by its own net.
    fn growth_window(seeds: &[Anchor], margin: i32) -> (Anchor, Anchor) {
        const CLIMB: i32 = 3;
        let mut min = Anchor { x: i32::MAX, y: i32::MAX, z: i32::MAX };
        let mut max = Anchor { x: i32::MIN, y: i32::MIN, z: i32::MIN };
        for cell in seeds {
            min = Anchor {
                x: min.x.min(cell.x),
                y: min.y.min(cell.y),
                z: min.z.min(cell.z),
            };
            max = Anchor {
                x: max.x.max(cell.x),
                y: max.y.max(cell.y),
                z: max.z.max(cell.z),
            };
        }
        (
            Anchor {
                // Never left of or before the origin. A gate at `x = -6` has its
                // blocks written outside the world and its socket reads as air
                // however carefully a route reached it, which is the hazard
                // `claim_column` guards on the placement side.
                x: (min.x - margin).max(0),
                y: min.y.max(PLANNER_Y),
                z: (min.z - margin).max(0),
            },
            Anchor {
                x: max.x + margin,
                y: max.y + CLIMB,
                z: max.z + margin,
            },
        )
    }

    /// Where a gate could land, which way it would face, and what that costs.
    struct Landing {
        anchor: Anchor,
        facing: geometry::CellFacing,
        /// Arrival cost at each declared input's approach cell, in declared
        /// input order.
        arrival: Vec<u64>,
        pull: f64,
        score: f64,
    }

    /// How many landings survived each stage of the enumeration.
    ///
    /// The wedge report's whole content: "no landing" without saying which test
    /// removed them all is the uninformative address §1.1 is about, one level
    /// up.
    #[derive(Debug, Clone, Copy, Default)]
    struct Funnel {
        offered: usize,
        approaches_met: usize,
        body_fits: usize,
    }

    /// Whether a gate body may stand at `origin`, given everything already
    /// laid.
    ///
    /// This is the rule `route_in_order` gets for free and growth has to ask
    /// for. There the gates are placed first and `keep_out` keeps the wires off
    /// them; here the wires are laid first, so the same clearance has to be
    /// asked from the gate's side -- no cell of the body over anything, and no
    /// foreign conductor within `keep_out` of anything of the body that
    /// conducts.
    ///
    /// The one exemption is the one the router already makes from the other
    /// direction: a socket is *meant* to have its own net's dust one cell out,
    /// which is `anchor_is_free_for`'s `anchor == goal && neighbour ==
    /// terminal_support` read backwards.
    ///
    /// `cells` and `conductors` are offsets about [`GROWTH_ORIGIN`], not
    /// positions. [`compile::gate_footprint`] realises a gate into a 64x8x64
    /// scratch world and scans all 32,768 cells of it, and this is asked of
    /// every cell of a field times four facings -- so it is asked once per
    /// facing and translated, which the function's own arithmetic permits: its
    /// answer is `origin` plus an offset that depends on nothing but arity,
    /// merge-ness and facing.
    struct BodyFit<'a> {
        origin: Anchor,
        cells: &'a [Anchor],
        conductors: &'a [Anchor],
        sockets: &'a [Anchor],
        approaches: &'a [Anchor],
        drivers: &'a [String],
        pins: &'a [Anchor],
        /// Where this body's own output net will start.
        pin: Anchor,
        /// How many legal first steps that net must be left, or `0` to ask for
        /// none -- which is the sketch as briefed, and the default, so the
        /// reported baseline is the paradigm and not a repair of it.
        escape: usize,
    }

    impl BodyFit<'_> {
        fn allowed(&self, reservation: &Reservation) -> bool {
            for offset in self.cells {
                if reservation.is_taken(&shifted(self.origin, *offset)) {
                    return false;
                }
            }
            // The approach has to be free, already this net's, or the
            // producer's own pin -- the last of which is the paradigm's whole
            // point, a socket landing directly on dust that is already there.
            for (index, approach) in self.approaches.iter().enumerate() {
                let mine = match reservation.owner(approach) {
                    None => true,
                    Some(occupied_by) => occupied_by == self.drivers[index],
                } || *approach == self.pins[index];
                if !mine {
                    return false;
                }
            }
            for offset in self.conductors {
                let conductor = shifted(self.origin, *offset);
                // `keep_out`, not `keep_out_against`: the cell under test is this
                // gate's conductor, and the derived join relation is about wire
                // against wire. See [`Occupancy::GateConductor`].
                for neighbour in keep_out(conductor) {
                    if reservation.conductor_owner(&neighbour).is_none() {
                        continue;
                    }
                    let arriving = self.sockets.iter().enumerate().any(|(index, socket)| {
                        conductor == *socket && neighbour == self.approaches[index]
                    });
                    if !arriving {
                        return false;
                    }
                }
            }
            self.escape == 0 || self.escapes(reservation) >= self.escape
        }

        /// The occupied cells that made [`allowed`](Self::allowed) say no.
        ///
        /// Same three arms in the same order, reporting instead of returning.
        /// Rip-up needs the cell, not the verdict: "this landing does not fit"
        /// is the uninformative address again, one level down.
        fn blockers(&self, reservation: &Reservation, into: &mut BTreeSet<Anchor>) {
            for offset in self.cells {
                let cell = shifted(self.origin, *offset);
                if reservation.is_taken(&cell) {
                    into.insert(cell);
                }
            }
            for (index, approach) in self.approaches.iter().enumerate() {
                let mine = match reservation.owner(approach) {
                    None => true,
                    Some(occupied_by) => occupied_by == self.drivers[index],
                } || *approach == self.pins[index];
                if !mine {
                    into.insert(*approach);
                }
            }
            for offset in self.conductors {
                let conductor = shifted(self.origin, *offset);
                // `keep_out`, not `keep_out_against`: the cell under test is this
                // gate's conductor, and the derived join relation is about wire
                // against wire. See [`Occupancy::GateConductor`].
                for neighbour in keep_out(conductor) {
                    if reservation.conductor_owner(&neighbour).is_none() {
                        continue;
                    }
                    let arriving = self.sockets.iter().enumerate().any(|(index, socket)| {
                        conductor == *socket && neighbour == self.approaches[index]
                    });
                    if !arriving {
                        into.insert(neighbour);
                    }
                }
            }
        }

        /// How many legal first steps this body's own output net would have out
        /// of its pin, once the body is standing here.
        ///
        /// `anchor_is_free_for` as it will answer a moment from now. The body is
        /// not in the reservation yet, so its own cells are asked of the offset
        /// lists instead of of the map, and the pin is that net's `start` --
        /// therefore the one conductor a first step is allowed to sit beside.
        ///
        /// **Why this is a knob and not a rule.** The framing says growth needs
        /// no reserved ring because "wires are laid before the gate exists, so
        /// nothing needs room saved for it". That is true of a gate's *inputs*
        /// and false of its *output*: the output net does not exist when the
        /// gate lands, so nothing keeps the gates placed afterwards off it. Set
        /// this above zero and the claim is under test rather than assumed.
        fn escapes(&self, reservation: &Reservation) -> usize {
            let mine = |cell: Anchor| {
                self.cells.iter().any(|offset| shifted(self.origin, *offset) == cell)
            };
            let mine_conducts = |cell: Anchor| {
                self.conductors.iter().any(|offset| shifted(self.origin, *offset) == cell)
            };
            neighbours(self.pin)
                .into_iter()
                .filter(|&next| {
                    let below = Anchor { y: next.y - 1, ..next };
                    next.y >= PLANNER_Y
                        && !reservation.is_taken(&next)
                        && !mine(next)
                        && reservation.conductor_owner(&below).is_none()
                        && !mine_conducts(below)
                        && keep_out(next).into_iter().all(|neighbour| {
                            neighbour == self.pin
                                || (reservation.conductor_owner(&neighbour).is_none()
                                    && !mine_conducts(neighbour))
                        })
                })
                .count()
        }
    }

    /// Why one landing could not be laid, kept so a wedge report says what was
    /// tried rather than that something was.
    struct LayRefusal {
        input: usize,
        /// Owned rather than `&'static str`: a supplied route's refusal has to
        /// name the cell and the rule, which is the difference between "the
        /// model has an encoding gap" and "the model has an encoding gap at
        /// (26, 1, 146), keep_out".
        why: String,
    }

    /// A gate the growth could not place, and everything known about why.
    struct Wedge {
        gate: String,
        depth: usize,
        arity: usize,
        windows: Vec<i32>,
        funnel: Funnel,
        fields: Vec<(String, usize)>,
        seals: Vec<Seal>,
        refusals: Vec<String>,
        /// Every occupied cell measured to be standing in this gate's way, so
        /// rip-up has something to aim at that is not a guess.
        blame: BTreeSet<Anchor>,
    }

    /// A net whose field adds nothing to what it has already laid: it cannot
    /// leave its own producer, and these are the cells standing on it.
    ///
    /// The distinction this exists to make is the whole diagnosis. "No landing"
    /// has two completely different causes -- **the fields could not meet**,
    /// which is a congestion story and the one the paradigm predicts it has
    /// deleted, and **a field is a single cell**, which is not about routing at
    /// all: the driver was placed with nothing reserving its way out, and gates
    /// placed afterwards closed around its output pin. Reporting only "0
    /// landings" would let a reader pick whichever they already believed.
    ///
    /// The blame flood is [`refusal_heat`], started at the producer's **pin**,
    /// so for a net that has already laid cells elsewhere this names the ring
    /// around the pin and not the whole tree's boundary. Every seal measured so
    /// far is a net whose tree is the bare pin, where the two are the same.
    struct Seal {
        signal: String,
        pin: Anchor,
        blamed: Vec<(Anchor, String, u64)>,
    }

    /// How a growth run is tuned. Every field is an environment variable with
    /// the default that produced this harness's own reported numbers, so the
    /// default run is the reproduction and an override is a sweep.
    #[derive(Debug, Clone)]
    struct GrowthSettings {
        order: String,
        lambda: f64,
        windows: Vec<i32>,
        tries: usize,
        escape: usize,
        seed_pitch: i32,
        verbose: bool,
        settle: bool,
        /// Rip-up budget: how many times a wedged gate may tear out the
        /// youngest wire standing in its way and try again. `0` is v1 -- the
        /// paradigm as briefed, which has no rip-up at all -- and is the
        /// default so the committed baseline stays the reproduction.
        rip: usize,
        /// A deterministic growth-order seed, or `0` for the `order` policy.
        /// Non-zero replaces the ready queue's key with a seeded permutation,
        /// which is the growth-side analogue of the 120-net-ordering shuffle.
        seed: u64,
        /// Rip the victim's whole net rather than the youngest branch and what
        /// hangs off it. Strictly more destructive, and it exists to separate
        /// two explanations of a rip-up that completes and then fails verify:
        /// a re-laid branch reading a stale carried strength off a trunk, or
        /// the geometry. A whole net is re-grown from its pin at full
        /// strength, so there is no trunk left to read.
        rip_whole: bool,
    }

    /// One branch of one net, as laid: everything needed to tear it out again
    /// and everything needed to put it back.
    ///
    /// `path` is the whole walk including its root, so a surviving branch can
    /// be replayed through [`reserve_path`] verbatim rather than through a
    /// second, differently-wrong reconstruction of what it claimed. `claimed`
    /// is the exact set of reservation keys this branch's laying *added*,
    /// diffed rather than predicted, because [`Reservation::insert`] is
    /// first-writer-wins and a branch running beside an older one claims fewer
    /// cells than its path has.
    #[derive(Debug, Clone)]
    struct Laid {
        serial: u64,
        signal: String,
        consumer: usize,
        input: usize,
        socket: Anchor,
        predecessor: Anchor,
        /// The cell this branch grew off. Always a cell the net already had --
        /// under plain Dijkstra from cost-0 seeds only a seed lacks a parent,
        /// so `path[0]` is the one laid cell any branch touches.
        root: Anchor,
        path: Vec<Anchor>,
        added: Vec<Anchor>,
        claimed: Vec<Anchor>,
    }

    /// One entry in the chronological claim log, replayed in order to rebuild
    /// the reservation after a rip-up.
    ///
    /// Rebuilding rather than un-inserting is what makes rip-up safe here:
    /// `insert` is first-writer-wins, so a cell two things wanted is recorded
    /// against the first, and removing that first thing would silently leave
    /// the second's block standing on an unclaimed cell for a later net to take.
    #[derive(Debug, Clone, Copy)]
    enum Claim {
        Body(usize),
        Approaches(usize),
        Branch(u64),
    }

    /// The growing world: one reservation, one dust tree per net, and the
    /// candidate being assembled underneath them.
    struct Growth<'a> {
        netlist: &'a Netlist,
        settings: &'a GrowthSettings,
        reservation: Reservation,
        trees: BTreeMap<String, NetTree>,
        /// Where each signal's dust leaves its producer.
        pins: BTreeMap<String, Anchor>,
        sinks: BTreeMap<String, Vec<(usize, usize)>>,
        depths: Vec<usize>,
        anchors: Vec<Anchor>,
        facings: Vec<geometry::CellFacing>,
        nodes: Vec<Option<PrimitiveNode>>,
        placed: Vec<bool>,
        wedge: Option<Wedge>,
        /// Every branch standing, youngest last.
        laid: Vec<Laid>,
        /// The chronological claim log the reservation is rebuilt from.
        log: Vec<Claim>,
        serial: u64,
        /// Rip-ups spent, and what each one tore out.
        ripped: usize,
        /// Branches torn out and successfully re-grown. `ripped` without this
        /// is a demolition count; the pair is the loop.
        relaid: usize,
        rip_log: Vec<String>,
        /// Branches torn out and not yet put back. A plan that finishes with
        /// this non-empty has a gate reading from nothing, so it is reported
        /// rather than handed to `candidate`.
        orphans: BTreeSet<(usize, usize)>,
    }

    impl<'a> Growth<'a> {
        /// Seed: every primary input's lever, and nothing else. Every gate is
        /// unplaced, which is what makes the ready queue mean something.
        fn seeded(netlist: &'a Netlist, settings: &'a GrowthSettings) -> Result<Self, String> {
            let placements = PortPlacements::default();
            let start = starting_layout(netlist, &placements).map_err(|error| error.to_string())?;
            let gates = netlist.gates.len();
            let nodes = gates + netlist.inputs.len();

            let mut growth = Growth {
                netlist,
                settings,
                reservation: Reservation::new(),
                trees: BTreeMap::new(),
                pins: BTreeMap::new(),
                sinks: net_sinks(netlist),
                depths: gate_depths(netlist).map_err(|error| error.to_string())?,
                anchors: vec![Anchor { x: 0, y: PLANNER_Y, z: 0 }; nodes],
                facings: vec![geometry::CellFacing::NORTH; nodes],
                nodes: vec![None; nodes],
                placed: vec![false; gates],
                wedge: None,
                laid: Vec::new(),
                log: Vec::new(),
                serial: 0,
                ripped: 0,
                relaid: 0,
                rip_log: Vec::new(),
                orphans: BTreeSet::new(),
            };

            for (index, input) in netlist.inputs.iter().enumerate() {
                let node = gates + index;
                // `starting_layout`'s own input row, or a pitch this harness was
                // told to use instead. The row is **not** growth's answer --
                // nothing has grown yet when the levers go down -- so it is a
                // knob, and the anchor box below inherits whatever it says.
                let anchor = if settings.seed_pitch > 0 {
                    Anchor {
                        x: INPUT_COLUMN_X + index as i32 * settings.seed_pitch,
                        ..start[node]
                    }
                } else {
                    start[node]
                };
                let facing = geometry::CellFacing::NORTH;
                let (cells, pin) = compile::lever_footprint(anchor, facing);
                let primitive = PrimitiveNode {
                    id: format!("input:{input}"),
                    anchor,
                    realisation: NodeRealisation::Primitive(Primitive::Lever),
                    footprint: cells.clone(),
                    conductors: cells,
                    pinned: placements.get(input).is_some(),
                    output_pin: Some(pin),
                };
                let owner = format!("primitive:{node}");
                for &cell in primitive.occupied() {
                    growth
                        .reservation
                        .insert(cell, &owner, primitive.occupancy_of(cell));
                }
                growth.anchors[node] = anchor;
                growth.facings[node] = facing;
                growth.nodes[node] = Some(primitive);
                growth.pins.insert(input.clone(), pin);
                growth.log.push(Claim::Body(node));
            }

            Ok(growth)
        }

        /// The ready queue's key for a gate: most-constrained-first, with a
        /// deterministic tail so the same netlist always grows the same way.
        fn key(&self, gate: usize) -> (i64, i64, usize) {
            let depth = self.depths[gate] as i64;
            let arity = self.netlist.gates[gate].inputs.len() as i64;
            // A seed replaces the policy outright rather than breaking its
            // ties, because a tie-break inside `(depth, -arity)` moves almost
            // nothing: it is the growth-side twin of the 120 net-order
            // shuffles, and those permuted the whole order.
            if self.settings.seed != 0 {
                return (scrambled(self.settings.seed, gate as u64) as i64, 0, gate);
            }
            match self.settings.order.as_str() {
                "arity" => (-arity, depth, gate),
                "index" => (gate as i64, 0, gate),
                _ => (depth, -arity, gate),
            }
        }

        /// Whether every gate driving this one has been placed. Primary inputs
        /// are placed by the seed, so they never hold anything back.
        fn ready(&self, gate: usize) -> bool {
            self.netlist.gates[gate]
                .inputs
                .iter()
                .all(|signal| self.pins.contains_key(signal))
        }

        /// Grow the whole circuit, or stop at the first wedge.
        fn grow(&mut self) {
            self.grow_stopping_before(None);
        }

        /// [`grow`](Self::grow), pausing just before it would land the gate
        /// whose output signal is `stop` and returning that gate's index.
        ///
        /// The windowed solver needs a growth state, and the only honest way to
        /// get one is to let growth build it: a state assembled by hand would be
        /// a second statement of what growth does. With `stop` at `None` this is
        /// `grow` verbatim -- the added line cannot fire -- which is why `grow`
        /// delegates rather than keeping a second copy of the queue.
        fn grow_stopping_before(&mut self, stop: Option<&str>) -> Option<usize> {
            // `!placed` as well as `ready`, which is a no-op on a fresh growth
            // -- nothing is placed when this first runs -- and is what lets a
            // paused growth be resumed after something else landed a gate.
            let mut queue: BTreeSet<(i64, i64, usize)> = (0..self.netlist.gates.len())
                .filter(|&gate| !self.placed[gate] && self.ready(gate))
                .map(|gate| self.key(gate))
                .collect();

            while let Some(&entry) = queue.iter().next() {
                queue.remove(&entry);
                let gate = entry.2;
                if stop == Some(self.netlist.gates[gate].output.as_str()) {
                    return Some(gate);
                }
                loop {
                    match self.land(gate) {
                        Ok(()) => {
                            self.placed[gate] = true;
                            for consumer in 0..self.netlist.gates.len() {
                                if !self.placed[consumer] && self.ready(consumer) {
                                    queue.insert(self.key(consumer));
                                }
                            }
                            break;
                        }
                        Err(wedge) => {
                            // v1 as briefed has no rip-up and stops here. With a
                            // budget, the wedge is a request: tear out the
                            // youngest wire standing in this gate's way and ask
                            // again. What is torn out goes on the re-lay list,
                            // so nothing is quietly lost.
                            if self.ripped >= self.settings.rip {
                                self.wedge = Some(*wedge);
                                return None;
                            }
                            let torn = self.rip_youngest(&wedge.blame, &wedge.gate);
                            if torn.is_empty() {
                                // Nothing in the way belongs to a wire. Rip-up
                                // cannot reach a gate body, so this is a wedge
                                // the budget does not address, and saying so is
                                // the measurement.
                                self.wedge = Some(*wedge);
                                return None;
                            }
                            self.ripped += 1;
                            self.orphans.extend(torn);
                        }
                    }
                }
                self.settle_orphans();
            }
            None
        }

        /// Rebuild the reservation from the claim log.
        ///
        /// Not "remove what the ripped branch inserted": [`Reservation::insert`]
        /// is first-writer-wins, so a cell two owners wanted is recorded against
        /// the first, and removing that first owner would leave the second's
        /// block standing on a cell the next net is free to take. Replaying the
        /// log gives every surviving claim its original chance in its original
        /// order, and the holes that open are exactly the ripped branch's.
        fn reseat(&mut self) {
            let mut fresh = Reservation::new();
            let log = self.log.clone();
            // Recomputed rather than carried, because first-writer-wins means a
            // cell two branches wanted changes hands when the first is torn
            // out, and a stale `claimed` set would then hide a blocking cell
            // from rip-up.
            //
            // **Measured inert, and recorded as such.** Disabling this
            // recompute left `full_adder` bit for bit identical on seeds 4, 6,
            // 8, 12 and 24 -- including the two 64-of-64 runs that re-lay 62
            // and 122 branches. It is kept because it is the correct
            // bookkeeping and costs one map per rip, not because anything
            // measured needed it. The run that first looked like its work --
            // `full_adder` stopping at 2 rip-ups of 64 with `(14, 1, 160)`
            // still blamed and no branch admitting to it -- has a different
            // cause: that cell is a placed gate's `socket-approach`, which is
            // not a wire and cannot be ripped at all.
            let mut owned: BTreeMap<u64, Vec<Anchor>> = BTreeMap::new();
            for claim in &log {
                match *claim {
                    Claim::Body(node) => {
                        let Some(primitive) = self.nodes[node].as_ref() else {
                            continue;
                        };
                        let owner = format!("primitive:{node}");
                        for &cell in primitive.occupied() {
                            fresh.insert(cell, &owner, primitive.occupancy_of(cell));
                        }
                    }
                    Claim::Approaches(gate) => {
                        for (input, driver) in
                            self.netlist.gates[gate].inputs.iter().enumerate()
                        {
                            let (_, approach) = socket_and_approach(
                                self.anchors[gate],
                                self.facings[gate],
                                input,
                            );
                            fresh.insert(approach, driver, Occupancy::Wire);
                        }
                    }
                    Claim::Branch(serial) => {
                        let Some(branch) =
                            self.laid.iter().find(|laid| laid.serial == serial)
                        else {
                            continue;
                        };
                        let before = claimed_cells(&fresh);
                        reserve_path(&mut fresh, &branch.signal, &branch.path);
                        let guard = format!(
                            "terminal:{}.in[{}]",
                            self.netlist.gates[branch.consumer].output, branch.input
                        );
                        for neighbour in horizontal_neighbours(branch.socket) {
                            if neighbour != branch.predecessor
                                && neighbour != self.anchors[branch.consumer]
                            {
                                fresh.insert(neighbour, &guard, Occupancy::Solid);
                            }
                        }
                        owned.insert(
                            serial,
                            claimed_cells(&fresh).difference(&before).copied().collect(),
                        );
                    }
                }
            }
            for laid in &mut self.laid {
                if let Some(cells) = owned.remove(&laid.serial) {
                    laid.claimed = cells;
                }
            }
            self.reservation = fresh;
        }

        /// Tear out the youngest wire standing in a wedged gate's way, and
        /// every younger branch of that same net hanging off it.
        ///
        /// **Youngest, and not most-blamed.** A blame count says which cell is
        /// most in the way; age says which cell had the least right to be
        /// there. The wedge measured here is a gate's output pin closed in by
        /// nets routed *after* it was placed, so age is the axis that names the
        /// culprit -- and it is also the only axis that cannot cycle, since
        /// what is torn out is re-laid younger than everything that displaced
        /// it.
        ///
        /// Returns the `(gate, input)` pairs left without a feed.
        fn rip_youngest(
            &mut self,
            blame: &BTreeSet<Anchor>,
            wedged: &str,
        ) -> BTreeSet<(usize, usize)> {
            let Some(victim) = self
                .laid
                .iter()
                .filter(|laid| laid.claimed.iter().any(|cell| blame.contains(cell)))
                .map(|laid| laid.serial)
                .max()
            else {
                return BTreeSet::new();
            };

            // A younger branch of the same net may have grown off a cell this
            // one laid. Ripping the trunk and leaving the twig would leave a
            // route rooted in nothing, so the twigs come out too, transitively.
            let signal = self
                .laid
                .iter()
                .find(|laid| laid.serial == victim)
                .expect("the victim was just chosen from this list")
                .signal
                .clone();
            let mut doomed: BTreeSet<u64> = if self.settings.rip_whole {
                self.laid
                    .iter()
                    .filter(|laid| laid.signal == signal)
                    .map(|laid| laid.serial)
                    .collect()
            } else {
                BTreeSet::from([victim])
            };
            loop {
                let held: BTreeSet<Anchor> = self
                    .laid
                    .iter()
                    .filter(|laid| doomed.contains(&laid.serial))
                    .flat_map(|laid| laid.added.iter().copied())
                    .collect();
                let more: BTreeSet<u64> = self
                    .laid
                    .iter()
                    .filter(|laid| {
                        laid.signal == signal
                            && !doomed.contains(&laid.serial)
                            && held.contains(&laid.root)
                    })
                    .map(|laid| laid.serial)
                    .collect();
                if more.is_empty() {
                    break;
                }
                doomed.extend(more);
            }

            let torn: Vec<Laid> = self
                .laid
                .iter()
                .filter(|laid| doomed.contains(&laid.serial))
                .cloned()
                .collect();
            self.laid.retain(|laid| !doomed.contains(&laid.serial));
            self.log
                .retain(|claim| !matches!(claim, Claim::Branch(serial) if doomed.contains(serial)));

            let gone: BTreeSet<Anchor> =
                torn.iter().flat_map(|laid| laid.added.iter().copied()).collect();
            if let Some(tree) = self.trees.get_mut(&signal) {
                let mut anchors = Vec::with_capacity(tree.anchors.len());
                let mut realisation = Vec::with_capacity(tree.anchors.len());
                let mut floors = Vec::with_capacity(tree.anchors.len());
                for index in 0..tree.anchors.len() {
                    if gone.contains(&tree.anchors[index]) {
                        continue;
                    }
                    anchors.push(tree.anchors[index]);
                    realisation.push(tree.realisation[index].clone());
                    floors.push(tree.floors[index].clone());
                }
                tree.anchors = anchors;
                tree.realisation = realisation;
                tree.floors = floors;
                for cell in &gone {
                    tree.strength.remove(cell);
                    tree.repeaters.remove(cell);
                }
                for laid in &torn {
                    let sink = self.netlist.gates[laid.consumer].output.clone();
                    tree.terminals.retain(|terminal| {
                        !(terminal.sink.gate == sink && terminal.sink.input_index == laid.input)
                    });
                }
            }

            self.rip_log.push(format!(
                "{wedged}: ripped {} branch(es) of net {signal} ({} cells) -- {}",
                torn.len(),
                gone.len(),
                torn.iter()
                    .map(|laid| format!(
                        "{}.in[{}]",
                        self.netlist.gates[laid.consumer].output, laid.input
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
            self.reseat();
            torn.into_iter().map(|laid| (laid.consumer, laid.input)).collect()
        }

        /// Put every torn-out branch back, ripping again for whatever refuses
        /// it, until the budget runs out.
        fn settle_orphans(&mut self) {
            while let Some(&(consumer, input)) = self.orphans.iter().next() {
                match self.relay(consumer, input) {
                    Ok(()) => {
                        self.orphans.remove(&(consumer, input));
                        self.relaid += 1;
                        self.rip_log.push(format!(
                            "re-laid {}.in[{input}]",
                            self.netlist.gates[consumer].output
                        ));
                    }
                    Err(blame) => {
                        if self.ripped >= self.settings.rip || blame.is_empty() {
                            return;
                        }
                        let name =
                            format!("{}.in[{input}]", self.netlist.gates[consumer].output);
                        let torn = self.rip_youngest(&blame, &name);
                        if torn.is_empty() {
                            return;
                        }
                        self.ripped += 1;
                        self.orphans.extend(torn);
                    }
                }
            }
        }

        /// Re-grow one branch into a socket whose gate is already standing.
        ///
        /// The error is the blame set, so a failed re-lay feeds the same
        /// rip-up the wedge does rather than a second, differently-informed one.
        fn relay(&mut self, consumer: usize, input: usize) -> Result<(), BTreeSet<Anchor>> {
            let signal = self.netlist.gates[consumer].inputs[input].clone();
            let pin = self.pins[&signal];
            let anchor = self.anchors[consumer];
            let facing = self.facings[consumer];
            let (socket, approach) = socket_and_approach(anchor, facing, input);
            let mut tree = self.trees.get(&signal).cloned().unwrap_or_default();
            let seeds: Vec<Anchor> =
                tree.seeds(pin).into_iter().chain([approach, socket]).collect();

            for &margin in &self.settings.windows {
                let window = growth_window(&seeds, margin);
                let mut reservation = self.reservation.clone();
                let mut attempt = tree.clone();
                if let Ok(mut branch) = self.branch(
                    consumer,
                    anchor,
                    input,
                    &signal,
                    pin,
                    socket,
                    approach,
                    &mut attempt,
                    &mut reservation,
                    window,
                    None,
                ) {
                    branch.serial = self.serial;
                    self.serial += 1;
                    self.log.push(Claim::Branch(branch.serial));
                    self.laid.push(branch);
                    self.reservation = reservation;
                    tree = attempt;
                    self.trees.insert(signal, tree);
                    return Ok(());
                }
            }
            Err(refusal_heat(pin, approach, &signal, &self.reservation).into_keys().collect())
        }

        /// Land one gate: flood its input nets, rank the places its sockets
        /// could go, and lay the first one that survives being laid.
        /// `Box`ed for the same reason `route_in_order` boxes its own
        /// [`RoutingFailure`]: the error is the rare arm and much the larger,
        /// and every landing pays for it otherwise.
        fn land(&mut self, gate: usize) -> Result<(), Box<Wedge>> {
            let definition = self.netlist.gates[gate].clone();
            let drivers = definition.inputs.clone();
            let pins: Vec<Anchor> = drivers.iter().map(|signal| self.pins[signal]).collect();
            let trees: Vec<NetTree> = drivers
                .iter()
                .map(|signal| self.trees.get(signal).cloned().unwrap_or_default())
                .collect();
            let seeds: Vec<BTreeSet<Anchor>> = trees
                .iter()
                .zip(&pins)
                .map(|(tree, &pin)| tree.seeds(pin))
                .collect();
            let all: Vec<Anchor> = seeds.iter().flatten().copied().collect();
            let windows = self.settings.windows.clone();
            let tries = self.settings.tries;
            let verbose = self.settings.verbose;

            let mut funnel = Funnel::default();
            let mut fields_seen: Vec<(String, usize)> = Vec::new();
            let mut seals: Vec<Seal> = Vec::new();
            let mut refusals: Vec<String> = Vec::new();
            let mut blame: BTreeSet<Anchor> = BTreeSet::new();

            for &margin in &windows {
                let window = growth_window(&all, margin);
                let fields: Vec<Flood> = (0..drivers.len())
                    .map(|input| {
                        flood_from(
                            &seeds[input],
                            pins[input],
                            None,
                            &drivers[input],
                            &self.reservation,
                            window,
                        )
                    })
                    .collect();
                fields_seen = drivers
                    .iter()
                    .zip(&fields)
                    .map(|(signal, field)| (signal.clone(), field.travelled.len()))
                    .collect();
                // A field that adds nothing to the seeds it started from is a
                // net that cannot leave its producer -- a different failure
                // from "the fields could not meet", and the one worth naming.
                seals = (0..drivers.len())
                    .filter(|&input| fields[input].travelled.len() == seeds[input].len())
                    .map(|input| {
                        let mut blamed: Vec<(Anchor, String, u64)> = refusal_heat(
                            pins[input],
                            pins[input],
                            &drivers[input],
                            &self.reservation,
                        )
                        .into_iter()
                        .map(|(cell, blame)| {
                            let owner = self
                                .reservation
                                .owner(&cell)
                                .unwrap_or("unclaimed")
                                .to_string();
                            (cell, format!("{owner} [{}]", self.what_stands_at(cell)), blame)
                        })
                        .collect();
                        blamed.sort_by(|left, right| {
                            right.2.cmp(&left.2).then(left.0.cmp(&right.0))
                        });
                        blamed.truncate(6);
                        Seal { signal: drivers[input].clone(), pin: pins[input], blamed }
                    })
                    .collect();
                // A sealed net's blame is the whole ring around its pin, not
                // the six printed: rip-up needs every cell that could be torn
                // out, and the truncation above is for a human reader.
                for input in 0..drivers.len() {
                    if fields[input].travelled.len() != seeds[input].len() {
                        continue;
                    }
                    blame.extend(
                        refusal_heat(pins[input], pins[input], &drivers[input], &self.reservation)
                            .into_keys(),
                    );
                }

                let (ranked, this) = self.landings(gate, &fields, &drivers, &pins, &mut blame);
                funnel = this;

                for landing in ranked.iter().take(tries) {
                    let mut reservation = self.reservation.clone();
                    let mut branch = trees.clone();
                    match self.lay(
                        gate,
                        landing,
                        &drivers,
                        &pins,
                        &mut branch,
                        &mut reservation,
                        window,
                        None,
                    ) {
                        Ok((node, laid)) => {
                            if verbose {
                                eprintln!(
                                    "    {} <- [{}] at ({}, {}, {}) facing {} | arrival {} \
                                     | pull {:.1} | score {:.1} | window {margin} | fields {}",
                                    definition.output,
                                    drivers.join(" "),
                                    landing.anchor.x,
                                    landing.anchor.y,
                                    landing.anchor.z,
                                    landing.facing.index(),
                                    landing
                                        .arrival
                                        .iter()
                                        .map(u64::to_string)
                                        .collect::<Vec<_>>()
                                        .join("+"),
                                    landing.pull,
                                    landing.score,
                                    fields_seen
                                        .iter()
                                        .map(|(_, size)| size.to_string())
                                        .collect::<Vec<_>>()
                                        .join("/"),
                                );
                            }
                            self.commit(
                                gate,
                                landing.anchor,
                                landing.facing,
                                node,
                                laid,
                                &drivers,
                                branch,
                                reservation,
                            );
                            return Ok(());
                        }
                        Err(refusal) => {
                            // A landing that ranked but could not be laid
                            // blames whatever the branch search was refused by,
                            // measured the same way the seal report measures a
                            // pin's ring.
                            let (_, approach) = socket_and_approach(
                                landing.anchor,
                                landing.facing,
                                refusal.input,
                            );
                            blame.extend(
                                refusal_heat(
                                    pins[refusal.input],
                                    approach,
                                    &drivers[refusal.input],
                                    &self.reservation,
                                )
                                .into_keys(),
                            );
                            refusals.push(format!(
                                "({}, {}, {}) f{}: {} at input {}",
                                landing.anchor.x,
                                landing.anchor.y,
                                landing.anchor.z,
                                landing.facing.index(),
                                refusal.why,
                                refusal.input
                            ));
                        }
                    }
                }
            }

            Err(Box::new(Wedge {
                gate: definition.output.clone(),
                depth: self.depths[gate],
                arity: drivers.len(),
                windows,
                funnel,
                fields: fields_seen,
                seals,
                refusals,
                blame,
            }))
        }

        /// Record a landing that has already been laid: the reservation it
        /// produced, the trees it grew, the body, and the chronological claim
        /// log the rip-up rebuilds from.
        ///
        /// Lifted out of [`land`](Self::land)'s success arm unchanged, so that a
        /// landing the windowed solver chose is written into the growth state by
        /// exactly the code a grown one is. A second copy of this bookkeeping is
        /// a second claim log to be wrong about, and `reseat` replays it.
        #[allow(clippy::too_many_arguments)]
        fn commit(
            &mut self,
            gate: usize,
            anchor: Anchor,
            facing: geometry::CellFacing,
            node: PrimitiveNode,
            laid: Vec<Laid>,
            drivers: &[String],
            trees: Vec<NetTree>,
            reservation: Reservation,
        ) {
            let output = self.netlist.gates[gate].output.clone();
            self.reservation = reservation;
            for (signal, tree) in drivers.iter().zip(trees) {
                self.trees.insert(signal.clone(), tree);
            }
            self.anchors[gate] = anchor;
            self.facings[gate] = facing;
            self.pins
                .insert(output, node.output_pin.expect("a gate node records its pin"));
            self.nodes[gate] = Some(node);
            // Chronological, and in exactly the order `lay` wrote them: body,
            // then the four socket pre-claims, then the branches cheapest-first.
            self.log.push(Claim::Body(gate));
            self.log.push(Claim::Approaches(gate));
            for mut branch in laid {
                branch.serial = self.serial;
                self.serial += 1;
                self.log.push(Claim::Branch(branch.serial));
                self.laid.push(branch);
            }
        }

        /// Land a gate at a position and along routes somebody else chose.
        ///
        /// The decode half of the windowed solver: everything from here down is
        /// [`lay`](Self::lay), so a solved plan is realised by the same repeater
        /// budget, the same terminal choice and the same guard cells as a grown
        /// one -- and refuses on the same grounds. **A solver answer that this
        /// refuses is an encoding bug in the model, not a result**, which is why
        /// the refusal is returned rather than swallowed.
        fn land_solved(
            &mut self,
            gate: usize,
            anchor: Anchor,
            facing: geometry::CellFacing,
            routes: &BTreeMap<usize, Vec<Anchor>>,
            window: (Anchor, Anchor),
        ) -> Result<(), String> {
            let drivers = self.netlist.gates[gate].inputs.clone();
            let pins: Vec<Anchor> = drivers.iter().map(|signal| self.pins[signal]).collect();
            let mut trees: Vec<NetTree> = drivers
                .iter()
                .map(|signal| self.trees.get(signal).cloned().unwrap_or_default())
                .collect();
            let arrival: Vec<u64> = (0..drivers.len())
                .map(|input| routes.get(&input).map_or(0, |path| path.len() as u64))
                .collect();
            let landing = Landing { anchor, facing, arrival, pull: 0.0, score: 0.0 };
            let mut reservation = self.reservation.clone();
            match self.lay(
                gate,
                &landing,
                &drivers,
                &pins,
                &mut trees,
                &mut reservation,
                window,
                Some(routes),
            ) {
                Ok((node, laid)) => {
                    self.commit(gate, anchor, facing, node, laid, &drivers, trees, reservation);
                    self.placed[gate] = true;
                    Ok(())
                }
                Err(refusal) => Err(format!("input {}: {}", refusal.input, refusal.why)),
            }
        }

        /// Every place this gate's sockets could meet its input nets, ranked.
        ///
        /// The enumeration inverts the socket geometry rather than scanning a
        /// box: for each facing, every cell of input 0's field is a candidate
        /// approach, and a candidate approach names exactly one anchor -- so the
        /// offered set is "wherever a route already is", stated literally, and
        /// no position is ever considered that no wire can reach.
        fn landings(
            &self,
            gate: usize,
            fields: &[Flood],
            drivers: &[String],
            pins: &[Anchor],
            blame: &mut BTreeSet<Anchor>,
        ) -> (Vec<Landing>, Funnel) {
            let definition = &self.netlist.gates[gate];
            let arity = drivers.len();
            let mut funnel = Funnel::default();
            let mut landings: Vec<Landing> = Vec::new();
            // Generate from the *smallest* field. The offered set is the same
            // whichever input generates it -- a landing has to satisfy every
            // input, so the enumeration is a filter over an intersection and the
            // order of the intersection does not change it -- but the work is
            // not: a gate with one sealed input and one wide-open one is
            // enumerated over the sealed one's handful of cells instead of the
            // open one's thousands. That is also the honest denominator for the
            // wedge funnel, which is the most-constrained net's count.
            let Some(generator) = (0..arity).min_by_key(|&input| {
                (fields[input].travelled.len(), input)
            }) else {
                return (landings, funnel);
            };

            // Where the gates that will read this one's output already have
            // their *other* drivers. The only forward-looking term there is: a
            // gate placed at the argmin of arrival cost alone walks away from
            // everything that has yet to consume it.
            let pull_to = self.consumer_barycentre(gate);

            for index in 0..4u8 {
                let facing = geometry::CellFacing::from_index(index).expect("0..4 is horizontal");
                let (body, conductors, pin_offset) =
                    compile::gate_footprint((0, 0, 0), definition, facing);
                let unit =
                    step(GROWTH_ORIGIN, geometry::input_directions(facing)[generator]);

                for (&cell, &cost) in &fields[generator].travelled {
                    // A gate stands on the gate plane. Growth chooses where in
                    // the plane, never which plane: `starting_layout` lays one
                    // storey and every route's floor is written one below it.
                    if cell.y != PLANNER_Y {
                        continue;
                    }
                    let anchor = Anchor {
                        x: cell.x - 2 * unit.x,
                        y: cell.y - 2 * unit.y,
                        z: cell.z - 2 * unit.z,
                    };
                    funnel.offered += 1;

                    // Three is the hardware maximum fan-in -- see
                    // `geometry::input_directions`, whose fourth face is the
                    // output's -- so no landing ever needs a fourth slot.
                    let mut arrival = [0u64; 3];
                    let mut sockets = [GROWTH_ORIGIN; 3];
                    let mut approaches = [GROWTH_ORIGIN; 3];
                    let mut met = true;
                    for input in 0..arity {
                        let (socket, approach) = socket_and_approach(anchor, facing, input);
                        let reached = if input == generator {
                            Some(cost)
                        } else {
                            fields[input].cost(approach)
                        };
                        match reached {
                            Some(cost) => arrival[input] = cost,
                            None => {
                                met = false;
                                break;
                            }
                        }
                        sockets[input] = socket;
                        approaches[input] = approach;
                    }
                    if !met {
                        continue;
                    }
                    funnel.approaches_met += 1;

                    let fit = BodyFit {
                        origin: anchor,
                        cells: &body,
                        conductors: &conductors,
                        sockets: &sockets[..arity],
                        approaches: &approaches[..arity],
                        drivers,
                        pins,
                        pin: shifted(anchor, pin_offset),
                        escape: self.settings.escape,
                    };
                    if !fit.allowed(&self.reservation) {
                        // Capped, because a gate whose fields are wide open
                        // offers tens of thousands of landings and rip-up needs
                        // a target list, not a census. Every landing that got
                        // this far already met all its approaches, so the cells
                        // named here are the ones a body could not stand on.
                        const BLAME_CAP: usize = 4096;
                        if blame.len() < BLAME_CAP {
                            fit.blockers(&self.reservation, blame);
                        }
                        continue;
                    }
                    funnel.body_fits += 1;

                    let pull = match pull_to {
                        Some((x, z)) => {
                            (f64::from(anchor.x) - x).abs() + (f64::from(anchor.z) - z).abs()
                        }
                        None => 0.0,
                    };
                    let summed: u64 = arrival[..arity].iter().sum();
                    landings.push(Landing {
                        anchor,
                        facing,
                        arrival: arrival[..arity].to_vec(),
                        pull,
                        score: summed as f64 + self.settings.lambda * pull,
                    });
                }
            }

            // Argmin, ties by (y, z, x, facing) -- a total order over the `f64`,
            // so two runs of the same netlist rank identically rather than
            // nearly.
            landings.sort_by(|left, right| {
                left.score
                    .total_cmp(&right.score)
                    .then(left.anchor.y.cmp(&right.anchor.y))
                    .then(left.anchor.z.cmp(&right.anchor.z))
                    .then(left.anchor.x.cmp(&right.anchor.x))
                    .then(left.facing.index().cmp(&right.facing.index()))
            });
            (landings, funnel)
        }

        /// What kind of thing is standing on a cell, which is the difference
        /// between a wedge rip-up can address and one it cannot.
        ///
        /// The owner string alone does not say: a cell owned by net `g4` is a
        /// wire if some branch laid it and a **socket approach** if a placed
        /// gate reserved it, and only the first can be torn out. Without this
        /// the central claim of the rip-up measurement -- that what remains in
        /// `segment_a`'s way is geometry and not congestion -- would be a
        /// deduction from the source rather than something the run printed.
        fn what_stands_at(&self, cell: Anchor) -> &'static str {
            if self
                .nodes
                .iter()
                .flatten()
                .any(|body| body.occupied().contains(&cell))
            {
                return "body";
            }
            for gate in 0..self.netlist.gates.len() {
                if !self.placed[gate] {
                    continue;
                }
                for input in 0..self.netlist.gates[gate].inputs.len() {
                    let (socket, approach) =
                        socket_and_approach(self.anchors[gate], self.facings[gate], input);
                    if cell == approach {
                        return "socket-approach";
                    }
                    if cell == socket {
                        return "socket";
                    }
                }
            }
            if self.laid.iter().any(|laid| laid.claimed.contains(&cell)) {
                return "wire";
            }
            "guard-or-floor"
        }

        /// The mean pin of the drivers, already placed, of everything that will
        /// consume this gate's output.
        fn consumer_barycentre(&self, gate: usize) -> Option<(f64, f64)> {
            let signal = &self.netlist.gates[gate].output;
            let mut sum = (0.0, 0.0);
            let mut count = 0usize;
            for &(consumer, _) in self.sinks.get(signal).map(Vec::as_slice).unwrap_or(&[]) {
                for driver in &self.netlist.gates[consumer].inputs {
                    if driver == signal {
                        continue;
                    }
                    let Some(&pin) = self.pins.get(driver) else {
                        continue;
                    };
                    sum = (sum.0 + f64::from(pin.x), sum.1 + f64::from(pin.z));
                    count += 1;
                }
            }
            (count > 0).then(|| (sum.0 / count as f64, sum.1 / count as f64))
        }

        /// Put the gate down and grow each of its input nets into it, cheapest
        /// branch first.
        ///
        /// Everything from `reserve_path` onwards is `route_in_order`'s body,
        /// because a branch laid any other way would be a second strength model
        /// to be wrong about: the same repeater plan, the same `carries` refusal
        /// for a run that decays, the same terminal style, the same guard cells
        /// around the socket, the same block written back over the socket when
        /// the style says repeater.
        ///
        /// `supplied` hands a route in rather than searching for one, keyed by
        /// input index. That is how the windowed solver's answer is decoded: the
        /// solver decides *which cells*, and everything that turns cells into
        /// blocks -- the repeater plan, the terminal style, the guard cells --
        /// stays this one function, so a solved plan and a grown plan are
        /// realised by the same code. `None` is growth, unchanged.
        #[allow(clippy::too_many_arguments)]
        fn lay(
            &self,
            gate: usize,
            landing: &Landing,
            drivers: &[String],
            pins: &[Anchor],
            trees: &mut [NetTree],
            reservation: &mut Reservation,
            window: (Anchor, Anchor),
            supplied: Option<&BTreeMap<usize, Vec<Anchor>>>,
        ) -> Result<(PrimitiveNode, Vec<Laid>), LayRefusal> {
            let definition = &self.netlist.gates[gate];
            let (footprint, conductors, output_pin) = compile::gate_footprint(
                (landing.anchor.x, landing.anchor.y, landing.anchor.z),
                definition,
                landing.facing,
            );
            let node = PrimitiveNode {
                id: format!("gate:{}", definition.output),
                anchor: landing.anchor,
                realisation: if definition.is_merge() {
                    NodeRealisation::WireMerge
                } else {
                    NodeRealisation::Primitive(Primitive::Torch)
                },
                footprint,
                conductors,
                pinned: false,
                output_pin: Some(output_pin),
            };
            let owner = format!("primitive:{gate}");
            for &cell in node.occupied() {
                reservation.insert(cell, &owner, node.occupancy_of(cell));
            }

            // The socket pre-claim `route_in_order` makes for every gate before
            // it routes anything, made here for this gate alone -- because here
            // the gate did not exist a moment ago. It is what stops one of this
            // gate's own input nets taking another's only way in.
            let mut sockets = Vec::with_capacity(drivers.len());
            let mut approaches = Vec::with_capacity(drivers.len());
            for (input, driver) in drivers.iter().enumerate() {
                let (socket, approach) = socket_and_approach(landing.anchor, landing.facing, input);
                reservation.insert(approach, driver, Occupancy::Wire);
                sockets.push(socket);
                approaches.push(approach);
            }

            let mut cheapest: Vec<usize> = (0..drivers.len()).collect();
            cheapest.sort_by_key(|&input| (landing.arrival[input], input));

            let mut laid: Vec<Laid> = Vec::with_capacity(drivers.len());
            for input in cheapest {
                laid.push(self.branch(
                    gate,
                    landing.anchor,
                    input,
                    &drivers[input],
                    pins[input],
                    sockets[input],
                    approaches[input],
                    &mut trees[input],
                    reservation,
                    window,
                    supplied.and_then(|routes| routes.get(&input)).map(Vec::as_slice),
                )?);
            }

            Ok((node, laid))
        }

        /// Grow one net into one socket, and record what that cost the world.
        ///
        /// Lifted out of [`lay`](Self::lay) unchanged so that rip-up can put a
        /// branch back by the same route it was laid by. A second, separately
        /// written re-router would be a second strength model and a second
        /// terminal model to be wrong about -- which is the reason this probe
        /// reaches through the shipping functions in the first place.
        #[allow(clippy::too_many_arguments)]
        fn branch(
            &self,
            gate: usize,
            anchor: Anchor,
            input: usize,
            signal: &str,
            pin: Anchor,
            socket: Anchor,
            approach: Anchor,
            tree: &mut NetTree,
            reservation: &mut Reservation,
            window: (Anchor, Anchor),
            supplied: Option<&[Anchor]>,
        ) -> Result<Laid, LayRefusal> {
            let definition = &self.netlist.gates[gate];
            let before = claimed_cells(reservation);
            let mut added: Vec<Anchor> = Vec::new();

            let mut path = match supplied {
                // A solved route arrives as the walk it is, root first, ending
                // at the approach. Everything after this line is identical to
                // the grown case on purpose: the strength budget, the terminal
                // choice and the guard cells are the judges either way.
                Some(cells) => {
                    if cells.first().is_none_or(|root| !tree.seeds(pin).contains(root)) {
                        return Err(LayRefusal { input, why: String::from("the supplied route starts nowhere this net has laid") });
                    }
                    if cells.last() != Some(&approach) {
                        return Err(LayRefusal { input, why: String::from("the supplied route does not end at the approach") });
                    }
                    // The flood's own legality, replayed step by step over a
                    // route the flood did not find. Every arm below is
                    // `flood_from`'s, in its order, against the same reservation
                    // it would have read -- so a supplied route is judged by the
                    // shipping rules rather than trusted. What this catches is
                    // exactly an encoding gap in whatever produced the route,
                    // and it names the cell and the rule rather than the fact.
                    let mut walked: BTreeMap<Anchor, Anchor> = BTreeMap::new();
                    let refuse = |at: Anchor, why: &str| LayRefusal {
                        input,
                        why: format!("supplied route: ({}, {}, {}) {why}", at.x, at.y, at.z),
                    };
                    for pair in cells.windows(2) {
                        let (at, next) = (pair[0], pair[1]);
                        if !neighbours(at).contains(&next) {
                            return Err(refuse(next, "is not a step dust can take"));
                        }
                        if self_obstructs(&walked, at, next) {
                            return Err(refuse(next, "breaks a step this route already took"));
                        }
                        if !within_bounds(next, window.0, window.1) {
                            return Err(refuse(next, "is outside the window"));
                        }
                        if !anchor_is_free_for(next, pin, approach, socket, signal, reservation) {
                            return Err(refuse(next, "is refused by anchor_is_free_for"));
                        }
                        if staircase_clearance(at, next).into_iter().any(|cell| {
                            let foreign = reservation.owner(&cell).is_some_and(|occupied_by| {
                                occupied_by != signal && occupied_by != stair_guard(signal)
                            });
                            let is_riser = next.y > at.y && cell.y == at.y;
                            if is_riser {
                                return foreign || reservation.conductor_owner(&cell).is_some();
                            }
                            reservation.owner(&cell).is_some()
                        }) {
                            return Err(refuse(next, "has no staircase clearance"));
                        }
                        walked.insert(next, at);
                    }
                    cells.to_vec()
                }
                None => {
                    let field = flood_from(
                        &tree.seeds(pin),
                        pin,
                        Some((approach, socket)),
                        signal,
                        reservation,
                        window,
                    );
                    match field.path_to(approach) {
                        Some(path) => path,
                        None => {
                            return Err(LayRefusal {
                                input,
                                why: String::from("no branch reaches the approach"),
                            })
                        }
                    }
                }
            };
            path.push(socket);
            reserve_path(reservation, signal, &path);

            // Where this branch hangs off, and what the signal is worth when
            // it gets there. The first branch of a net is the one case
            // `route_in_order` also has: it starts at the producer's pin, at
            // full strength, with the pin itself the first cell of dust.
            let root = path[0];
            let fresh = tree.anchors.is_empty();
            let (previous_cell, incoming, cells) = if fresh {
                (pin, crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH, &path[..])
            } else {
                let carried = *tree
                    .strength
                    .get(&root)
                    .expect("a branch grows off a cell this net laid");
                (root, carried, &path[1..])
            };
            let trunk_repeaters = if fresh {
                0
            } else {
                *tree.repeaters.get(&root).unwrap_or(&0)
            };

            let run = realise_branch_from(previous_cell, incoming, cells);
            if !run.carries {
                return Err(LayRefusal {
                    input,
                    why: String::from("the branch decays before it arrives"),
                });
            }
            let budget_needs_repeater = run.blocks.last().is_some_and(|block| {
                block.kind == crate::redstone::world::block::BlockKind::Repeater
            });
            let strength_before_terminal = run.strength_before_terminal;
            let branch_repeaters = run.repeaters;

            let mut carried = incoming;
            let mut counted = trunk_repeaters;
            for ((cell, block), floor) in cells.iter().zip(run.blocks).zip(run.floors) {
                if block.kind == crate::redstone::world::block::BlockKind::Repeater {
                    carried = crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
                    counted += 1;
                } else {
                    carried = carried.saturating_sub(1);
                }
                if tree.anchors.contains(cell) {
                    continue;
                }
                tree.anchors.push(*cell);
                tree.realisation.push(block);
                tree.floors.push(floor);
                tree.strength.insert(*cell, carried);
                tree.repeaters.insert(*cell, counted);
                added.push(*cell);
            }

            let predecessor = path.get(path.len().saturating_sub(2)).copied().unwrap_or(pin);

            let consumers = self.sinks.get(signal).map(Vec::as_slice).unwrap_or(&[]);
            let bare_merge =
                definition.is_merge() && consumers.iter().all(|&(sink, _)| sink == gate);

            let kind = if bare_merge {
                RouteTerminalKind::BareMergeDust
            } else {
                let style = if budget_needs_repeater {
                    TerminalStyle::RepeaterIntoSupport
                } else {
                    terminal_style(&TerminalApproach::new(
                        predecessor,
                        socket,
                        anchor,
                        strength_before_terminal,
                        terminal_is_isolated(reservation, signal, predecessor, socket, anchor),
                    ))
                };
                if let Some(index) = tree.anchors.iter().position(|cell| *cell == socket) {
                    tree.realisation[index] = match style {
                        TerminalStyle::RepeaterIntoSupport => {
                            compile::repeater(compile::direction_from(
                                Position::new(predecessor.x, predecessor.y, predecessor.z),
                                Position::new(socket.x, socket.y, socket.z),
                            ))
                        }
                        TerminalStyle::DirectedDustIntoSupport => compile::dust(),
                    };
                }
                style.into()
            };

            let guard = format!("terminal:{}.in[{input}]", definition.output);
            for neighbour in horizontal_neighbours(socket) {
                if neighbour != predecessor && neighbour != anchor {
                    reservation.insert(neighbour, &guard, Occupancy::Solid);
                }
            }

            tree.terminals.push(RouteTerminal {
                sink: RouteSink {
                    gate: definition.output.clone(),
                    input_index: input,
                    anchor: socket,
                },
                kind,
                repeaters: trunk_repeaters + branch_repeaters,
            });

            let claimed: Vec<Anchor> =
                claimed_cells(reservation).difference(&before).copied().collect();
            Ok(Laid {
                serial: 0,
                signal: signal.to_string(),
                consumer: gate,
                input,
                socket,
                predecessor,
                root,
                path,
                added,
                claimed,
            })
        }

        /// The candidate this growth built, or `None` if anything is unplaced.
        fn candidate(&self) -> Option<PlanCandidate> {
            if self.nodes.iter().any(Option::is_none) {
                return None;
            }
            let nodes: Vec<PrimitiveNode> = self.nodes.iter().flatten().cloned().collect();
            // `BTreeMap` order is `net_sinks`' order, which is the order
            // `route_in_order` pushes its routes in.
            let routes: Vec<Route> = self
                .trees
                .iter()
                .filter(|(_, tree)| !tree.anchors.is_empty())
                .map(|(signal, tree)| {
                    let mut route = Route::new(signal.clone(), Vec::new());
                    route.owner = Some(signal.clone());
                    route.anchors = tree.anchors.clone();
                    route.realisation = tree.realisation.clone();
                    route.floors = tree.floors.clone();
                    route.terminals = tree.terminals.clone();
                    route
                })
                .collect();
            Some(PlanCandidate::with_facings(
                self.anchors.clone(),
                nodes,
                routes,
                self.facings.clone(),
            ))
        }
    }

    /// Every ordered transition between input states, timed from a settled
    /// state, with every output read at each one.
    ///
    /// `a_self_placed_and4_computes_and4`'s loop over an arbitrary circuit's
    /// input list. All three of its properties are kept and each matters: a
    /// sweep visits `2^n` of the `2^n (2^n - 1)` transitions, chaining them on
    /// one simulator reports a number the circuit does not have, and timing from
    /// an unsettled state charges the previous transition to this one.
    ///
    /// **The panel's shared blind spot was that nobody measured delay.** A
    /// layout that routes, verifies, and settles in 300 ticks is not a win over
    /// one that does not route, and no measurement on this branch would have
    /// said so.
    fn worst_settle_and_truth(
        realised: &RealisedCandidate,
        inputs: &[&str],
        outputs: &[String],
        expected: fn(&[bool]) -> Vec<bool>,
    ) -> Result<(u64, String, usize), String> {
        use crate::redstone::simulator::Simulator;

        let width = inputs.len();
        let mut levers = Vec::with_capacity(width);
        for name in inputs {
            match realised.ports.input_positions.get(*name) {
                Some(at) => levers.push(*at),
                None => return Err(format!("no lever for input `{name}`")),
            }
        }
        let mut sinks = Vec::with_capacity(outputs.len());
        for signal in outputs {
            match realised.ports.output_positions.get(signal) {
                Some(at) => sinks.push(*at),
                None => return Err(format!("no output `{signal}`")),
            }
        }

        // Most-significant bit first, which is what `INPUT_NAMES` means
        // everywhere else in this module and what lets `decoder_digit` read a
        // BCD digit off the vector.
        let bits_of = |mask: usize| -> Vec<bool> {
            (0..width)
                .map(|index| (mask >> (width - 1 - index)) & 1 == 1)
                .collect()
        };
        let states = 1usize << width;
        let mut worst = 0u64;
        // Zero-padded to the input width, because `{:b}` alone prints the
        // transition `2 -> 13` as `10 -> 1101`, and a reader has no way to tell
        // that first number from a four-bit `1010`.
        let mut worst_at = String::new();
        let mut seen = 0usize;

        for from in 0..states {
            let before = bits_of(from);
            for to in 0..states {
                if from == to {
                    continue;
                }
                let after = bits_of(to);
                let mut simulator = Simulator::new(realised.world.clone());
                for (at, &bit) in levers.iter().zip(before.iter()) {
                    let mut state = simulator.world().get(at.0, at.1, at.2).clone();
                    state.lit = bit;
                    simulator.world_mut().set(at.0, at.1, at.2, state);
                }
                simulator
                    .run_until_stable(2000)
                    .map_err(|error| format!("did not settle at {before:?}: {error:?}"))?;
                for (at, &bit) in levers.iter().zip(after.iter()) {
                    let mut state = simulator.world().get(at.0, at.1, at.2).clone();
                    state.lit = bit;
                    simulator.world_mut().set(at.0, at.1, at.2, state);
                }
                let ticks = simulator
                    .run_until_stable(2000)
                    .map_err(|error| format!("did not settle at {after:?}: {error:?}"))?;
                if ticks > worst {
                    worst = ticks;
                    worst_at = format!("{from:0width$b} -> {to:0width$b}");
                }

                let want = expected(&after);
                for (index, at) in sinks.iter().enumerate() {
                    let got = simulator.world().get(at.0, at.1, at.2).lit;
                    if got != want[index] {
                        return Err(format!(
                            "{after:?} -> `{}` expected {}, got {got}, reached from {before:?}",
                            outputs[index], want[index]
                        ));
                    }
                }
                seen += 1;
            }
        }

        Ok((worst, worst_at, seen))
    }

    /// The shipping placer and router on the same circuit, reported in the
    /// same units.
    ///
    /// Growth's `236 blocks / 18 ticks` means nothing without the number it is
    /// being compared to, and until this existed the only such number in the
    /// tree was and4's, in a different test, printed on a different day.
    /// `a_self_placed_full_adder_computes_a_full_adder` proves `full_adder`
    /// places, routes and computes through `plan_from_netlist`, and prints
    /// neither its size nor its delay.
    fn shipping_baseline(case: &ConditionCircuit) {
        use std::time::Instant;

        let started = Instant::now();
        let candidate = match plan_from_netlist(&case.netlist, &PortPlacements::default()) {
            Ok(candidate) => candidate,
            Err(error) => {
                eprintln!("  {} SHIPPING: does not plan: {error}", case.name);
                return;
            }
        };
        let (width, depth, area) = anchor_box(&candidate.anchors);
        let realised =
            match realise_and_verify(&candidate, &case.netlist, candidate_world_size(&candidate)) {
                Ok(realised) => realised,
                Err(error) => {
                    eprintln!("  {} SHIPPING: plans, DOES NOT VERIFY: {error}", case.name);
                    return;
                }
            };
        let blocks = (0..realised.world.cells().len())
            .filter(|&flat| {
                let (x, y, z) = realised.world.decode(flat);
                realised.world.get(x, y, z).kind
                    != crate::redstone::world::block::BlockKind::Air
            })
            .count();
        eprintln!(
            "  {} SHIPPING: plans and verifies in {:.1}s | anchor box {width}x{depth}={area}              | {blocks} blocks",
            case.name,
            started.elapsed().as_secs_f64()
        );
        match worst_settle_and_truth(&realised, case.inputs, &case.outputs, case.expected) {
            Ok((worst, at, transitions)) => eprintln!(
                "  {} SHIPPING: truth table Ok over {transitions} ordered transitions,                  worst settle {worst} game ticks at {at}",
                case.name
            ),
            Err(error) => eprintln!("  {} SHIPPING: TRUTH TABLE WRONG: {error}", case.name),
        }
    }

    /// Grow one circuit and report every number the paradigm has to be judged
    /// on: where each gate landed, how big the result is, what wedged, whether
    /// it verifies, and -- only if it does -- what it costs in game ticks.
    fn grow_and_report(case: &ConditionCircuit, settings: &GrowthSettings) {
        use std::time::Instant;

        eprintln!(
            "== growth {}: {} gates, {} inputs | order {} | lambda {} | windows {:?} \
             | tries {} | escape {} | rip {}{} | levers {} ==",
            case.name,
            case.netlist.gates.len(),
            case.netlist.inputs.len(),
            if settings.seed == 0 {
                settings.order.clone()
            } else {
                format!("seed {}", settings.seed)
            },
            settings.lambda,
            settings.windows,
            settings.tries,
            settings.escape,
            settings.rip,
            if settings.rip_whole { " whole-net" } else { "" },
            if settings.seed_pitch > 0 {
                format!("pitch {}", settings.seed_pitch)
            } else {
                "starting_layout".to_string()
            },
        );

        let started = Instant::now();
        let mut growth = match Growth::seeded(&case.netlist, settings) {
            Ok(growth) => growth,
            Err(error) => {
                eprintln!("  {}: SEED FAILED: {error}", case.name);
                return;
            }
        };
        growth.grow();
        let grew = started.elapsed().as_secs_f64();

        let gates = case.netlist.gates.len();
        let placed = growth.placed.iter().filter(|done| **done).count();
        let standing: Vec<Anchor> =
            growth.nodes.iter().flatten().map(|node| node.anchor).collect();
        let (width, depth, area) = anchor_box(&standing);
        let wire: usize = growth.trees.values().map(|tree| tree.anchors.len()).sum();

        if let Some(wedge) = &growth.wedge {
            eprintln!(
                "  WEDGE at {} (depth {}, {} input(s)) after {placed}/{gates} gates, {grew:.1}s",
                wedge.gate, wedge.depth, wedge.arity
            );
            eprintln!(
                "    windows {:?} | offered {} -> approaches met {} -> body fits {}",
                wedge.windows,
                wedge.funnel.offered,
                wedge.funnel.approaches_met,
                wedge.funnel.body_fits
            );
            eprintln!(
                "    fields: {}",
                wedge
                    .fields
                    .iter()
                    .map(|(signal, cells)| format!("{signal} {cells} cells"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for seal in &wedge.seals {
                eprintln!(
                    "    SEALED: net {} cannot leave its pin ({}, {}, {}); blamed: {}",
                    seal.signal,
                    seal.pin.x,
                    seal.pin.y,
                    seal.pin.z,
                    seal.blamed
                        .iter()
                        .map(|(cell, owner, blame)| format!(
                            "({}, {}, {}) {owner} x{blame}",
                            cell.x, cell.y, cell.z
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            for refusal in wedge.refusals.iter().take(6) {
                eprintln!("    refused: {refusal}");
            }
            for line in &growth.rip_log {
                eprintln!("    rip: {line}");
            }
            eprintln!(
                "  {}: WEDGED at {placed}/{gates} gates -- anchor box \
                 {width}x{depth}={area}, {wire} cells of wire, {} rip-up(s) of {} spent,                  {} re-laid",
                case.name, growth.ripped, settings.rip, growth.relaid
            );
            return;
        }

        if !growth.orphans.is_empty() {
            eprintln!(
                "  {}: RIP-UP LOST {} branch(es) it could not put back after {} rip-up(s) \
                 of {}: {}",
                case.name,
                growth.orphans.len(),
                growth.ripped,
                settings.rip,
                growth
                    .orphans
                    .iter()
                    .map(|&(consumer, input)| format!(
                        "{}.in[{input}]",
                        growth.netlist.gates[consumer].output
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            return;
        }

        if placed < gates {
            eprintln!(
                "  {}: STARVED at {placed}/{gates} gates with no wedge -- {} gate(s) never \
                 became ready, so some input is driven by nothing",
                case.name,
                gates - placed
            );
            return;
        }

        let Some(candidate) = growth.candidate() else {
            eprintln!("  {}: grew every gate and built no candidate", case.name);
            return;
        };
        let cost = candidate.cost();
        eprintln!(
            "  {}: grew {placed}/{gates} gates in {grew:.1}s | anchor box \
             {width}x{depth}={area} | {wire} cells of wire | cost wire {} delay {} turns {} \
             | {} rip-up(s) of {}, {} branch(es) re-laid",
            case.name,
            cost.wire,
            cost.delay,
            cost.turns,
            growth.ripped,
            settings.rip,
            growth.relaid
        );

        for line in &growth.rip_log {
            eprintln!("    rip: {line}");
        }

        let started = Instant::now();
        let realised =
            match realise_and_verify(&candidate, &case.netlist, candidate_world_size(&candidate)) {
                Ok(realised) => realised,
                Err(error) => {
                    eprintln!(
                        "  {}: GREW, DOES NOT VERIFY after {:.1}s: {error}",
                        case.name,
                        started.elapsed().as_secs_f64()
                    );
                    return;
                }
            };
        let blocks = (0..realised.world.cells().len())
            .filter(|&flat| {
                let (x, y, z) = realised.world.decode(flat);
                realised.world.get(x, y, z).kind
                    != crate::redstone::world::block::BlockKind::Air
            })
            .count();
        eprintln!(
            "  {}: VERIFIES in {:.1}s, {blocks} blocks",
            case.name,
            started.elapsed().as_secs_f64()
        );

        if !settings.settle {
            eprintln!("  {}: truth table and settle NOT MEASURED (REDA_GROWTH_SETTLE=0)", case.name);
            return;
        }
        let started = Instant::now();
        match worst_settle_and_truth(&realised, case.inputs, &case.outputs, case.expected) {
            Ok((worst, at, transitions)) => eprintln!(
                "  {}: truth table Ok over {transitions} ordered transitions, worst settle \
                 {worst} game ticks at {at}, {:.1}s",
                case.name,
                started.elapsed().as_secs_f64()
            ),
            Err(error) => eprintln!("  {}: TRUTH TABLE WRONG: {error}", case.name),
        }
    }

    /// **The growth probe.** Does wire-first constructive compilation place and
    /// route what the shipping placer plus the shipping router cannot?
    ///
    /// The paradigm, in the operator's framing: wires are ~45% of the blocks and
    /// gates 3%, so wires should be the primary state and gate positions
    /// derived. Nets grow as real dust trees through the world in topological
    /// order; a gate becomes placeable when its drivers are placed; its input
    /// nets flood outward through free space under the router's own legality
    /// rules; and the gate lands at the argmin of summed arrival cost. The
    /// measured failure this is meant to delete is `segment_a`'s `g32`, walled
    /// into a 256-cell pocket that **no net ordering escapes** -- because the
    /// goal is fixed. Here it is an output.
    ///
    /// # What this is not
    ///
    /// A measurement harness, not a router. It asserts nothing -- a probe that
    /// gates something is a probe somebody tunes until it goes green -- and it
    /// has **no wedge escape**: the first gate with no landing stops the
    /// circuit and the wedge is reported. Wedge frequency is a result.
    ///
    /// Three things it keeps from the shipping path rather than re-deriving,
    /// because a probe measuring a different physics measures nothing: step
    /// legality ([`anchor_is_free_for`], [`keep_out`], [`staircase_clearance`],
    /// [`self_obstructs`], [`neighbours`]), the strength budget
    /// ([`realise_branch_from`] -- repeaters every <= 15 cells and a refresh
    /// before every climb), and the terminal machinery ([`terminal_style`],
    /// [`terminal_is_isolated`], the guard cells).
    ///
    /// **Routes without verifies is worth nothing.** The congestion probe
    /// routed `segment_a` at anchor box 11,342 and failed `verify_candidate`
    /// with a torch-merge violation. So this one runs `realise_and_verify` on
    /// whatever it builds and, for anything that survives that, the truth table
    /// and the worst settle over every ordered transition -- the number nobody
    /// on the panel measured.
    ///
    /// # Re-running it, and sweeping it
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::measure_whether_growth_places_and_routes \
    ///   -- --ignored --nocapture
    /// ```
    ///
    /// | variable | default | what it is |
    /// |---|---|---|
    /// | `REDA_GROWTH_CIRCUIT` | `and4` | `and4`, `full_adder`, `segment_a`, `seven_segment`, or `all` |
    /// | `REDA_GROWTH_ORDER` | `depth` | ready-queue policy: `depth` (then widest gate), `arity`, `index` |
    /// | `REDA_GROWTH_LAMBDA` | `0.5` | weight on the pull toward future consumers' placed drivers |
    /// | `REDA_GROWTH_WINDOWS` | `8,16,32,64` | plan margins tried around the input nets, in order |
    /// | `REDA_GROWTH_TRIES` | `8` | landings attempted per window before the next one |
    /// | `REDA_GROWTH_ESCAPE` | `0` | legal first steps a landing must leave its own output pin; `0` is the sketch |
    /// | `REDA_GROWTH_SEED_PITCH` | `0` | lever column pitch; `0` is `starting_layout`'s own row |
    /// | `REDA_GROWTH_VERBOSE` | `1` | print a line per gate landed |
    /// | `REDA_GROWTH_SETTLE` | `1` | truth table and settle sweep on anything that verifies |
    /// | `REDA_GROWTH_RIP` | `0` | rip-up budget; `0` is v1, the paradigm with no rip-up at all |
    /// | `REDA_GROWTH_RIP_WHOLE` | `0` | rip the victim's whole net, not the youngest branch and its twigs |
    /// | `REDA_GROWTH_SEED` | `0` | deterministic growth-order seed; replaces `ORDER` when non-zero |
    /// | `REDA_GROWTH_BASELINE` | `0` | also run `plan_from_netlist` on the same circuit, same units |
    ///
    /// # What it measured
    ///
    /// `REDA_GROWTH_CIRCUIT=all`, defaults otherwise, `--release`, whole run
    /// 7.3s. Every number below is one line of that run's output.
    ///
    /// | circuit | gates grown | anchor box | wire cells | verify | truth table | worst settle |
    /// |---|---|---|---|---|---|---|
    /// | and4 | **7/7** | 61x10 = 610 | 106 | **Ok**, 236 blocks | **Ok**, 240 transitions | 18 ticks, `0010 -> 1101` |
    /// | full_adder | 7/22 | 41x10 = 410 | 86 | WEDGE `g9` | — | — |
    /// | segment_a | 18/46 | 63x19 = 1,197 | 347 | WEDGE `g8` | — | — |
    /// | seven_segment | 18/84 | 63x19 = 1,197 | 347 | WEDGE `g8` | — | — |
    ///
    /// **and4 is a real end-to-end result**: grown, verified, and right on all
    /// 240 ordered transitions. Read the box with care, though -- 61 of its 610
    /// is `starting_layout`'s lever row, which growth never moves and
    /// relaxation does, so it is not comparable to relaxation's 45x23 = 1,035
    /// cell for cell. The comparable numbers are the ones that do not depend on
    /// where the levers went: **236 blocks against relaxation's 232 and
    /// legacy's 572**, and **18 game ticks against relaxation's 14 and legacy's
    /// 26** (`a_self_placed_and4_computes_and4`, same loop, same day). Growth
    /// is level with relaxation on size and a quarter slower.
    ///
    /// # The one failure mode, and it is not the one the paradigm predicts
    ///
    /// **Every wedge is a sealed source pin, and none is "the fields could not
    /// meet".** The funnel says so in each case: `offered 4 -> approaches met 4
    /// -> body fits 0`, over a field of **one** cell. A one-input gate's only
    /// driver could not leave its own output pin, so the four offered landings
    /// are the four facings of a socket sitting directly on that pin, and all
    /// four collide with the gate that owns it. `segment_a`'s is `g7` at
    /// `(26, 1, 146)`, walled by `g4`'s dust at `(24, 1, 146)`, `g5`'s at
    /// `(25, 3, 145)`, `g2`'s at `(26, 1, 144)` and `primitive:5`'s body at
    /// `(28, 1, 146)`.
    ///
    /// The mechanism is the exact dual of the ring the framing deletes. "Wires
    /// are laid before the gate exists, so nothing needs room saved for it" is
    /// true of a gate's **inputs** and false of its **output**: a gate's output
    /// net does not exist when the gate lands, so nothing keeps the gates placed
    /// afterwards off it, and they close around it.
    ///
    /// **Measured, not argued.** Nine `(order, lambda)` combinations --
    /// `{depth, arity, index}` x `{0, 0.5, 2}` -- all wedge on `full_adder`,
    /// and seven of the nine on the same gate `g9` over the same sealed pin:
    /// `depth` reaches 7/22, `arity` 8/22, `index` 9/22. The two exceptions are
    /// `depth` and `arity` at `lambda = 2`, which stop *earlier*, at 6/22 on the
    /// two-input `g5`.
    /// `REDA_GROWTH_ESCAPE`, which refuses a landing that does not leave its own
    /// pin `N` legal first steps, was added to test the obvious repair and
    /// **does not work**: at 1, 2 and 3 `full_adder` still wedges at `g9` on
    /// `g8`'s pin, in the same cell, because `g8` had four escapes when it
    /// landed and lost them to gates placed later. The knob is live rather than
    /// inert -- at 3 the plan changes (87 wire cells against 86, a different
    /// blamed owner at `(16, 1, 162)`) -- it just cannot reach the cause.
    /// Nothing short of reserving a gate's exit *before* its consumers are
    /// placed addresses this, and that is the `reservation(d)` ring again.
    ///
    /// # What this harness's own numbers do not cover
    ///
    /// Falsification, by injection, reverted (2026-08-16, `--release`, and4):
    ///
    /// - **The verify arm is live.** Moving one cell of every route one step in
    ///   x: `GREW, DOES NOT VERIFY: cannot realise node g3: cell (29, 1, 62) is
    ///   listed twice by this route`.
    /// - **The truth-table arm is live**, and catches what the invariants pass:
    ///   replacing one dust cell of the first route with stone *after*
    ///   `realise_and_verify` returned still prints `VERIFIES in 0.0s, 236
    ///   blocks`, then `TRUTH TABLE WRONG: [true, true, true, true] -> g6
    ///   expected true, got false`.
    /// - **and4 does not exercise three of the rules this harness adds.**
    ///   Disabling [`BodyFit`]'s `keep_out` arm, disabling its `is_taken` arm,
    ///   and forcing every terminal to claim directed dust each left and4's plan
    ///   *bit for bit identical* -- same box, same 106 wire cells, same 236
    ///   blocks, same 18 ticks. The argmin landing never sat on any of them. So
    ///   and4 verifying is evidence that the pipeline is wired, and **not**
    ///   evidence that those three rules are right; the circuit that would test
    ///   them is one that grows past a wedge.
    ///
    /// # The pre-registered kill criteria, run (2026-08-16, `--release`)
    ///
    /// Three criteria were written down before any of this was measured, each
    /// of which kills the paradigm on its own. **Two fired.** What follows is
    /// the run that decided each, and every number is a line one of them
    /// printed.
    ///
    /// ## The parity floor, and the baseline it is measured against
    ///
    /// `REDA_GROWTH_BASELINE=1` runs `plan_from_netlist` on the same netlist
    /// through the same `realise_and_verify` and the same settle sweep, so the
    /// comparison is one run rather than two days.
    /// `a_self_placed_full_adder_computes_a_full_adder` has always proved
    /// `full_adder` places, routes and computes on the shipping path; it has
    /// never printed its size or its delay.
    ///
    /// | circuit | shipping box | shipping blocks | shipping ticks | growth, default order |
    /// |---|---|---|---|---|
    /// | and4 | 45x23 = 1,035 | 232 | 14 | **7/7, verifies, 236 blocks, 18 ticks** |
    /// | full_adder | 33x105 = 3,465 | 1,065 | 46 | **WEDGE `g9` at 7/22** |
    /// | segment_a | does not plan | — | — | **WEDGE `g8` at 18/46** |
    /// | seven_segment | does not plan | — | — | **WEDGE `g8` at 18/84** |
    ///
    /// The same run confirms the premise it is being judged against, in one
    /// place instead of two: `segment_a SHIPPING: does not plan: no safe local
    /// route from (99, 1, 97) to (122, 1, 89)` and `seven_segment ... from
    /// (83, 1, 106) to (83, 1, 96)` -- the two addresses the ledger has always
    /// quoted. and4's 232 blocks and 14 ticks match the relaxation numbers this
    /// harness was already comparing itself to.
    ///
    /// **and4 holds, and holds robustly.** 40 of 40 growth-order seeds grow
    /// 7/7, verify, and are right on all 240 ordered transitions, at **18 game
    /// ticks in every one of the 40** and 236 blocks in 35 of them, 252 in the
    /// other 5.
    ///
    /// **full_adder does not.** The default order wedges at 7/22. Over seeds
    /// 1..120 with rip-up off, **7 of 120 complete and 2 of those verify** --
    /// seed 3 at 570 blocks / 42 ticks, seed 69 at 582 blocks / 28 ticks. The
    /// other five complete and fail `verify_candidate`: three torch-merge
    /// violations (a net reaching a gate's support block), two signal-strength
    /// violations. **Those are growth's, not the rip-up's** -- rip-up was off
    /// in every one of the 120.
    ///
    /// ## (1) HARD WEDGING -- **fires**
    ///
    /// ```text
    /// segment_a, default order, rip 0:    WEDGE g8 at 18/46, all four windows,
    ///                                     offered 4 -> approaches met 4 -> body fits 0
    /// segment_a, default order, rip 64:   WEDGE g8 at 18/46, 5 rip-ups spent,
    ///                                     17 branches torn out, 347 -> 143 wire cells
    /// segment_a, default order, rip 1000: WEDGE g8 at 18/46, 5 rip-ups spent
    /// segment_a, whole-net rip 64:        WEDGE g8 at 18/46, 3 rip-ups spent,
    ///                                     19 branches torn out, 347 -> 139 wire cells
    /// ```
    ///
    /// The budget is not what binds -- a thousand buys exactly what 64 does,
    /// because rip-up runs out of *rippable wire*, not out of tries. What is
    /// left standing on `g7`'s pin is measured rather than deduced, by
    /// [`Growth::what_stands_at`], which separates the two things an owner
    /// string alone cannot:
    ///
    /// ```text
    /// rip 0:  (26,1,147) primitive:7 [body]   (24,1,146) g4 [socket-approach]
    ///         (25,3,145) g5 [wire]            (26,1,144) g2 [socket-approach]
    ///         (26,1,148) primitive:7 [body]   (28,1,146) primitive:5 [body]
    /// rip 64: (26,1,147) primitive:7 [body]   (24,1,146) g4 [socket-approach]
    ///         (26,1,144) g2 [socket-approach] (26,1,148) primitive:7 [body]
    ///         (28,1,146) primitive:5 [body]   (26,2,147) primitive:7 [body]
    /// ```
    ///
    /// Rip-up tore out the **one** wire in the ring and the pin is still
    /// sealed. Everything else is a gate body or the input socket of a gate
    /// already standing -- geometry fixed by a placement decision, which no
    /// amount of wire rip-up can move. `full_adder`'s wedge is the same shape:
    /// five bodies and one `socket-approach`, 2 rip-ups of 64 spent.
    ///
    /// So the escape hatch the criterion allows was built, it fires, it works
    /// -- 64 of 64 rip-ups and 126 re-lays on some seeds -- and **it does not
    /// address this failure**, because this failure is not congestion.
    ///
    /// The obvious pairing is refuted too. `REDA_GROWTH_ESCAPE=3` reserves a
    /// gate's way out *at landing*, which is the half rip-up cannot supply, and
    /// `REDA_GROWTH_ESCAPE=3 REDA_GROWTH_RIP=64` together still wedge at `g8`
    /// after 18/46, 5 rip-ups spent, on the same pin with the same six
    /// blockers. `seven_segment` wedges identically at `g8` after 18/84 -- same
    /// cell `(26, 1, 146)`, same taxonomy, 5 rip-ups -- which is what it should
    /// do, since the two circuits share `segment_a`'s first eighteen gates.
    ///
    /// ## (2) DOMINATED -- **not decidable as written, and not the reason**
    ///
    /// The criterion asks whether `segment_a` completes only above legacy's
    /// 23,220. It never completes, at any setting tried, so it has no area.
    /// Where growth *does* verify it is not dominated at all: and4 is 236
    /// blocks against 232 (+1.7%) at 18 ticks against 14, and full_adder's two
    /// verified seeds are 570 and 582 blocks against the shipping path's
    /// **1,065** (-46%) at 42 and 28 ticks against 46. Growth's output quality
    /// is not the problem. Its reliability is.
    ///
    /// ## (3) ORDER FRAGILITY -- **fires**
    ///
    /// Twenty deterministic growth-order seeds (`REDA_GROWTH_SEED=1..20`,
    /// [`scrambled`] permuting the ready queue outright, which is the
    /// growth-side twin of the 120 net-order shuffles):
    ///
    /// | | completes | best reached | spread |
    /// |---|---|---|---|
    /// | `segment_a`, rip 0 | **0 of 20** | 25/46 | 9/46 .. 25/46 |
    /// | `segment_a`, rip 64 | **0 of 20** | 37/46 | 13/46 .. 37/46 |
    ///
    /// The rip-up run is the generous one and it is where the rip-up gets its
    /// hardest workout on this branch: **1,056 rip-ups and 1,150 branch
    /// re-lays across the twenty seeds**, 16 of the 20 exhausting the whole
    /// budget. It buys reach -- the best seed goes from 25/46 to 37/46 -- and
    /// it buys **no completions**.
    ///
    /// The nine named `(order, lambda)` combinations the earlier sweep used on
    /// `full_adder` say the same thing about `segment_a`, and none of them is a
    /// seed: `{depth, arity, index}` x `{0, 0.5, 2}` all wedge, at 18, 18, 19,
    /// 18, 18, 8, 11, 15 and 19 of 46. **Twenty-nine orderings, no
    /// completion.**
    ///
    /// That is the shuffle experiment's shape, not an area/delay spread over
    /// completions. `segment_a` has no completion to spread over.
    ///
    /// ## The blind spot, filled
    ///
    /// Worst settle over every ordered transition from a settled state on a
    /// fresh simulator, for everything that verifies: and4 **18** ticks
    /// (shipping 14, legacy 26), full_adder **42** ticks at seed 3 and **28**
    /// at seed 69 (shipping 46). Nothing else growth produced ever verified,
    /// so nothing else has a tick count.
    ///
    /// # Rule 2 on the rip-up itself
    ///
    /// The rip-up is new code, and "it still wedges" is worthless coming from
    /// machinery that cannot succeed. What it does, measured:
    ///
    /// - **It fires and re-lays, at volume.** `full_adder` seed 6 spends all 64
    ///   rip-ups and re-lays 122 branches; seed 4, 62; seed 8, 63. The
    ///   `segment_a` seed sweep alone spends 1,056 rip-ups and re-lays 1,150
    ///   branches.
    /// - **It changes outcomes.** `full_adder` completes 22/22 at seeds 12, 18
    ///   and 24 with rip-up and wedges at all three without it.
    /// - **And every one of those three then fails `verify_candidate`** -- two
    ///   signal-strength, one torch-merge. Under rule 6 that is worth nothing,
    ///   and it is reported rather than rounded off.
    /// - **The failures are not the rip-up's.** Two hypotheses, one experiment
    ///   each. A re-laid branch reading a stale carried strength off a trunk:
    ///   refuted -- `REDA_GROWTH_RIP_WHOLE=1` re-grows the whole net from its
    ///   pin at full strength, and seeds 12, 18 and 24 fail *identically*, same
    ///   gate, same message. The rip-up introducing a class of violation growth
    ///   does not have: refuted -- with rip-up **off**, 5 of the 7 `full_adder`
    ///   completions over seeds 1..120 fail with the same two classes.
    /// - **One claim about it was wrong and is corrected rather than deleted.**
    ///   [`Growth::reseat`]'s recompute of each branch's claimed cells was
    ///   first written up as the fix for `full_adder` stopping at 2 rip-ups of
    ///   64. Disabling it leaves seeds 4, 6, 8, 12 and 24 bit for bit
    ///   identical, so it is **measured inert**; the real cause of that stop is
    ///   the taxonomy above.
    ///
    /// # What is still NOT MEASURED
    ///
    /// - `verilog:and4` and `verilog:seven_segment` -- this harness takes the
    ///   four hand-written circuits, and the projection deadlock that stops
    ///   `verilog:seven_segment` is upstream of anything here.
    /// - **Why** growth's torch-merge and signal-strength violations happen.
    ///   An instrument for it was written and **thrown away rather than
    ///   believed**: it looked for a foreign conductor within [`keep_out`] of a
    ///   placed gate's support block and found *none* on any of the five
    ///   failing completions, which does not mean there is none -- it means it
    ///   was asking the wrong question. Reading `compile::net_reach` says why:
    ///   the relation that fires is `dust_powers_block_toward`, a directional
    ///   rule about a dust cell's connection *shape*, and that file's own
    ///   comment says "nothing else in the compiler models that adjacency --
    ///   `dust_reach` and `verify_connectivity` are both strictly
    ///   dust-reaches-dust". So the step legality this probe reaches through
    ///   verbatim cannot see the thing that fails. That is a reading of the
    ///   source, not a measurement, and is labelled as one.
    /// - `seven_segment` past its wedge, at any setting.
    /// - Rip-up above 64 on anything but `segment_a`'s default order.
    ///
    /// # The companion harness
    ///
    /// Every failure above is reported as "no landing" or "no safe local route",
    /// which says only *I did not find one*.
    /// [`calibrate_the_windowed_solver`] is the other half: a complete decision
    /// procedure over a bounded box around one gate, calibrated against the
    /// answers this probe already produced. It is **calibration only** -- it
    /// runs on `and4`, the one circuit whose growth completes, verifies and
    /// computes correctly, and it deliberately does not touch the wedges above.
    #[test]
    #[ignore = "measurement harness: asserts nothing, grows a circuit gate by gate, minutes on the larger ones"]
    fn measure_whether_growth_places_and_routes() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist, SEGMENT_NAMES,
        };

        let setting = |name: &str, fallback: &str| -> String {
            std::env::var(name).unwrap_or_else(|_| fallback.to_string())
        };

        let settings = GrowthSettings {
            order: setting("REDA_GROWTH_ORDER", "depth"),
            lambda: setting("REDA_GROWTH_LAMBDA", "0.5").parse().expect("a weight"),
            windows: setting("REDA_GROWTH_WINDOWS", "8,16,32,64")
                .split(',')
                .map(|piece| piece.trim().parse().expect("a margin"))
                .collect(),
            tries: setting("REDA_GROWTH_TRIES", "8").parse().expect("a count"),
            escape: setting("REDA_GROWTH_ESCAPE", "0").parse().expect("a count"),
            seed_pitch: setting("REDA_GROWTH_SEED_PITCH", "0").parse().expect("a pitch"),
            verbose: setting("REDA_GROWTH_VERBOSE", "1") != "0",
            settle: setting("REDA_GROWTH_SETTLE", "1") != "0",
            rip: setting("REDA_GROWTH_RIP", "0").parse().expect("a budget"),
            seed: setting("REDA_GROWTH_SEED", "0").parse().expect("a seed"),
            rip_whole: setting("REDA_GROWTH_RIP_WHOLE", "0") != "0",
        };

        let (and4, and4_output) = build_and4_netlist();
        let (adder, adder_outputs) = build_full_adder_netlist();
        let (segment_a, segment_a_output) = build_single_segment_netlist(0);
        let (decoder, decoder_outputs) = build_seven_segment_netlist();

        let cases = [
            ConditionCircuit {
                name: "and4",
                netlist: and4,
                inputs: &crate::circuits::and4::INPUT_NAMES[..],
                outputs: vec![and4_output],
                expected: and4_expected,
            },
            ConditionCircuit {
                name: "full_adder",
                netlist: adder,
                inputs: &crate::circuits::full_adder::INPUT_NAMES[..],
                outputs: vec![adder_outputs["sum"].clone(), adder_outputs["cout"].clone()],
                expected: full_adder_expected,
            },
            ConditionCircuit {
                name: "segment_a",
                netlist: segment_a,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: vec![segment_a_output],
                expected: segment_a_expected,
            },
            ConditionCircuit {
                name: "seven_segment",
                netlist: decoder,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: SEGMENT_NAMES.iter().map(|name| decoder_outputs[name].clone()).collect(),
                expected: seven_segment_expected,
            },
        ];

        let wanted = setting("REDA_GROWTH_CIRCUIT", "and4");
        let chosen: Vec<&ConditionCircuit> = cases
            .iter()
            .filter(|case| wanted == "all" || wanted == case.name)
            .collect();
        assert!(!chosen.is_empty(), "REDA_GROWTH_CIRCUIT names no circuit");

        let baseline = setting("REDA_GROWTH_BASELINE", "0") != "0";
        for case in chosen {
            if baseline {
                shipping_baseline(case);
            }
            grow_and_report(case, &settings);
        }
    }

    // =====================================================================
    // A windowed constraint model over growth's own state, and the
    // calibration that makes its answers worth believing.
    // =====================================================================

    use crate::compile::satcnf::{self, Cnf, Lit, Outcome};

    /// The shipping step relation, restricted to the gate plane.
    ///
    /// Derived rather than restated: [`neighbours`] is `dust_reach`'s twelve
    /// cells, and this is a filter over it, so a change there is a change here.
    fn plane_neighbours(cell: Anchor) -> Vec<Anchor> {
        neighbours(cell).into_iter().filter(|next| next.y == cell.y).collect()
    }

    /// Whether a cell is reachable by a net's own wire within `d` steps.
    ///
    /// Three answers rather than a literal, because two of them are constants:
    /// a cell the net already has is reachable at zero cost and needs no
    /// variable, and a cell nearer than its own shortest possible approach is
    /// unreachable and needs none either. Both cases delete clauses rather than
    /// weaken them.
    #[derive(Debug, Clone, Copy)]
    enum Reach {
        Always,
        Never,
        At(Lit),
    }

    /// One input net of the gate being placed, as the model sees it.
    struct NetWindow {
        signal: String,
        pin: Anchor,
        /// Cells this net already holds that a new branch may grow from, on the
        /// gate plane. [`NetTree::seeds`] is the shipping definition; this is
        /// that set filtered to `PLANNER_Y`.
        seeds: BTreeSet<Anchor>,
        /// Seeds the plane restriction dropped. Non-zero means the model is
        /// answering a *narrower* question than the window admits, and the
        /// report says so rather than rounding it off.
        seeds_off_plane: usize,
        domain: Vec<Anchor>,
        index: BTreeMap<Anchor, usize>,
        used: Vec<Lit>,
        /// `reach[cell][d - first[cell]]`.
        reach: Vec<Vec<Lit>>,
        /// The shortest number of steps in which this net could possibly arrive
        /// at each domain cell, ignoring every constraint but the window's own
        /// free space. Below it the cell is provably unreachable, so no variable
        /// exists.
        first: Vec<usize>,
        depth: usize,
        /// Whether `depth` is the exact bound (`|domain|`, which no simple path
        /// can exceed) rather than a cap. **An UNSAT from a capped model is
        /// bounded by path length and is not a proof of infeasibility**, and the
        /// report never prints it as one.
        exact: bool,
    }

    impl NetWindow {
        /// Whether this net can reach no cell of the window at all, at any path
        /// length -- as opposed to none within the depth cap.
        ///
        /// The distinction decides what an UNSAT is worth. A capped model's
        /// UNSAT is bounded by path length; a net that cannot take a single
        /// legal first step out of what it already holds is stranded outright,
        /// and an UNSAT that turns on that fact does not depend on the cap.
        fn stranded_entirely(&self) -> bool {
            self.first.iter().all(|&steps| steps == usize::MAX)
        }

        fn reach_at(&self, cell: Anchor, steps: usize) -> Reach {
            if self.seeds.contains(&cell) {
                return Reach::Always;
            }
            let Some(&cell_index) = self.index.get(&cell) else {
                return Reach::Never;
            };
            if self.first[cell_index] > self.depth
                || steps < self.first[cell_index]
                || steps > self.depth
            {
                return Reach::Never;
            }
            Reach::At(self.reach[cell_index][steps - self.first[cell_index]])
        }
    }

    /// A candidate landing: where the gate would stand, and the literal that
    /// says it does.
    struct SolvedLanding {
        anchor: Anchor,
        facing: geometry::CellFacing,
        approaches: Vec<Anchor>,
        place: Lit,
    }

    /// The windowed model: the CNF, the objects its variables mean, and the
    /// numbers a report needs.
    struct WindowModel {
        nets: Vec<NetWindow>,
        landings: Vec<SolvedLanding>,
        cnf: Cnf,
        /// The connectivity ladder's group, kept so the calibration can take it
        /// out and watch the wire disappear.
        group_reach: satcnf::Group,
        /// Landings whose body could not stand against the fixed world at all,
        /// so they never became variables.
        body_rejected: usize,
        /// *Which* landings those were. Bookkeeping only -- pushed in the same
        /// arm that increments `body_rejected`, read by nothing that builds a
        /// clause -- and it exists because on a wedge the interesting landings
        /// are precisely the ones that never became variables and so can never
        /// appear in a core. See [`why_the_body_would_not_stand`].
        body_rejected_at: Vec<(Anchor, geometry::CellFacing)>,
        offered: usize,
        aliased_inputs: bool,
    }

    impl WindowModel {
        fn exact(&self) -> bool {
            self.nets.iter().all(|net| net.exact) && !self.aliased_inputs
        }

        /// Every group but the connectivity ladder.
        fn without_connectivity(&self) -> BTreeSet<satcnf::Group> {
            (0..self.cnf.group_count()).filter(|&group| group != self.group_reach).collect()
        }

        fn summary(&self) -> String {
            format!(
                "{} vars, {} clauses, {} groups | landings {} of {} offered ({} body-rejected) \
                 | {} | depth {}",
                self.cnf.vars(),
                self.cnf.clause_count(),
                self.cnf.group_count(),
                self.landings.len(),
                self.offered,
                self.body_rejected,
                self.nets
                    .iter()
                    .map(|net| format!("{} {} cells", net.signal, net.domain.len()))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.nets
                    .iter()
                    .map(|net| format!(
                        "{}{}",
                        net.depth,
                        if net.exact { "" } else { "*" }
                    ))
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        }
    }

    /// Build the windowed model for one gate, against one growth state.
    ///
    /// # What is modelled, and where each rule comes from
    ///
    /// Nothing below states a rule of the game twice. Every constraint is either
    /// a call into the shipping predicate or a filter over what one returned:
    ///
    /// | rule | shipping source |
    /// |---|---|
    /// | one owner per cell | [`anchor_is_free_for`]'s owner arm (1619) |
    /// | dust needs a floor, and the floor may be no conductor | its `below` arm (1631) |
    /// | two nets' conductors need clearance | its `keep_out` arm (1642), which is `dust_reach`'s conservative plan-time shape |
    /// | where a gate's blocks go | [`compile::gate_footprint`], realised into a scratch world |
    /// | a gate body may stand here | [`BodyFit::allowed`], which is the growth probe's own reading of the two above from the gate's side |
    /// | where a socket is and what may enter it | [`socket_and_approach`] |
    /// | which cells a step may go to | [`neighbours`] |
    /// | repeaters, decay, and what a run carries | **not modelled** -- [`realise_branch_from`] decides it at decode, and refuses |
    ///
    /// # The single-plane restriction, stated once
    ///
    /// New wire is given cells at [`PLANNER_Y`] only. That restricts the
    /// *answer*, not the world: everything already laid is read in full 3D, so a
    /// route at `y = 3` still keeps new dust out through [`keep_out`] and still
    /// forbids a floor over itself. What it costs is completeness -- a window
    /// whose only solution climbs comes back UNSAT. What it buys is that three
    /// rules are discharged **exactly** rather than approximated:
    ///
    /// * [`staircase_clearance`] returns nothing when `to.y == from.y`, so a
    ///   flat route needs no stair guard and the model omits none;
    /// * [`self_obstructs`] has two arms and each requires a step in `y`, so a
    ///   flat path cannot obstruct itself;
    /// * [`anchor_is_free_for`]'s floor arm asks about `y - 1`, which for a flat
    ///   answer is always a *fixed* cell, so it is discharged by the domain
    ///   filter and never becomes a clause.
    ///
    /// Those three are derivations from the shipping predicates, not
    /// assumptions, and
    /// `the_flat_restriction_discharges_exactly_the_three_rules_it_claims` runs
    /// them.
    ///
    /// # Connectivity
    ///
    /// The hard one, and the place SAT routing is normally won or lost. Each net
    /// gets a bounded-reachability ladder: `reach[c][d]` implies either
    /// `reach[c][d-1]` or (`c` is used **and** some neighbour is reachable at
    /// `d-1`), with the ladder's floor being "is a cell this net already has".
    /// Asserting `reach[approach][depth]` therefore forces a connected walk of
    /// used cells from the net's existing tree to the socket's approach. Only
    /// the downward implication is stated, which is why a spuriously-true
    /// `reach` cannot satisfy anything: it can only create an obligation.
    ///
    /// `depth` defaults to `|domain|`, which **no simple path can exceed**, so
    /// the ladder is not a bound at all unless it is capped on purpose. When it
    /// is capped, [`WindowModel::exact`] goes false and every report of an UNSAT
    /// says the answer is bounded by path length.
    fn window_model(
        growth: &Growth,
        gate: usize,
        window: (Anchor, Anchor),
        reach_cap: usize,
    ) -> WindowModel {
        let definition = growth.netlist.gates[gate].clone();
        let drivers = definition.inputs.clone();
        let arity = drivers.len();

        let mut cnf = Cnf::new();
        let group_place =
            cnf.group(format!("gate {} stands in exactly one place", definition.output));
        let group_body = cnf.group("a gate's own cells hold no wire");
        let group_body_clear =
            cnf.group("a gate's conductors keep foreign wire out (keep_out)");
        let group_one = cnf.group("one net per cell");
        let group_clear = cnf.group("two nets' conductors keep clear of each other (keep_out)");
        let group_reach = cnf.group("a net's wire is connected to what it has already laid");
        let group_goal = cnf.group("every socket's approach is reached by its own net");

        let mut aliased_inputs = false;
        for left in 0..arity {
            for right in left + 1..arity {
                if drivers[left] == drivers[right] {
                    aliased_inputs = true;
                }
            }
        }

        // ---- the per-net cell domains -----------------------------------
        let mut nets: Vec<NetWindow> = Vec::with_capacity(arity);
        for signal in &drivers {
            let pin = growth.pins[signal];
            let tree = growth.trees.get(signal).cloned().unwrap_or_default();
            let every_seed = tree.seeds(pin);
            let seeds: BTreeSet<Anchor> =
                every_seed.iter().copied().filter(|cell| cell.y == PLANNER_Y).collect();
            let seeds_off_plane = every_seed.len() - seeds.len();

            let mut domain: Vec<Anchor> = Vec::new();
            for x in window.0.x..=window.1.x {
                for z in window.0.z..=window.1.z {
                    let cell = Anchor { x, y: PLANNER_Y, z };
                    if seeds.contains(&cell) {
                        continue;
                    }
                    // `anchor_is_free_for`'s owner arm. A cell this net already
                    // owns is a seed and was taken above; anything else owned is
                    // out.
                    if growth.reservation.owner(&cell).is_some() {
                        continue;
                    }
                    // Its floor arm: laying a floor over a conductor deletes it,
                    // and that holds against this net's own conductors too.
                    let below = Anchor { y: cell.y - 1, ..cell };
                    if growth.reservation.conductor_owner(&below).is_some() {
                        continue;
                    }
                    // Its keep_out arm, with the one exemption the shipping
                    // search makes for a route leaving its own producer:
                    // `neighbour == start`.
                    let clear = keep_out(cell).into_iter().all(|neighbour| {
                        neighbour == pin
                            || growth
                                .reservation
                                .conductor_owner(&neighbour)
                                .is_none_or(|occupied_by| occupied_by == signal.as_str())
                    });
                    if !clear {
                        continue;
                    }
                    domain.push(cell);
                }
            }

            let index: BTreeMap<Anchor, usize> =
                domain.iter().enumerate().map(|(at, &cell)| (cell, at)).collect();

            // The shortest walk that could possibly reach each cell, through
            // free space alone. A cell nearer than this is provably unreachable,
            // which deletes variables rather than adding constraints.
            let mut first: Vec<usize> = vec![usize::MAX; domain.len()];
            let mut frontier: BTreeSet<Anchor> = seeds.clone();
            let mut steps = 0usize;
            while !frontier.is_empty() {
                steps += 1;
                let mut next: BTreeSet<Anchor> = BTreeSet::new();
                for cell in &frontier {
                    for neighbour in plane_neighbours(*cell) {
                        if let Some(&at) = index.get(&neighbour) {
                            if first[at] == usize::MAX {
                                first[at] = steps;
                                next.insert(neighbour);
                            }
                        }
                    }
                }
                frontier = next;
            }

            let exact_bound = domain.len();
            let depth = if reach_cap == 0 {
                exact_bound
            } else {
                reach_cap.min(exact_bound)
            };
            let exact = depth == exact_bound;

            // A cell no walk of `depth` steps could reach keeps its `use`
            // variable -- the model may still put wire there, uselessly -- and
            // gets no reach ladder, because every rung of one would be a
            // variable that can only ever be false. `first == usize::MAX` is
            // that case, and [`NetWindow::reach_at`] reads it as `Never`.
            //
            // **Deleting such a cell outright was tried and is worse.** With the
            // sealed-pin calibration it emptied the domain, which emptied the
            // landing set, which made the model unsatisfiable through an empty
            // at-least-one clause -- the right answer with an uninformative
            // core. Keeping the cells leaves the contradiction where it belongs:
            // between "the gate stands somewhere" and "its sockets are reached".
            let used: Vec<Lit> = domain.iter().map(|_| cnf.var()).collect();
            let reach: Vec<Vec<Lit>> = (0..domain.len())
                .map(|at| {
                    if first[at] > depth {
                        return Vec::new();
                    }
                    (first[at]..=depth).map(|_| cnf.var()).collect()
                })
                .collect();

            nets.push(NetWindow {
                signal: signal.clone(),
                pin,
                seeds,
                seeds_off_plane,
                domain,
                index,
                used,
                reach,
                first,
                depth,
                exact,
            });
        }

        // ---- connectivity ------------------------------------------------
        for net in &nets {
            for (at, &cell) in net.domain.iter().enumerate() {
                if net.first[at] > net.depth {
                    continue;
                }
                for steps in net.first[at]..=net.depth {
                    let here = net.reach[at][steps - net.first[at]];
                    let earlier = (steps > net.first[at])
                        .then(|| net.reach[at][steps - 1 - net.first[at]]);

                    // Reaching a cell means occupying it.
                    let mut arm = vec![-here, net.used[at]];
                    arm.extend(earlier);
                    cnf.add(arm, group_reach);

                    // ... and arriving from somewhere this net reached sooner.
                    let mut arm = vec![-here];
                    arm.extend(earlier);
                    let mut already_there = false;
                    for neighbour in plane_neighbours(cell) {
                        match net.reach_at(neighbour, steps - 1) {
                            Reach::Always => {
                                already_there = true;
                                break;
                            }
                            Reach::Never => {}
                            Reach::At(literal) => arm.push(literal),
                        }
                    }
                    if !already_there {
                        cnf.add(arm, group_reach);
                    }
                }
            }
        }

        // ---- wire against wire -------------------------------------------
        for left in 0..nets.len() {
            for right in left + 1..nets.len() {
                // Two inputs driven by one signal are one net, and a net is free
                // to run beside itself. Modelling them as strangers would forbid
                // legal layouts, so the pair is skipped and the model declares
                // itself inexact.
                if nets[left].signal == nets[right].signal {
                    continue;
                }
                for (at, &cell) in nets[left].domain.iter().enumerate() {
                    if let Some(&other) = nets[right].index.get(&cell) {
                        cnf.add(
                            [-nets[left].used[at], -nets[right].used[other]],
                            group_one,
                        );
                    }
                    for neighbour in keep_out(cell) {
                        if let Some(&other) = nets[right].index.get(&neighbour) {
                            cnf.add(
                                [-nets[left].used[at], -nets[right].used[other]],
                                group_clear,
                            );
                        }
                    }
                }
            }
        }

        // ---- where the gate may stand ------------------------------------
        //
        // Enumerated over the window grown by the two cells a socket's approach
        // stands out from its anchor, so that every landing whose approaches
        // fall inside the window is offered even when its own anchor does not.
        const APPROACH_REACH: i32 = 2;
        let mut landings: Vec<SolvedLanding> = Vec::new();
        let mut offered = 0usize;
        let mut body_rejected = 0usize;
        let mut body_rejected_at: Vec<(Anchor, geometry::CellFacing)> = Vec::new();
        let pins: Vec<Anchor> = nets.iter().map(|net| net.pin).collect();
        for index in 0..4u8 {
            let facing = geometry::CellFacing::from_index(index).expect("0..4 is horizontal");
            // Once per facing and translated, which is what [`BodyFit`]'s own
            // doc asks for and what its offsets permit: `gate_footprint`
            // realises the gate into a 64x8x64 scratch world and scans all
            // 32,768 cells of it, and a window offers thousands of candidate
            // anchors. Calling it per candidate was measured at minutes per
            // window before this line moved out of the loop.
            let (body_offsets, conductor_offsets, pin_offset) =
                compile::gate_footprint((0, 0, 0), &definition, facing);
            for x in window.0.x - APPROACH_REACH..=window.1.x + APPROACH_REACH {
                for z in window.0.z - APPROACH_REACH..=window.1.z + APPROACH_REACH {
                    let anchor = Anchor { x, y: PLANNER_Y, z };
                    let mut sockets = Vec::with_capacity(arity);
                    let mut approaches = Vec::with_capacity(arity);
                    // A landing whose socket wants dust in a cell this window
                    // does not contain is outside the question being asked, and
                    // is dropped rather than answered. A landing whose approach
                    // is *in* the window and cannot be reached is a different
                    // thing entirely -- an answer -- and becomes a clause below,
                    // so that an unsatisfiable window's core says which of the
                    // two it was.
                    let mut in_window = true;
                    for (input, net) in nets.iter().enumerate().take(arity) {
                        let (socket, approach) = socket_and_approach(anchor, facing, input);
                        sockets.push(socket);
                        approaches.push(approach);
                        if !net.seeds.contains(&approach) && !net.index.contains_key(&approach) {
                            in_window = false;
                        }
                    }
                    if !in_window {
                        continue;
                    }
                    offered += 1;

                    let fit = BodyFit {
                        origin: anchor,
                        cells: &body_offsets,
                        conductors: &conductor_offsets,
                        sockets: &sockets,
                        approaches: &approaches,
                        drivers: &drivers,
                        pins: &pins,
                        pin: shifted(anchor, pin_offset),
                        // The escape knob is growth's, and it is off here for the
                        // same reason it is off there: the default run is the
                        // paradigm, not a repair of it.
                        escape: 0,
                    };
                    if !fit.allowed(&growth.reservation) {
                        body_rejected += 1;
                        body_rejected_at.push((anchor, facing));
                        continue;
                    }

                    let place = cnf.var();
                    let body: Vec<Anchor> =
                        body_offsets.iter().map(|offset| shifted(anchor, *offset)).collect();
                    let conductors: Vec<Anchor> =
                        conductor_offsets.iter().map(|offset| shifted(anchor, *offset)).collect();

                    for cell in &body {
                        for net in &nets {
                            if let Some(&at) = net.index.get(cell) {
                                cnf.add([-place, -net.used[at]], group_body);
                            }
                        }
                    }
                    for conductor in &conductors {
                        // A wire may not stand on a gate's conductor: its floor
                        // would be written over it. `anchor_is_free_for`'s below
                        // arm, asked from the gate's side.
                        let above = Anchor { y: conductor.y + 1, ..*conductor };
                        for net in &nets {
                            if let Some(&at) = net.index.get(&above) {
                                cnf.add([-place, -net.used[at]], group_body);
                            }
                        }
                        // Asked from the gate's side, so conservative by construction --
                            // [`keep_out`], the same as `BodyFit`.
                            for neighbour in keep_out(*conductor) {
                            // `BodyFit`'s own exemption, verbatim: a socket is
                            // meant to have the arriving net's dust one cell out.
                            let arriving = sockets
                                .iter()
                                .enumerate()
                                .any(|(input, socket)| {
                                    *conductor == *socket && neighbour == approaches[input]
                                });
                            if arriving {
                                continue;
                            }
                            for net in &nets {
                                if let Some(&at) = net.index.get(&neighbour) {
                                    cnf.add([-place, -net.used[at]], group_body_clear);
                                }
                            }
                        }
                    }
                    for (input, net) in nets.iter().enumerate() {
                        match net.reach_at(approaches[input], net.depth) {
                            Reach::Always => {}
                            Reach::At(literal) => cnf.add([-place, literal], group_goal),
                            Reach::Never => cnf.add([-place], group_goal),
                        }
                    }

                    landings.push(SolvedLanding { anchor, facing, approaches, place });
                }
            }
        }

        let place_literals: Vec<Lit> = landings.iter().map(|landing| landing.place).collect();
        cnf.exactly_one(&place_literals, group_place);

        WindowModel {
            nets,
            landings,
            cnf,
            group_reach,
            body_rejected,
            body_rejected_at,
            offered,
            aliased_inputs,
        }
    }

    /// Why the model has no variable for a cell: the domain filter's arms,
    /// asked one at a time.
    ///
    /// "The window model has no variable for (26, 1, 146)" is the uninformative
    /// address one level down again. When the answer growth itself produced
    /// falls outside the model's domain, the useful output is **which rule
    /// excluded it** -- because one of the two is true and they need different
    /// work: either the model states a rule the router does not, which is an
    /// encoding bug, or the flat restriction bit, which is a known limit.
    fn why_not_in_domain(
        growth: &Growth,
        signal: &str,
        pin: Anchor,
        cell: Anchor,
        window: (Anchor, Anchor),
    ) -> String {
        if cell.y != PLANNER_Y {
            return format!("it is at y = {}, and this model gives new wire the gate plane only", cell.y);
        }
        if !within_bounds(cell, window.0, window.1) {
            return "it is outside the window".to_string();
        }
        if let Some(owner) = growth.reservation.owner(&cell) {
            return format!("the reservation already gives it to `{owner}`");
        }
        let below = Anchor { y: cell.y - 1, ..cell };
        if let Some(owner) = growth.reservation.conductor_owner(&below) {
            return format!("its floor at y = {} conducts for `{owner}`", below.y);
        }
        for neighbour in keep_out(cell) {
            if neighbour == pin {
                continue;
            }
            if let Some(owner) = growth.reservation.conductor_owner(&neighbour) {
                if owner != signal {
                    return format!(
                        "keep_out sees `{owner}`'s conductor at ({}, {}, {})",
                        neighbour.x, neighbour.y, neighbour.z
                    );
                }
            }
        }
        "no arm of the domain filter refuses it, so this is a bookkeeping defect".to_string()
    }

    /// The model, with the answer growth itself produced asserted into it.
    ///
    /// **This is the load-bearing half of the KNOWN SAT calibration and the
    /// free solve is not.** Asking the solver to *find* a landing measures the
    /// search; asserting a landing we already know is legal and asking whether
    /// the constraints permit it measures the **encoding**, which is the thing
    /// that can be silently wrong. A window the search cannot crack in its
    /// budget is a scale limit; a window whose constraints reject a plan that
    /// verifies is a wrong-answer generator.
    ///
    /// Every domain cell not on one of growth's own walks is forced *false*, so
    /// what is being asked is whether that exact configuration satisfies every
    /// clause -- not whether some superset of it does.
    #[allow(clippy::too_many_arguments)]
    fn with_known_answer(
        model: &WindowModel,
        growth: &Growth,
        gate: usize,
        anchor: Anchor,
        facing: geometry::CellFacing,
        routes: &[Vec<Anchor>],
        window: (Anchor, Anchor),
        intruder: Option<(usize, Anchor)>,
    ) -> Result<Cnf, String> {
        let mut cnf = model.cnf.clone();
        let group = cnf.group("the landing growth itself produced");

        let landing = model
            .landings
            .iter()
            .find(|landing| landing.anchor == anchor && landing.facing == facing)
            .ok_or_else(|| {
                format!(
                    "the model offers no landing at ({}, {}, {}) facing {}, which is where \
                     growth put this gate",
                    anchor.x,
                    anchor.y,
                    anchor.z,
                    facing.index()
                )
            })?;
        cnf.add([landing.place], group);

        for (input, net) in model.nets.iter().enumerate() {
            let walk = routes.get(input).map(Vec::as_slice).unwrap_or(&[]);
            let mut wanted: BTreeSet<usize> = BTreeSet::new();
            if let Some((into, cell)) = intruder {
                if into == input {
                    match net.index.get(&cell) {
                        Some(&at) => {
                            wanted.insert(at);
                        }
                        None => {
                            return Err(format!(
                                "the intruder cell ({}, {}, {}) is not in net {}'s domain",
                                cell.x, cell.y, cell.z, net.signal
                            ))
                        }
                    }
                }
            }
            for &cell in walk {
                if net.seeds.contains(&cell) {
                    continue;
                }
                match net.index.get(&cell) {
                    Some(&at) => {
                        wanted.insert(at);
                    }
                    None => {
                        return Err(format!(
                            "growth laid net {} through ({}, {}, {}) and the model has no \
                             variable for it: {}",
                            net.signal,
                            cell.x,
                            cell.y,
                            cell.z,
                            why_not_in_domain(growth, &net.signal, net.pin, cell, window)
                        ))
                    }
                }
            }
            for at in 0..net.domain.len() {
                if wanted.contains(&at) {
                    cnf.add([net.used[at]], group);
                } else {
                    cnf.add([-net.used[at]], group);
                }
            }
        }

        // A structural read of the same question, so an UNSAT here arrives as a
        // cell and a rule rather than as a group name. "The encoding rejects the
        // known answer" is the uninformative address one last time; what the
        // next person needs is which body cell, which neighbour, and which of
        // the shipping predicates the model thought it was quoting.
        if intruder.is_none() {
            if let Some(clash) = body_clash(growth, gate, anchor, facing, routes) {
                return Err(clash);
            }
        }
        Ok(cnf)
    }

    /// The body-against-wire clauses, re-derived and compared against the
    /// shipping predicate that is supposed to be their source.
    ///
    /// [`anchor_is_free_for`] is asked the same question about the same cell
    /// against a reservation that has the gate standing in it. Where the two
    /// disagree, the model states a rule the router does not -- and that is the
    /// failure this whole phase exists to catch, so it is reported with both
    /// answers rather than with one.
    fn body_clash(
        growth: &Growth,
        gate: usize,
        anchor: Anchor,
        facing: geometry::CellFacing,
        routes: &[Vec<Anchor>],
    ) -> Option<String> {
        let definition = &growth.netlist.gates[gate];
        let (body, conductors, _) =
            compile::gate_footprint((anchor.x, anchor.y, anchor.z), definition, facing);
        let mut sockets = Vec::new();
        let mut approaches = Vec::new();
        for input in 0..definition.inputs.len() {
            let (socket, approach) = socket_and_approach(anchor, facing, input);
            sockets.push(socket);
            approaches.push(approach);
        }

        // What the router sees when it lays these routes: the body claimed,
        // then the approaches, exactly as `lay` writes them.
        let mut reservation = growth.reservation.clone();
        let owner = format!("primitive:{gate}");
        for &cell in &body {
            let occupancy = if conductors.contains(&cell) {
                Occupancy::GateConductor
            } else {
                Occupancy::Solid
            };
            reservation.insert(cell, &owner, occupancy);
        }

        for (input, walk) in routes.iter().enumerate() {
            let signal = &definition.inputs[input];
            let pin = growth.pins[signal];
            for &cell in walk.iter().skip(1) {
                for conductor in &conductors {
                    if !keep_out(*conductor).contains(&cell) {
                        continue;
                    }
                    let arriving = *conductor == sockets[input] && cell == approaches[input];
                    if arriving {
                        continue;
                    }
                    // The model forbids this pairing. Does the router?
                    let router_allows = anchor_is_free_for(
                        cell,
                        pin,
                        approaches[input],
                        sockets[input],
                        signal,
                        &reservation,
                    );
                    return Some(format!(
                        "the model forbids net {signal}'s cell ({}, {}, {}) because the gate's \
                         conductor at ({}, {}, {}) is within keep_out of it, and \
                         `anchor_is_free_for` {} the same cell against the same reservation. \
                         The exemptions the router makes and the model's `arriving` test does \
                         not: start = the driver's pin ({}, {}, {}), goal = the approach \
                         ({}, {}, {}), terminal_support = the socket ({}, {}, {})",
                        cell.x,
                        cell.y,
                        cell.z,
                        conductor.x,
                        conductor.y,
                        conductor.z,
                        if router_allows { "ALLOWS" } else { "also refuses" },
                        pin.x,
                        pin.y,
                        pin.z,
                        approaches[input].x,
                        approaches[input].y,
                        approaches[input].z,
                        sockets[input].x,
                        sockets[input].y,
                        sockets[input].z,
                    ));
                }
            }
        }
        None
    }

    /// The walk a solved cell set makes from the net's existing tree to the
    /// approach.
    ///
    /// A breadth-first search *inside the answer*: the model decides which cells
    /// the net gets, and this reads a route out of them. Deterministic --
    /// `BTreeSet` frontier, `Anchor` order -- and it reuses
    /// [`reconstruct_path`], which is what makes `path[0]` the laid cell the
    /// branch hangs off, exactly as `flood_from` leaves it.
    fn path_through(net: &NetWindow, used: &BTreeSet<Anchor>, goal: Anchor) -> Option<Vec<Anchor>> {
        if net.seeds.contains(&goal) {
            return Some(vec![goal]);
        }
        let mut previous: BTreeMap<Anchor, Anchor> = BTreeMap::new();
        let mut seen: BTreeSet<Anchor> = net.seeds.clone();
        let mut frontier: BTreeSet<Anchor> = net.seeds.clone();
        while !frontier.is_empty() {
            let mut next: BTreeSet<Anchor> = BTreeSet::new();
            for &cell in &frontier {
                for neighbour in plane_neighbours(cell) {
                    if seen.contains(&neighbour) || !used.contains(&neighbour) {
                        continue;
                    }
                    seen.insert(neighbour);
                    previous.insert(neighbour, cell);
                    if neighbour == goal {
                        return Some(reconstruct_path(previous, goal));
                    }
                    next.insert(neighbour);
                }
            }
            frontier = next;
        }
        None
    }

    /// What a solve concluded, decoded into things the planner speaks.
    struct SolvedWindow {
        anchor: Anchor,
        facing: geometry::CellFacing,
        routes: BTreeMap<usize, Vec<Anchor>>,
        /// Cells the model gave each net but the extracted walk did not use.
        /// Never zero by construction, but worth reporting: a large number means
        /// the model is buying connectivity it does not need.
        stranded: usize,
    }

    fn decode(model: &WindowModel, assignment: &[bool]) -> Result<SolvedWindow, String> {
        let chosen: Vec<&SolvedLanding> = model
            .landings
            .iter()
            .filter(|landing| satcnf::value(assignment, landing.place))
            .collect();
        if chosen.len() != 1 {
            return Err(format!("{} landings are true; exactly one must be", chosen.len()));
        }
        let landing = chosen[0];

        let mut routes: BTreeMap<usize, Vec<Anchor>> = BTreeMap::new();
        let mut stranded = 0usize;
        for (input, net) in model.nets.iter().enumerate() {
            let used: BTreeSet<Anchor> = net
                .domain
                .iter()
                .enumerate()
                .filter(|(at, _)| satcnf::value(assignment, net.used[*at]))
                .map(|(_, &cell)| cell)
                .collect();
            let Some(path) = path_through(net, &used, landing.approaches[input]) else {
                return Err(format!(
                    "net {} has no walk through its own solved cells to ({}, {}, {})",
                    net.signal,
                    landing.approaches[input].x,
                    landing.approaches[input].y,
                    landing.approaches[input].z
                ));
            };
            stranded += used.len() + 1 - path.len().min(used.len() + 1);
            routes.insert(input, path);
        }

        Ok(SolvedWindow { anchor: landing.anchor, facing: landing.facing, routes, stranded })
    }


    /// The three rules the single-plane restriction claims to discharge, run
    /// rather than asserted in prose.
    ///
    /// Each is a property of a *shipping* predicate, and the model leans on all
    /// three: if any one of them stopped holding, the flat model would be
    /// silently missing a constraint, which is the encoding-gap failure this
    /// whole phase exists to guard against. So they are pinned here, in the
    /// suite rather than in a harness, where a change to the router breaks them.
    #[test]
    fn the_flat_restriction_discharges_exactly_the_three_rules_it_claims() {
        let here = Anchor { x: 20, y: PLANNER_Y, z: 30 };

        // 1. A flat step needs no staircase clearance, and a step in y does.
        for next in plane_neighbours(here) {
            assert!(
                staircase_clearance(here, next).is_empty(),
                "a flat step to ({}, {}, {}) asked for stair cells",
                next.x,
                next.y,
                next.z
            );
        }
        let up = Anchor { x: here.x + 1, y: here.y + 1, z: here.z };
        let down = Anchor { x: here.x + 1, y: here.y - 1, z: here.z };
        assert_eq!(staircase_clearance(here, up).len(), 2, "a climb needs a riser and headroom");
        assert_eq!(staircase_clearance(here, down).len(), 1, "a descent needs its riser empty");

        // 2. A flat walk cannot obstruct itself, however it doubles back.
        let walk = [
            here,
            Anchor { x: here.x + 1, ..here },
            Anchor { x: here.x + 1, z: here.z + 1, ..here },
            Anchor { x: here.x, z: here.z + 1, ..here },
            Anchor { z: here.z + 2, ..here },
            Anchor { x: here.x + 1, z: here.z + 2, ..here },
        ];
        let mut walked: BTreeMap<Anchor, Anchor> = BTreeMap::new();
        for pair in walk.windows(2) {
            assert!(
                !self_obstructs(&walked, pair[0], pair[1]),
                "a flat walk obstructed itself at ({}, {}, {})",
                pair[1].x,
                pair[1].y,
                pair[1].z
            );
            walked.insert(pair[1], pair[0]);
        }
        // The controls: the same predicate does fire once the walk leaves the
        // plane, so the assertions above are about flatness and not about
        // `self_obstructs` being inert. One case per arm, and each step is one
        // [`neighbours`] really offers -- the first draft of this used a step
        // straight down, which dust cannot take, and asserted about a case the
        // router can never reach.
        //
        // Arm one, the smothered climb: the walk leaves `here` upward, and a
        // later step would land two levels directly above `here`, whose floor
        // fills the cell that climb needed.
        let mut climbed: BTreeMap<Anchor, Anchor> = BTreeMap::new();
        let stepped_up = Anchor { x: here.x + 1, y: here.y + 1, z: here.z };
        let two_above = Anchor { y: here.y + 2, ..here };
        climbed.insert(stepped_up, here);
        assert!(
            neighbours(stepped_up).contains(&two_above),
            "the control has to use a step dust can take"
        );
        assert!(
            self_obstructs(&climbed, stepped_up, two_above),
            "a step whose own floor smothers a cell this walk climbed out of has to be refused"
        );

        // Arm two, the blocked drop: the walk already passed through the cell a
        // drop needs to fall past.
        let mut dropped: BTreeMap<Anchor, Anchor> = BTreeMap::new();
        let overhead = Anchor { x: here.x + 1, y: here.y + 2, z: here.z };
        let across = Anchor { y: here.y + 2, ..here };
        let below = Anchor { y: here.y + 1, ..here };
        dropped.insert(across, overhead);
        dropped.insert(below, across);
        let falling = Anchor { x: here.x + 1, y: here.y, z: here.z };
        assert!(
            neighbours(below).contains(&falling),
            "the control has to use a step dust can take"
        );
        assert!(
            self_obstructs(&dropped, below, falling),
            "a drop past a cell this walk already occupies has to be refused"
        );

        // 3. `neighbours` restricted to the plane is exactly the four
        //    horizontal steps, so no in-plane step is lost by the filter.
        assert_eq!(plane_neighbours(here).len(), 4);
        assert_eq!(neighbours(here).len(), 12);
        for next in plane_neighbours(here) {
            assert!(horizontal_neighbours(here).contains(&next));
        }
    }

    /// How a calibration run is tuned. Same shape as [`GrowthSettings`], for the
    /// same reason: the default run is the reproduction and an override is a
    /// sweep.
    #[derive(Debug, Clone)]
    struct SolveSettings {
        circuit: String,
        gates: String,
        margin: i32,
        reach_cap: usize,
        budget: u64,
    }

    /// Grow `case` with growth's own defaults, pausing before `gate`.
    fn growth_paused_before<'a>(
        netlist: &'a Netlist,
        settings: &'a GrowthSettings,
        gate: &str,
    ) -> Result<(Growth<'a>, usize), String> {
        let mut growth = Growth::seeded(netlist, settings)?;
        match growth.grow_stopping_before(Some(gate)) {
            Some(index) => Ok((growth, index)),
            None => Err(format!(
                "growth never reached {gate}: it stopped at {}",
                growth
                    .wedge
                    .as_ref()
                    .map(|wedge| wedge.gate.clone())
                    .unwrap_or_else(|| "the end of the circuit".to_string())
            )),
        }
    }

    /// The window a calibration case asks about: the box around what growth's
    /// own answer occupies, grown by `margin`.
    ///
    /// **This is the honest shape of a known-answer test and it is worth being
    /// explicit about.** The window is chosen so that the answer we already have
    /// is inside it. That is the point -- a model that says UNSAT on a case we
    /// can build by hand is wrong -- and it is *not* evidence that the model
    /// finds answers nobody had. What it establishes is the direction that has
    /// to hold first: the encoding admits the legal configurations it should.
    fn window_around_answer(
        growth: &Growth,
        gate: usize,
        anchor: Anchor,
        facing: geometry::CellFacing,
        routes: &[Vec<Anchor>],
        margin: i32,
    ) -> (Anchor, Anchor) {
        let definition = &growth.netlist.gates[gate];
        let (body, _, _) = compile::gate_footprint((anchor.x, anchor.y, anchor.z), definition, facing);
        let mut cells: Vec<Anchor> = body;
        for route in routes {
            cells.extend(route.iter().copied());
        }
        for signal in &definition.inputs {
            cells.push(growth.pins[signal]);
        }
        growth_window(&cells, margin)
    }

    /// Solve one window and say, in one line, exactly what was concluded.
    fn report_solve(label: &str, model: &WindowModel, outcome: &Outcome, seconds: f64) {
        let verdict = match outcome {
            Outcome::Sat(_) => "SAT",
            Outcome::Unsat if model.exact() => "UNSAT (exact for this window)",
            Outcome::Unsat => "UNSAT (bounded: capped path length, NOT a proof of infeasibility)",
            Outcome::Unknown => "UNKNOWN (budget exhausted -- this is not UNSAT)",
        };
        eprintln!("    {label}: {verdict} in {seconds:.2}s");
        eprintln!("      {}", model.summary());
        for net in &model.nets {
            if net.stranded_entirely() && matches!(outcome, Outcome::Unsat) {
                eprintln!(
                    "      net {} can reach no cell of this window at all, at any path length \
                     -- so this UNSAT does not depend on the depth cap",
                    net.signal
                );
            }
            if net.seeds_off_plane > 0 {
                eprintln!(
                    "      net {} has {} seed cell(s) off the gate plane, which this model \
                     cannot grow from",
                    net.signal, net.seeds_off_plane
                );
            }
        }
    }

    /// What one gate's KNOWN SAT calibration concluded.
    struct SatCase {
        /// The finished circuit, with this gate's landing decided by the model.
        candidate: PlanCandidate,
        gate: usize,
        /// Whether the *unaided* solve decided the window, as opposed to only
        /// the encoding check passing.
        searched: bool,
        /// The cells this window's answer put down, so the injection below
        /// perturbs something the model chose rather than something it merely
        /// stood next to.
        decided: Vec<Anchor>,
        /// Whether the clearance calibration found a pair of nets to run on.
        clearance: usize,
        /// Whether the answer growth found is inside the model's declared scope
        /// at all. `Some(cell)` names the cell that leaves the gate plane.
        out_of_scope: Option<Anchor>,
        route_cells: usize,
    }

    /// **Calibration 1: KNOWN SAT**, in two halves that measure different
    /// things and must not be confused with each other.
    ///
    /// **(a) The encoding admits the known answer.** Growth's own landing and
    /// its own walks are asserted into the model and the constraints are asked
    /// whether that exact configuration is permitted. This is the check that
    /// matters: it cannot time out into a false negative, and a failure names
    /// the group -- or the cell and the rule -- that rejected a plan which
    /// `verify_candidate` accepts. **An UNSAT here is a wrong-answer
    /// generator.**
    ///
    /// **(b) The search finds an answer unaided.** The same model with nothing
    /// assumed. A SAT answer is decoded, laid through `Growth::lay` and
    /// verified. An `Unknown` here is a statement about this solver's budget on
    /// this window, not about the window -- and it is reported as one rather
    /// than rounded into either of the other two answers.
    fn calibrate_known_sat(
        case: &ConditionCircuit,
        gate_name: &str,
        settings: &SolveSettings,
        growth_settings: &GrowthSettings,
    ) -> Result<SatCase, String> {
        use std::time::Instant;
        let mut clearance = 0usize;

        // --- what growth itself did, which is the answer being reproduced ---
        let (mut oracle, gate) = growth_paused_before(&case.netlist, growth_settings, gate_name)?;
        let before = oracle.laid.len();
        oracle
            .land(gate)
            .map_err(|wedge| format!("growth wedged at {} rather than landing it", wedge.gate))?;
        oracle.placed[gate] = true;
        let grown_anchor = oracle.anchors[gate];
        let grown_facing = oracle.facings[gate];
        // Back into declared-input order: `lay` grows the cheapest branch first,
        // so `laid` is not in socket order and the model is.
        let mut grown_routes: Vec<Vec<Anchor>> =
            vec![Vec::new(); case.netlist.gates[gate].inputs.len()];
        for laid in &oracle.laid[before..] {
            // `Laid::path` is the walk **plus the socket**: `branch` pushes the
            // socket after the flood found the approach. What the model routes
            // to, and what `land_solved` will hand back, is the walk that ends
            // at the *approach*, so the socket comes off again here.
            //
            // Getting this backwards was the first thing the calibration caught,
            // and it caught it the way it was meant to: as
            // `anchor_is_free_for` refusing (12, 1, 67) -- the socket -- for a
            // route that was never supposed to contain it.
            let walk = &laid.path[..laid.path.len().saturating_sub(1)];
            grown_routes[laid.input] = walk.to_vec();
        }
        let grown_cells: usize =
            grown_routes.iter().map(|path| path.len().saturating_sub(1)).sum();

        // --- the same state again, with the gate still unplaced ---
        let (mut growth, gate) = growth_paused_before(&case.netlist, growth_settings, gate_name)?;
        let window = window_around_answer(
            &growth,
            gate,
            grown_anchor,
            grown_facing,
            &grown_routes,
            settings.margin,
        );

        let started = Instant::now();
        let model = window_model(&growth, gate, window, settings.reach_cap);
        let built = started.elapsed().as_secs_f64();
        eprintln!(
            "    window ({}, {}, {})..({}, {}, {}) built in {built:.2}s | growth landed it at \
             ({}, {}, {}) facing {} laying {grown_cells} cell(s) of wire",
            window.0.x,
            window.0.y,
            window.0.z,
            window.1.x,
            window.1.y,
            window.1.z,
            grown_anchor.x,
            grown_anchor.y,
            grown_anchor.z,
            grown_facing.index()
        );
        eprintln!("      {}", model.summary());

        // --- (a) does the encoding admit the answer we already have? ---
        //
        // Only askable when that answer is inside the model's declared scope. A
        // window whose known solution climbs is not a failure of the encoding
        // and must not be reported as one -- the flat restriction is stated at
        // [`window_model`] and this is where it bites. **Naming the cell is the
        // result**, and it is the reason (a) exists at all: a search that came
        // back `Unknown` on the same window would have said nothing about why.
        let out_of_scope = grown_routes
            .iter()
            .flatten()
            .copied()
            .find(|cell| cell.y != PLANNER_Y);
        match out_of_scope {
            Some(cell) => eprintln!(
                "      (a) OUT OF SCOPE: growth's own answer leaves the gate plane at \
                 ({}, {}, {}), so this window's known answer is not one this model can \
                 express. The encoding is not under test here; (b) still is.",
                cell.x, cell.y, cell.z
            ),
            None => {
                let assumed = with_known_answer(
                    &model,
                    &growth,
                    gate,
                    grown_anchor,
                    grown_facing,
                    &grown_routes,
                    window,
                    None,
                )?;
                let started = Instant::now();
                let admitted = assumed.solve(settings.budget);
                let admitted_in = started.elapsed().as_secs_f64();
                match &admitted {
                    Outcome::Sat(_) => {
                        eprintln!(
                            "      (a) the encoding ADMITS growth's own landing and walks, \
                             {admitted_in:.2}s"
                        );
                        clearance += usize::from(calibrate_clearance(
                            &model,
                            &growth,
                            gate,
                            grown_anchor,
                            grown_facing,
                            &grown_routes,
                            window,
                            settings,
                        )?);
                    }
                    Outcome::Unknown => {
                        return Err(
                            "the encoding check exhausted its budget, which it should never \
                             do: everything added to it is a unit clause"
                                .to_string(),
                        )
                    }
                    Outcome::Unsat => {
                        let core = assumed
                            .core(settings.budget)
                            .map(|groups| {
                                groups
                                    .iter()
                                    .map(|&group| assumed.group_name(group).to_string())
                                    .collect::<Vec<_>>()
                                    .join(" + ")
                            })
                            .unwrap_or_else(|| "no core".to_string());
                        return Err(format!(
                            "the encoding REJECTS a landing that growth laid and \
                             `verify_candidate` accepts. That is an encoding bug, not a \
                             result. Core: {core}"
                        ));
                    }
                }
            }
        }

        // --- (b) can the search find one on its own? ---
        let started = Instant::now();
        let outcome = model.cnf.solve(settings.budget);
        let solved = started.elapsed().as_secs_f64();
        report_solve(&format!("(b) {gate_name} unaided"), &model, &outcome, solved);

        let (anchor, facing, routes, searched) = match &outcome {
            // UNSAT where the known answer is flat is a contradiction and an
            // encoding bug. UNSAT where the known answer climbs is an *answer*:
            // this window admits no flat solution at all, which is a real
            // statement about the geometry and exactly the kind of thing a
            // complete procedure can say and a search cannot.
            Outcome::Unsat if out_of_scope.is_none() => {
                return Err(format!(
                    "the model says UNSAT on a window growth itself solved flat -- growth \
                     landed {gate_name} at ({}, {}, {}) facing {}",
                    grown_anchor.x,
                    grown_anchor.y,
                    grown_anchor.z,
                    grown_facing.index()
                ))
            }
            Outcome::Unsat => {
                eprintln!(
                    "      NO FLAT SOLUTION exists in this window{}. Growth's own answer \
                     climbs, so this agrees with it rather than contradicting it.",
                    if model.exact() {
                        ""
                    } else {
                        ", for any route within the depth cap"
                    }
                );
                let mut routes: BTreeMap<usize, Vec<Anchor>> = BTreeMap::new();
                for (input, path) in grown_routes.iter().enumerate() {
                    routes.insert(input, path.clone());
                }
                (grown_anchor, grown_facing, routes, false)
            }
            Outcome::Unknown => {
                eprintln!(
                    "      the unaided search did not decide this window inside {} conflicts. \
                     **That is a budget, not an infeasibility**, and it is reported as its \
                     own answer rather than folded into either of the other two.{} The round \
                     trip below runs on growth's own answer instead.",
                    settings.budget,
                    if out_of_scope.is_none() {
                        " (a) above already showed the constraints permit a solution."
                    } else {
                        " (a) could not be asked here, because growth's own answer climbs."
                    }
                );
                let mut routes: BTreeMap<usize, Vec<Anchor>> = BTreeMap::new();
                for (input, path) in grown_routes.iter().enumerate() {
                    routes.insert(input, path.clone());
                }
                (grown_anchor, grown_facing, routes, false)
            }
            Outcome::Sat(assignment) => {
                let solution = decode(&model, assignment)?;
                eprintln!(
                    "      decoded: ({}, {}, {}) facing {} | routes {} | {} cell(s) solved but \
                     unused | {}",
                    solution.anchor.x,
                    solution.anchor.y,
                    solution.anchor.z,
                    solution.facing.index(),
                    solution
                        .routes
                        .values()
                        .map(|path| path.len().to_string())
                        .collect::<Vec<_>>()
                        .join("+"),
                    solution.stranded,
                    if solution.anchor == grown_anchor && solution.facing == grown_facing {
                        "the same place growth chose"
                    } else {
                        "a different place from growth, which is allowed: the question is \
                         legality, not agreement"
                    },
                );

                // **Rule 2, on the hardest part of the encoding.** Connectivity
                // is where SAT routing is normally won or lost, and "the answer
                // happened to be connected" is not evidence that anything forced
                // it to be. So the ladder is taken out and the same window asked
                // again: with it gone nothing anywhere makes a `use` variable
                // true -- the goal clause only asks for a `reach` literal, and
                // the ladder is what turns that into wire -- so the model comes
                // back satisfiable with no route at all and the decode cannot
                // find a walk.
                //
                // Skipped, loudly, when the window needed no wire: for a landing
                // whose socket sits on its driver's pin there is nothing for the
                // ladder to force, and a test that passes there proves nothing.
                if solution.routes.values().any(|path| path.len() > 1) {
                    let relaxed =
                        model.cnf.solve_groups(&model.without_connectivity(), settings.budget);
                    match &relaxed {
                        Outcome::Sat(assignment) => match decode(&model, assignment) {
                            Ok(_) => {
                                return Err(
                                    "with the connectivity ladder removed the model still \
                                     produced a connected route, so nothing in this encoding is \
                                     forcing connectivity"
                                        .to_string(),
                                )
                            }
                            Err(why) => {
                                eprintln!("      without the connectivity ladder: {why}")
                            }
                        },
                        other => {
                            return Err(format!(
                                "removing the connectivity ladder should only ever make a \
                                 window easier, and it came back {other:?}"
                            ))
                        }
                    }
                } else {
                    eprintln!(
                        "      no wire needed here (every socket lands on its driver's pin), \
                         so the connectivity ladder is NOT under test on this gate"
                    );
                }

                (solution.anchor, solution.facing, solution.routes, true)
            }
        };

        let route_cells: usize =
            routes.values().map(|path| path.len().saturating_sub(1)).sum();
        let decided: Vec<Anchor> =
            routes.values().flat_map(|path| path.iter().skip(1).copied()).collect();
        growth
            .land_solved(gate, anchor, facing, &routes, window)
            .map_err(|why| format!("the answer would not lay: {why}"))?;

        // The rest of the circuit is grown normally, so what goes to
        // `verify_candidate` is a whole circuit with one gate's landing decided
        // by the model.
        growth.grow_stopping_before(None);
        if let Some(wedge) = &growth.wedge {
            return Err(format!("growth wedged at {} after the landing", wedge.gate));
        }
        let candidate = growth
            .candidate()
            .ok_or_else(|| "growth built no candidate after the landing".to_string())?;
        Ok(SatCase { candidate, gate, searched, out_of_scope, decided, route_cells, clearance })
    }

    /// **A known UNSAT that turns on clearance alone**, built by adding one
    /// cell to an answer that is otherwise known good.
    ///
    /// This exists because the obvious calibration does not reach the rule the
    /// spec cares most about. `and4`'s windows never make two nets *contend*:
    /// six of its seven gates land a socket straight onto their driver's pin and
    /// lay no wire at all, and the seventh -- `g3` -- is the one whose known
    /// answer climbs. Deleting every between-net clearance clause was injected
    /// and left `g6`'s calibration **green**, which is the honest measurement
    /// that the group was untested and the reason this function exists.
    ///
    /// So the case is constructed rather than found: take growth's own answer,
    /// which the model admits, and force one extra cell into a *different* net
    /// -- a cell chosen to be within [`keep_out`] of the first net's wire and
    /// clear of everything else. The window must go UNSAT, and the core must
    /// name the clearance group and nothing else that would explain it away.
    ///
    /// Returns whether a pair of nets was available to run on at all, so the
    /// report can say "not exercised" rather than imply it passed.
    #[allow(clippy::too_many_arguments)]
    fn calibrate_clearance(
        model: &WindowModel,
        growth: &Growth,
        gate: usize,
        anchor: Anchor,
        facing: geometry::CellFacing,
        routes: &[Vec<Anchor>],
        window: (Anchor, Anchor),
        settings: &SolveSettings,
    ) -> Result<bool, String> {
        let definition = &growth.netlist.gates[gate];
        let (body, conductors, _) =
            compile::gate_footprint((anchor.x, anchor.y, anchor.z), definition, facing);
        let forbidden_by_body: BTreeSet<Anchor> = conductors
            .iter()
            .flat_map(|conductor| keep_out(*conductor))
            .chain(conductors.iter().copied())
            .chain(body.iter().copied())
            .collect();

        for (into, net) in model.nets.iter().enumerate() {
            for (other, wire) in routes.iter().enumerate() {
                if other == into || model.nets[other].signal == net.signal {
                    continue;
                }
                // Skip the root: it is a cell the other net already held, so a
                // clash with it is the *fixed* world's business and is settled
                // by the domain filter rather than by a clause.
                for &cell in wire.iter().skip(1) {
                    for candidate in keep_out(cell) {
                        if forbidden_by_body.contains(&candidate)
                            || routes[into].contains(&candidate)
                            || !net.index.contains_key(&candidate)
                        {
                            continue;
                        }
                        let intruded = with_known_answer(
                            model,
                            growth,
                            gate,
                            anchor,
                            facing,
                            routes,
                            window,
                            Some((into, candidate)),
                        )?;
                        match intruded.solve(settings.budget) {
                            Outcome::Unsat => {}
                            verdict => {
                                return Err(format!(
                                    "net {}'s wire at ({}, {}, {}) and net {}'s cell at \
                                     ({}, {}, {}) are within keep_out of each other, which the \
                                     shipping router refuses, and the model came back {verdict:?}",
                                    growth.netlist.gates[gate].inputs[other],
                                    cell.x,
                                    cell.y,
                                    cell.z,
                                    net.signal,
                                    candidate.x,
                                    candidate.y,
                                    candidate.z
                                ))
                            }
                        }
                        let core = intruded.core(settings.budget).ok_or_else(|| {
                            "the clearance case is unsatisfiable and produced no core".to_string()
                        })?;
                        let named: Vec<String> = core
                            .iter()
                            .map(|&group| intruded.group_name(group).to_string())
                            .collect();
                        if !named.iter().any(|name| name.contains("keep clear of each other")) {
                            return Err(format!(
                                "the clearance case is UNSAT for the wrong reason: {named:?}"
                            ));
                        }
                        eprintln!(
                            "      (c) one cell of net {} moved to ({}, {}, {}), within \
                             keep_out of net {}'s wire at ({}, {}, {}), makes the same window \
                             UNSAT -- core: {}",
                            net.signal,
                            candidate.x,
                            candidate.y,
                            candidate.z,
                            growth.netlist.gates[gate].inputs[other],
                            cell.x,
                            cell.y,
                            cell.z,
                            named.join(" + ")
                        );
                        return Ok(true);
                    }
                }
            }
        }
        eprintln!(
            "      (c) no two nets of this gate both lay wire in this window, so the clearance \
             rule is NOT under test here"
        );
        Ok(false)
    }

    /// **Calibration 2: KNOWN UNSAT.** Seal a driver's pin and the answer must
    /// be UNSAT, with a core naming the constraints that conflict.
    ///
    /// The seal is not arbitrary: it is the shape every measured wedge on this
    /// branch has. `segment_a` stops with net `g7` unable to leave `(26, 1,
    /// 146)`, walled by four owners. Sealing a pin by hand builds the same
    /// situation in a circuit small enough that the answer is not in doubt --
    /// which is what makes it a calibration rather than an experiment.
    fn calibrate_known_unsat(
        case: &ConditionCircuit,
        gate_name: &str,
        settings: &SolveSettings,
        growth_settings: &GrowthSettings,
    ) -> Result<Vec<String>, String> {
        // The seal is only the case it claims to be if it really strands the
        // net; `stranded_entirely` is checked below rather than assumed.
        use std::time::Instant;

        let (mut oracle, gate) = growth_paused_before(&case.netlist, growth_settings, gate_name)?;
        let before = oracle.laid.len();
        oracle.land(gate).map_err(|wedge| format!("growth wedged at {}", wedge.gate))?;
        let grown_anchor = oracle.anchors[gate];
        let grown_facing = oracle.facings[gate];
        let grown_routes: Vec<Vec<Anchor>> =
            oracle.laid[before..].iter().map(|laid| laid.path.clone()).collect();

        let (mut growth, gate) = growth_paused_before(&case.netlist, growth_settings, gate_name)?;
        let window = window_around_answer(
            &growth,
            gate,
            grown_anchor,
            grown_facing,
            &grown_routes,
            settings.margin,
        );

        // Seal the first driver's pin: every cell dust could step to becomes a
        // stranger's conductor. Nothing else in the window is touched.
        let signal = growth.netlist.gates[gate].inputs[0].clone();
        let pin = growth.pins[&signal];
        for cell in neighbours(pin) {
            growth.reservation.insert(cell, "sealant", Occupancy::GateConductor);
        }
        eprintln!(
            "    sealed net {signal}'s pin at ({}, {}, {}) with {} foreign conductor(s)",
            pin.x,
            pin.y,
            pin.z,
            neighbours(pin).len()
        );

        let model = window_model(&growth, gate, window, settings.reach_cap);
        let started = Instant::now();
        let outcome = model.cnf.solve(settings.budget);
        let solved = started.elapsed().as_secs_f64();
        report_solve(&format!("{gate_name} KNOWN UNSAT"), &model, &outcome, solved);

        match outcome {
            Outcome::Unsat => {
                if !model.nets.iter().any(NetWindow::stranded_entirely) {
                    return Err(
                        "the sealed window came back UNSAT without stranding any net, so it is \
                         not the case this calibration means to build"
                            .to_string(),
                    );
                }
            }
            Outcome::Sat(assignment) => {
                let decoded = decode(&model, &assignment)
                    .map(|solution| {
                        format!(
                            "({}, {}, {}) facing {}",
                            solution.anchor.x,
                            solution.anchor.y,
                            solution.anchor.z,
                            solution.facing.index()
                        )
                    })
                    .unwrap_or_else(|why| why);
                return Err(format!(
                    "the model found a landing for a gate whose only driver is sealed in: {decoded}"
                ));
            }
            Outcome::Unknown => {
                return Err("the sealed window exhausted its budget rather than deciding".into())
            }
        }

        let core = model
            .cnf
            .core(settings.budget)
            .ok_or_else(|| "no core came back from an unsatisfiable model".to_string())?;
        let named: Vec<String> =
            core.iter().map(|&group| model.cnf.group_name(group).to_string()).collect();
        for name in &named {
            eprintln!("      core: {name}");
        }
        Ok(named)
    }

    /// **The windowed solver, and the calibration that makes its answers worth
    /// believing.**
    ///
    /// A companion to [`measure_whether_growth_places_and_routes`], not a
    /// replacement for it. Growth's weakest component is wedge escape; a
    /// complete solver's weakest deployment is whole-circuit scale. Windowed,
    /// they cover each other -- and this harness is the half that has to come
    /// first, because **an uncalibrated solver's UNSAT is worthless**.
    ///
    /// # What it does
    ///
    /// Growth is paused just before it lands a chosen gate. A box is drawn
    /// around the region that gate's landing occupies, and inside that box a
    /// CDCL solver is asked the joint question growth answers greedily: *where
    /// does this gate stand, which way does it face, and how does each of its
    /// input nets reach its socket, given everything already laid is fixed?* The
    /// answer is decoded back into a `PlanCandidate` and put through
    /// `verify_candidate` and the truth table, because a SAT answer the verifier
    /// rejects is an encoding bug and not a result.
    ///
    /// The model is documented at [`window_model`], including the one
    /// restriction it carries -- **new wire is flat** -- and the three shipping
    /// predicates that restriction discharges exactly.
    ///
    /// # Re-running it
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::calibrate_the_windowed_solver \
    ///   -- --ignored --nocapture
    /// ```
    ///
    /// | variable | default | meaning |
    /// |---|---|---|
    /// | `REDA_SOLVE_CIRCUIT` | `and4` | which circuit's growth state to window |
    /// | `REDA_SOLVE_GATES` | `all` | gate output names, comma separated, or `all` |
    /// | `REDA_SOLVE_MARGIN` | `3` | cells of slack around growth's own answer |
    /// | `REDA_SOLVE_REACH` | `96` | reachability depth cap; `0` asks for the exact bound |
    /// | `REDA_SOLVE_BUDGET` | `300000` | conflicts before a solve reports `Unknown` |
    ///
    /// # What it asserts, and why this one asserts where the growth probe does
    /// not
    ///
    /// The growth probe deliberately asserts nothing, because a probe that gates
    /// something is a probe somebody tunes until it goes green. Calibration is
    /// the opposite: **the assertion is the calibration**. A model that cannot
    /// reproduce answers we already know is not trustworthy on answers we do
    /// not, so each case below fails the test rather than printing a number.
    ///
    /// 1. **(a) The encoding admits the known answer.** Growth's own landing and
    ///    its own walks are asserted into the model and the constraints are
    ///    asked whether that exact configuration is permitted. This is the check
    ///    that matters: it cannot time out into a false negative.
    /// 2. **(b) The search finds one unaided.** Same model, nothing assumed. SAT
    ///    is decoded, laid through `Growth::lay`, and the finished circuit must
    ///    pass `verify_candidate` **and** its truth table. `Unknown` is reported
    ///    as its own answer and never as UNSAT.
    /// 3. **(c) A clearance violation makes the same window UNSAT**, with the
    ///    clearance group in the core -- see [`calibrate_clearance`] for why
    ///    this had to be constructed rather than found.
    /// 4. **KNOWN UNSAT.** A driver's pin sealed by twelve foreign
    ///    conductors -- the measured shape of every wedge on this branch --
    ///    must come back UNSAT with a core naming the placement and the goal.
    /// 5. **DECODE ROUND TRIP.** A cell of the answer is torn out of the walk
    ///    and `verify_candidate` must reject it, so "it verified" is a claim
    ///    the test could have failed to make.
    ///
    /// # What it measured
    ///
    /// `--release`, every default, whole run 115s.
    /// Every number below is a line of that run.
    ///
    /// | gate | window | model | (a) admits | (b) unaided | decoded | verifies |
    /// |---|---|---|---|---|---|---|
    /// | g0 | 9x9 | 6,052 vars / 15,409 clauses | **yes** | SAT 0.00s | (12,1,66) f3, no wire | 236 blocks, 240/240 |
    /// | g1 | 9x9 | 6,052 / 15,409 | **yes** | SAT 0.00s | (32,1,66) f3, no wire | 236 blocks, 240/240 |
    /// | g2 | 9x9 | 6,052 / 15,409 | **yes** | SAT 0.00s | (52,1,66) f3, no wire | 236 blocks, 240/240 |
    /// | g3 | 47x12, 3 nets | 118,874 / 346,809 | **out of scope** | **UNKNOWN** 110s | growth's own | 236 blocks, 240/240 |
    /// | g4 | 9x9 | 3,661 / 9,855 | **yes** | SAT 0.00s | (28,1,60) f3 -- *not* growth's (30,1,62) f0 | 252 blocks, 240/240 |
    /// | g5 | 9x9 | 6,052 / 15,409 | **yes** | SAT 0.00s | (72,1,66) f3, no wire | 236 blocks, 240/240 |
    /// | g6 | 47x15, 2 nets | 81,783 / 246,093 | **yes** | SAT 0.05s | (32,1,60) f0, 1+45 cells | 240 blocks, 240/240 |
    ///
    /// Six of seven windows are in scope and **every one of them admits the
    /// answer growth found**; six are decided unaided; all seven decode, lay,
    /// verify and compute `and4` correctly on all 240 ordered transitions.
    ///
    /// `g4` is the row worth looking at twice: the solver put the gate somewhere
    /// growth did not, and the circuit still verifies and is still right -- at
    /// 252 blocks against 236. Legality, not agreement, is what is being
    /// checked.
    ///
    /// **The sealed window.** `g6` with net `g4`'s pin ringed by twelve foreign
    /// conductors: **UNSAT in 0.02s**, on a core of two groups -- `gate g6
    /// stands in exactly one place` together with `every socket's approach is
    /// reached by its own net`. The run also prints that `g4` can reach no cell
    /// of the window *at any path length*, so that UNSAT does not depend on the
    /// depth cap.
    ///
    /// **The clearance case.** One cell of net `g4` placed at `(68, 1, 66)`,
    /// within `keep_out` of net `g5`'s wire at `(69, 1, 66)`, makes `g6`'s
    /// otherwise-admitted window UNSAT, core `two nets' conductors keep clear of
    /// each other (keep_out)`. It runs on **one of the seven windows** and the
    /// other six print `the clearance rule is NOT under test here`, because in
    /// those six only one net lays any wire at all. That coverage number is in
    /// the summary line rather than left to be inferred.
    ///
    /// # What calibration caught, which is the point of running it
    ///
    /// Three defects, each found by a case that then went green, and each of a
    /// kind no amount of reading would have settled:
    ///
    /// 1. **A convention mismatch.** `Laid::path` ends at the *socket*; the
    ///    model routes to the *approach*. The first run of (a) reported it as
    ///    `anchor_is_free_for` refusing `(12, 1, 67)` -- naming the cell and the
    ///    rule, which is exactly what a model is for.
    /// 2. **`gate_footprint` in the inner loop.** It realises a gate into a
    ///    64x8x64 scratch world and scans 32,768 cells, and a window offers
    ///    thousands of anchors. Hoisting it out took window construction from
    ///    minutes to 0.09s.
    /// 3. **The flat restriction bites on `and4` itself.** `g3`'s known answer
    ///    climbs to `(34, 2, 66)`. That is reported as OUT OF SCOPE rather than
    ///    as a failure, because the restriction is declared at
    ///    [`window_model`] -- but it means the very first circuit this model met
    ///    already has a window it cannot express.
    ///
    /// # Rule 2, on the model itself
    ///
    /// Two defects injected, confirmed red, reverted (2026-08-16, `--release`,
    /// `REDA_SOLVE_GATES=g6`):
    ///
    /// - **Delete every between-net clearance clause.** Case (c) goes red. It
    ///   does **not** go red on (a), (b) or the round trip, and that null result
    ///   is why (c) exists: `and4`'s windows never make two nets contend, so
    ///   without a constructed case the rule the spec cares most about would
    ///   have been shipped untested.
    /// - **Delete the socket-arrival exemption** (`BodyFit`'s `arriving`), which
    ///   makes the model over-strict rather than too loose. Case (a) goes red
    ///   with `the encoding REJECTS a landing that growth laid and
    ///   `verify_candidate` accepts`, core naming the body-clearance group.
    ///
    /// The solver underneath has its own known-answer tests in
    /// [`crate::compile::satcnf`]: pigeonhole UNSAT, the same generator one
    /// pigeon smaller SAT, an exhausted budget reporting `Unknown` and never
    /// `Unsat`, and 200 random 3-SAT instances checked against exhaustive search
    /// in both directions. Every SAT answer is additionally read back against
    /// every clause before it leaves `Cnf::solve_groups`.
    ///
    /// # What this does NOT establish
    ///
    /// - **That the model finds answers nobody had.** Every window here is drawn
    ///   around an answer growth already produced. That is what makes it a
    ///   known-answer test and what stops it being evidence of anything else.
    /// - **`and4`'s `g3`, at all.** Its known answer is out of scope (it climbs)
    ///   and its unaided search is undecided at 300,000 conflicts. Raised to
    ///   5,000,000 it was still searching after nine minutes and 3.3 GB of
    ///   learnt clauses, and was killed rather than left to finish, so whether
    ///   `g3`'s window has a flat solution is **NOT MEASURED** -- and note that
    ///   "not measured" is the whole point of keeping `Unknown` a separate
    ///   answer.
    /// - **Any window with three contending nets.** `g3` is the only one in this
    ///   circuit and it is the one that is undecided.
    /// - **The strength budget.** [`realise_branch_from`] is not modelled at
    ///   all; it judges at decode and can refuse. It never did on these seven
    ///   windows, which is a fact about these windows.
    /// - **Climbing routes**, staircase clearance and `self_obstructs` as
    ///   *constraints*: the flat restriction discharges them exactly rather than
    ///   encoding them, which is proved for flat answers by
    ///   `the_flat_restriction_discharges_exactly_the_three_rules_it_claims` and
    ///   says nothing about answers that climb.
    /// - **`full_adder`, `segment_a`, `seven_segment` and the real wedges.**
    ///   Deliberately out of scope for this phase: the point of calibrating is
    ///   that the *next* phase's answer can be believed.
    #[test]
    #[ignore = "calibration harness: builds a CNF per gate and solves it; about two minutes at the default budget"]
    fn calibrate_the_windowed_solver() {
        let setting = |name: &str, fallback: &str| -> String {
            std::env::var(name).unwrap_or_else(|_| fallback.to_string())
        };
        let settings = SolveSettings {
            circuit: setting("REDA_SOLVE_CIRCUIT", "and4"),
            gates: setting("REDA_SOLVE_GATES", "all"),
            margin: setting("REDA_SOLVE_MARGIN", "3").parse().expect("a margin"),
            // 96, and the reason it is not `0` is measured. `0` asks for the
            // exact bound, `|domain|`, which no simple path can exceed -- and
            // for `and4`'s `g3`, whose three input nets each see a 400-plus-cell
            // window, that is millions of ladder rungs: the run passed 1.2 GB
            // and ten minutes of CPU without finishing and was killed. 96 is
            // comfortably longer than any route in these windows (the longest
            // growth lays is 45) and every report says out loud when a bound
            // binds, because **an UNSAT under a cap is bounded by path length
            // and is not a proof of infeasibility**.
            reach_cap: setting("REDA_SOLVE_REACH", "96").parse().expect("a depth"),
            // 300,000, which is the budget the report's numbers were taken at.
            // Six of `and4`'s seven windows are decided in well under a second;
            // the seventh, `g3`, is not decided at this budget, and raising it
            // to 5,000,000 only bought nine minutes and 3.3 GB of learnt
            // clauses before the run was killed. So what the harness says about
            // `g3` is `Unknown` -- a budget, not an answer -- and raising this
            // number is not the way to change that.
            budget: setting("REDA_SOLVE_BUDGET", "300000").parse().expect("a budget"),
        };
        // Growth's own defaults, so the state being windowed is the one the
        // growth probe reports and not a variant of it.
        let growth_settings = GrowthSettings {
            order: "depth".to_string(),
            lambda: 0.5,
            windows: vec![8, 16, 32, 64],
            tries: 8,
            escape: 0,
            seed_pitch: 0,
            verbose: false,
            settle: true,
            rip: 0,
            seed: 0,
            rip_whole: false,
        };

        let (and4, and4_output) = build_and4_netlist();
        let case = ConditionCircuit {
            name: "and4",
            netlist: and4,
            inputs: &crate::circuits::and4::INPUT_NAMES[..],
            outputs: vec![and4_output],
            expected: and4_expected,
        };
        assert_eq!(
            settings.circuit, case.name,
            "only `and4` has a growth run that completes, verifies and computes correctly, \
             so it is the only circuit whose answers are known well enough to calibrate against"
        );

        let chosen: Vec<String> = if settings.gates == "all" {
            case.netlist.gates.iter().map(|gate| gate.output.clone()).collect()
        } else {
            settings.gates.split(',').map(|piece| piece.trim().to_string()).collect()
        };

        eprintln!(
            "== windowed solver calibration: {} | gates {:?} | margin {} | depth cap {} \
             | budget {} ==",
            case.name,
            chosen,
            settings.margin,
            if settings.reach_cap == 0 {
                "exact".to_string()
            } else {
                settings.reach_cap.to_string()
            },
            settings.budget
        );

        let mut verified = 0usize;
        let mut injected = 0usize;
        let mut searched = 0usize;
        let mut admitted = 0usize;
        let mut clearance = 0usize;
        let mut undecided: Vec<String> = Vec::new();
        let mut outside: Vec<String> = Vec::new();
        for gate_name in &chosen {
            eprintln!("  -- {gate_name} --");
            let case_result =
                match calibrate_known_sat(&case, gate_name, &settings, &growth_settings) {
                    Ok(result) => result,
                    Err(why) => panic!("KNOWN SAT calibration failed on {gate_name}: {why}"),
                };
            let SatCase {
                candidate,
                gate,
                searched: found,
                out_of_scope,
                decided,
                route_cells,
                clearance: clearance_here,
            } = case_result;
            clearance += clearance_here;
            if found {
                searched += 1;
            } else {
                undecided.push(gate_name.clone());
            }
            if let Some(cell) = out_of_scope {
                outside.push(format!("{gate_name}@({},{},{})", cell.x, cell.y, cell.z));
            } else {
                admitted += 1;
            }

            // Rule 6: routes without verifies is worth nothing.
            let realised = realise_and_verify(
                &candidate,
                &case.netlist,
                candidate_world_size(&candidate),
            )
            .unwrap_or_else(|error| {
                panic!("the landing decided for {gate_name} verifies nothing: {error}")
            });
            let blocks = (0..realised.world.cells().len())
                .filter(|&flat| {
                    let (x, y, z) = realised.world.decode(flat);
                    realised.world.get(x, y, z).kind
                        != crate::redstone::world::block::BlockKind::Air
                })
                .count();
            let (worst, at, transitions) =
                worst_settle_and_truth(&realised, case.inputs, &case.outputs, case.expected)
                    .unwrap_or_else(|error| {
                        panic!("the solved landing for {gate_name} computes the wrong function: {error}")
                    });
            eprintln!(
                "      VERIFIES, {blocks} blocks | truth table Ok over {transitions} ordered \
                 transitions, worst settle {worst} ticks at {at} | {route_cells} cell(s) of \
                 wire decoded"
            );
            verified += 1;

            // Rule 2: the round trip has to be able to fail. One cell of the
            // gate this window solved is moved, and the verifier must catch it.
            let signal = case.netlist.gates[gate].inputs[0].clone();
            let mut broken = candidate.clone();
            let at = broken
                .routes
                .iter()
                .position(|route| route.id == signal)
                .expect("the gate's first input has a route");
            let route = &broken.routes[at];
            // A cell this window's answer actually put down, when it put any
            // down. A landing whose socket sits on its driver's pin decides no
            // wire at all, and the fallback says so rather than claiming a
            // perturbation of something the model never chose.
            let (moved, from_the_answer) = match route
                .anchors
                .iter()
                .position(|cell| decided.contains(cell))
            {
                Some(at) => (at, true),
                None => (route.anchors.len() / 2, false),
            };
            let was = route.anchors[moved];

            // **One step is not always a defect, which is a measurement rather
            // than an inconvenience.** A route cell displaced onto a free
            // neighbour can be a perfectly legal *different* plan -- and it is,
            // on `g3`: `(10, 1, 65)` moved one step in x leaves all four
            // physical invariants happy. So the assertion is made with a
            // displacement that certainly breaks the walk, and the one-step case
            // is reported as an observation with nothing hanging on it.
            let mut nudged = broken.clone();
            nudged
                .routes
                .iter_mut()
                .find(|route| route.id == signal)
                .expect("the route was found a moment ago")
                .anchors[moved] = Anchor { x: was.x + 1, ..was };
            let one_step = verify_candidate(&nudged, &case.netlist);

            broken.routes[at].anchors[moved] = Anchor { x: was.x + 8, ..was };
            match verify_candidate(&broken, &case.netlist) {
                Ok(()) => panic!(
                    "net {signal}'s cell ({}, {}, {}) moved eight steps in x and every \
                     invariant stayed happy, so the round trip proves nothing",
                    was.x, was.y, was.z
                ),
                Err(error) => {
                    eprintln!(
                        "      injection: ({}, {}, {}){} torn out of the walk is caught -- \
                         {error}",
                        was.x,
                        was.y,
                        was.z,
                        if from_the_answer {
                            ", a cell this window's answer chose,"
                        } else {
                            ", which this window's answer did not choose (it decided no wire),"
                        }
                    );
                    eprintln!(
                        "      injection: the same cell moved *one* step {}",
                        match one_step {
                            Ok(()) =>
                                "is NOT caught -- the perturbed plan is legal, just different"
                                    .to_string(),
                            Err(error) => format!("is caught too -- {error}"),
                        }
                    );
                    injected += 1;
                }
            }

        }

        // KNOWN UNSAT, on the last gate of the list -- one is enough, and the
        // seal is the same construction wherever it is applied.
        let sealed = chosen.last().expect("at least one gate");
        eprintln!("  -- {sealed}, sealed --");
        let core = match calibrate_known_unsat(&case, sealed, &settings, &growth_settings) {
            Ok(core) => core,
            Err(why) => panic!("KNOWN UNSAT calibration failed on {sealed}: {why}"),
        };
        // **What the core is expected to name, and what it is not.** The
        // contradiction is between "this gate stands somewhere" and "every
        // socket's approach is reached by its own net": with the pin sealed,
        // the free-space reachability precomputation proves no cell of the
        // window is reachable at all, so the connectivity ladder is *empty* and
        // its group contributes no clause to name. That is a stronger outcome
        // than a searched one, not a weaker one -- the model decided it without
        // a single conflict -- and the expectation says so rather than asking
        // for a group that correctly has nothing in it.
        for expected in [
            "stands in exactly one place",
            "every socket's approach is reached by its own net",
        ] {
            assert!(
                core.iter().any(|name| name.contains(expected)),
                "the core does not name `{expected}`; it names {core:?}"
            );
        }

        eprintln!(
            "== calibration over {} window(s): {admitted} in scope and every one of them \
             ADMITS the answer growth found; {} out of scope (growth's own answer climbs): \
             {}; {searched} decided unaided by the search{}; {verified} decoded, laid, \
             verified and right on the truth table; {injected} injection(s) caught; 1 sealed \
             window UNSAT with a {} group core; the clearance rule was itself put under \
             test on {clearance} window(s) ==",
            chosen.len(),
            outside.len(),
            if outside.is_empty() { "none".to_string() } else { outside.join(" ") },
            if undecided.is_empty() {
                String::new()
            } else {
                format!(
                    " ({} not decided inside the budget: {})",
                    undecided.len(),
                    undecided.join(" ")
                )
            },
            core.len()
        );
        assert!(
            searched > 0,
            "not one window was decided by the search unaided, so nothing here measures the \
             solver as a solver"
        );
        assert!(
            admitted > 0,
            "every window was out of the model's scope, so nothing here measures the encoding"
        );
    }

    // =====================================================================
    // The calibrated model, pointed at the two wedges it was calibrated to be
    // believed on.
    // =====================================================================

    /// Which arm of [`BodyFit::allowed`] refused a landing, and at which cell.
    ///
    /// "The body does not fit" is the uninformative address one level down
    /// again, and on a wedge it is the *only* address that matters: a landing
    /// whose body is refused never becomes a variable, so it can never appear
    /// in a core. The cells come from [`BodyFit::blockers`], which is the
    /// shipping reporter for the shipping predicate -- the three arms in the
    /// same order. All this adds is which arm a reported cell came from, by
    /// asking whether it is one of the body's own cells or one of the sockets'
    /// approaches, both of which are inputs to the fit rather than rules of the
    /// game.
    ///
    /// Returns whether at least one blocker is the **first** arm -- a cell of
    /// this gate's own body landing where a block already stands. That
    /// distinction decides how much a refusal is worth. Arm three is
    /// [`keep_out`], the *conservative plan-time shape* of `dust_reach`, and the
    /// spec's own open question is whether it is too strict; arm one is two
    /// blocks in one cell, which no relaxation of any clearance rule can permit.
    /// A wedge whose landings all fail on arm one is not a wedge that a looser
    /// clearance rule would open.
    fn why_the_body_would_not_stand(
        growth: &Growth,
        gate: usize,
        anchor: Anchor,
        facing: geometry::CellFacing,
    ) -> (bool, String) {
        let definition = growth.netlist.gates[gate].clone();
        let drivers = definition.inputs.clone();
        let pins: Vec<Anchor> = drivers.iter().map(|signal| growth.pins[signal]).collect();
        let (body_offsets, conductor_offsets, pin_offset) =
            compile::gate_footprint((0, 0, 0), &definition, facing);
        let mut sockets = Vec::with_capacity(drivers.len());
        let mut approaches = Vec::with_capacity(drivers.len());
        for input in 0..drivers.len() {
            let (socket, approach) = socket_and_approach(anchor, facing, input);
            sockets.push(socket);
            approaches.push(approach);
        }
        let fit = BodyFit {
            origin: anchor,
            cells: &body_offsets,
            conductors: &conductor_offsets,
            sockets: &sockets,
            approaches: &approaches,
            drivers: &drivers,
            pins: &pins,
            pin: shifted(anchor, pin_offset),
            escape: 0,
        };
        if fit.allowed(&growth.reservation) {
            return (false, "it stands -- this landing was not refused".to_string());
        }
        let mut blockers: BTreeSet<Anchor> = BTreeSet::new();
        fit.blockers(&growth.reservation, &mut blockers);
        if blockers.is_empty() {
            return (
                false,
                "refused with no blocking cell, which can only be the escape budget".to_string(),
            );
        }
        let body: BTreeSet<Anchor> =
            body_offsets.iter().map(|offset| shifted(anchor, *offset)).collect();
        let overlaps = blockers.iter().any(|cell| body.contains(cell));
        let said = blockers
            .iter()
            .map(|cell| {
                format!(
                    "({}, {}, {}) `{}` [{}] -- {}",
                    cell.x,
                    cell.y,
                    cell.z,
                    growth.reservation.owner(cell).unwrap_or("unclaimed"),
                    growth.what_stands_at(*cell),
                    if body.contains(cell) {
                        "BLOCK OVERLAP: a block of this gate's own body would have to land on it"
                    } else if approaches.contains(cell) {
                        "it is a socket's approach and it belongs to somebody else"
                    } else {
                        "keep_out: a foreign conductor beside one of this gate's conductors"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n            ");
        (overlaps, said)
    }

    /// **The two measured wedges, put to the windowed solver.**
    ///
    /// [`calibrate_the_windowed_solver`] establishes that the model's answers
    /// are worth believing, on `and4`, the one circuit whose growth completes,
    /// verifies and computes correctly. This is what that was for. Every method
    /// tried on this branch reports the wedges as "no landing" or "no safe local
    /// route", which says only *I did not find one*; the question the spec puts
    /// first is whether there is one.
    ///
    /// # Re-running it
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::put_the_measured_wedges_to_the_windowed_solver \
    ///   -- --ignored --nocapture
    /// ```
    ///
    /// | variable | default | meaning |
    /// |---|---|---|
    /// | `REDA_WEDGE_CIRCUIT` | `all` | `segment_a`, `full_adder`, or `all` |
    /// | `REDA_WEDGE_MARGINS` | `3,8,16,32,64` | the window sweep, in cells |
    /// | `REDA_WEDGE_REACH` | `96` | reachability depth cap; `0` asks for the exact bound |
    /// | `REDA_WEDGE_BUDGET` | `300000` | conflicts before a solve reports `Unknown` |
    ///
    /// The window is [`growth_window`] over the input nets' own seeds --
    /// **growth's own box, the one [`Growth::land`] searches in** -- so the
    /// sweep is over the same margins growth itself sweeps (`8,16,32,64`) with a
    /// tighter one added below them.
    ///
    /// # What it asserts
    ///
    /// Two things, and neither of them is the answer:
    ///
    /// 1. **The wedge is the documented one.** Gate and gate count are pinned
    ///    against the growth probe's own record, so a run that measures a
    ///    *different* state fails rather than quietly reporting about it.
    /// 2. **The control window comes back SAT and lays.** A model that says
    ///    UNSAT everywhere -- because a rule is over-strict, or because the
    ///    landing enumeration is broken -- would "prove" every wedge infeasible.
    ///    So the same construction is run on a gate this circuit's growth *did*
    ///    land, and it must find a landing, decode it and lay it through
    ///    [`Growth::land_solved`]. That is the rule-2 hook: it is what an UNSAT
    ///    at the wedge is worth anything against.
    ///
    /// The verdict itself is printed, never asserted. A harness that asserts its
    /// own conclusion is one somebody tunes until it agrees.
    ///
    /// # What an UNSAT here does and does not mean
    ///
    /// Stated before the numbers, because it is the whole reading of them.
    /// **Everything already laid is fixed.** The model re-decides one gate's
    /// placement and its input nets' wire, against a world of placed bodies and
    /// laid wire it may not move. So an UNSAT is a proof about *this state*: no
    /// legal placement of this gate exists anywhere in the window, at any
    /// orientation, with any route -- not a proof that the circuit is
    /// unroutable. Those are different claims and only the first is measured
    /// here.
    ///
    /// # What it measured
    ///
    /// `--release`, every default, whole run 12s. **Both wedges are UNSAT at
    /// every window size, and the UNSAT does not depend on the depth cap.**
    ///
    /// | circuit | wedge | margin 3 | 8 | 16 | 32 | 64 |
    /// |---|---|---|---|---|---|---|
    /// | `full_adder` | `g9` after 7/22 | UNSAT 0.00s | 0.00s | 0.01s | 0.03s | 0.10s |
    /// | `segment_a` | `g8` after 18/46 | UNSAT 0.00s | 0.00s | 0.00s | 0.03s | 0.11s |
    ///
    /// At margin 64 `segment_a`'s window is 91x129 cells: 97,924 variables,
    /// 1,142,945 clauses, built in 0.29s, decided in 0.11s, core minimised in
    /// 1.31s. Growth's own window sweep is `8,16,32,64`, so every margin growth
    /// searches at is covered and one below it.
    ///
    /// ## The core, and it is two groups
    ///
    /// At margins 8 and above, on both circuits:
    ///
    /// ```text
    /// `gate g8 stands in exactly one place`
    ///     + `every socket's approach is reached by its own net`
    /// ```
    ///
    /// At margin 3 the core is **one** group -- the window is smaller than the
    /// gate's own reach, every landing is refused before becoming a variable,
    /// and what is left is an empty at-least-one clause. That is the right
    /// answer with the uninformative core [`window_model`] warns about, and it
    /// is reported as its own row rather than averaged in.
    ///
    /// ## What the two groups mean at `(26, 1, 146)`, in cells
    ///
    /// Three facts, each measured separately and none of them needing the
    /// solver to be believed:
    ///
    /// 1. **The pin cannot be left.** Every in-plane step out of it is refused,
    ///    and the harness names the rule per cell -- for `segment_a`, `keep_out`
    ///    sees `g4` at `(24, 1, 146)`, `primitive:5` at `(28, 1, 146)` and `g2`
    ///    at `(26, 1, 144)`, and `(26, 1, 147)` is owned outright by
    ///    `primitive:7`. The **shipping 3D flood** ([`flood_from`], twelve
    ///    neighbours) reaches **1 cell of 1** at every margin, so this is not
    ///    the flat restriction: no climbing route exists to be missed.
    /// 2. **So only four landings could ever be served** -- the ones whose
    ///    socket approach *is* the pin, one per facing.
    /// 3. **All four are refused, and all four by BLOCK OVERLAP** -- a cell of
    ///    the new gate's body landing where another gate's block already stands
    ///    (`(28, 1, 146)` `primitive:5`, `(26, 1, 147)` and `(26, 1, 148)`
    ///    `primitive:7`, `(24, 1, 144)` `primitive:14`, `(24, 1, 146)` `g4`'s
    ///    socket approach). 4 of 4 on `segment_a` and 4 of 4 on `full_adder`.
    ///    **Not** by [`keep_out`], the conservative plan-time rule the spec's
    ///    section 8 keeps open against `dust_reach`: relaxing that rule would
    ///    not open either wedge.
    ///
    /// Growth's own funnel says the same thing from the other side -- `offered 4
    /// -> approaches met 4 -> body fits 0` -- so the solver and the shipping
    /// enumeration agree on the four, and what the solver adds is the *rest of
    /// the window*: 43,436 further landings at margin 64, every one of them
    /// refuted rather than untried.
    ///
    /// ## The controls
    ///
    /// Same model, same window function, margin 8, on gates of the same circuit
    /// that growth landed: `full_adder` **6 of 6 SAT and laid**, 3 of them
    /// laying wire (13, 59 and 65 cells); `segment_a` **8 of 8 SAT and laid**,
    /// including `g7`, a **three-net** window solved in 0.20s with routes
    /// 1+47+59 -- which is the first three-contending-net window measured
    /// anywhere on this branch.
    ///
    /// ## Rule 2, on this harness
    ///
    /// Injected, confirmed, reverted (2026-08-16, `--release`):
    ///
    /// - **Too loose: delete the domain filter's `keep_out` arm.** Control `g2`
    ///   of `full_adder` goes **red** -- `g2's answer would not lay, which is an
    ///   encoding bug: input 1: supplied route: (17, 1, 161) is refused by
    ///   `anchor_is_free_for``. This is the assertion that stops a too-permissive
    ///   model from "unwedging" anything.
    /// - **Over-strict: delete the domain filter's `neighbour == pin`
    ///   exemption.** Control `g2` goes UNSAT, and **nothing goes red**: `g4` and
    ///   `g5` still find 34- and 66-cell routes, because their nets have trees
    ///   to start from and do not need the exemption. So that exemption is
    ///   **not** under test here, and this null result is recorded rather than
    ///   claimed as coverage.
    /// - The wedges' own answer did **not** move under either injection. For the
    ///   too-loose one that is a null result too, and it is consistent with the
    ///   three facts above: the wedge does not turn on the domain filter.
    ///
    /// ## What was tried and did not unwedge
    ///
    /// `REDA_WEDGE_GATE=g8` re-decides `full_adder`'s **driver** -- the gate
    /// whose output pin is the one that ends up sealed -- rather than the
    /// consumer. SAT at margins 8, 16 and 32 (0.07s / 0.18s / 0.44s), landing it
    /// at `(14, 1, 164)` facing 0 with a 17-cell route, and it lays; growth then
    /// wedges at `g9` **again**. That is what the model should be expected to
    /// do: [`BodyFit`]'s `escape` is `0` here, exactly as it is in growth, so
    /// nothing in the model asks a gate to leave its own output net a way out
    /// either. The core points at placement, and this is the measurement that
    /// the model as built reproduces the same placement mistake.
    ///
    /// # What this does NOT establish
    ///
    /// - **That `segment_a` is infeasible.** Everything already laid is fixed.
    ///   The natural next model lets the window re-decide what is inside it, and
    ///   it is not built.
    /// - **That growth can be repaired by this.** No landing was found, so there
    ///   is no completion, no area against legacy's 23,220, and **no settle tick
    ///   count** -- `segment_a` still has no verified candidate to measure one
    ///   on.
    /// - **Anything about `seven_segment`**, whose wedge the growth probe
    ///   records as the same cell and taxonomy but which is not run here.
    #[test]
    #[ignore = "measurement harness: builds a CNF per window per margin on the two wedges"]
    fn put_the_measured_wedges_to_the_windowed_solver() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::build_single_segment_netlist;
        use std::time::Instant;

        let setting = |name: &str, fallback: &str| -> String {
            std::env::var(name).unwrap_or_else(|_| fallback.to_string())
        };
        let margins: Vec<i32> = setting("REDA_WEDGE_MARGINS", "3,8,16,32,64")
            .split(',')
            .map(|piece| piece.trim().parse().expect("a margin"))
            .collect();
        let reach_cap: usize = setting("REDA_WEDGE_REACH", "96").parse().expect("a depth");
        let budget: u64 = setting("REDA_WEDGE_BUDGET", "300000").parse().expect("a budget");
        let wanted = setting("REDA_WEDGE_CIRCUIT", "all");
        let control_cap: usize =
            setting("REDA_WEDGE_CONTROLS", "6").parse().expect("a count");
        // 8, because 8 is the smallest margin `Growth::land` itself ever
        // searches at (`windows` defaults to `8,16,32,64`). A control run
        // *tighter* than anything growth uses measures the margin and not the
        // instrument, and the first version of this used `margins[0]` -- 3 --
        // where `full_adder`'s `g5` came back UNSAT. At 8 the same window is SAT
        // in 0.10s with a 65-cell route that lays. That null result is recorded
        // rather than deleted: a margin below growth's own is a different
        // question, and asking it of a control answers nothing about the wedge.
        let control_margin: i32 =
            setting("REDA_WEDGE_CONTROL_MARGIN", "8").parse().expect("a margin");

        // Growth's own defaults, so the state being windowed is the state the
        // growth probe reports and not a variant of it.
        let growth_settings = GrowthSettings {
            order: "depth".to_string(),
            lambda: 0.5,
            windows: vec![8, 16, 32, 64],
            tries: 8,
            escape: 0,
            seed_pitch: 0,
            verbose: false,
            settle: true,
            rip: 0,
            seed: 0,
            rip_whole: false,
        };

        let (adder, adder_outputs) = build_full_adder_netlist();
        let (segment_a, segment_a_output) = build_single_segment_netlist(0);
        let cases = [
            ConditionCircuit {
                name: "full_adder",
                netlist: adder,
                inputs: &crate::circuits::full_adder::INPUT_NAMES[..],
                outputs: vec![adder_outputs["sum"].clone(), adder_outputs["cout"].clone()],
                expected: full_adder_expected,
            },
            ConditionCircuit {
                name: "segment_a",
                netlist: segment_a,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: vec![segment_a_output],
                expected: segment_a_expected,
            },
        ];
        // The growth probe's own record, pinned so a moved baseline fails here
        // rather than being silently measured instead.
        let documented: BTreeMap<&str, (&str, usize)> =
            [("full_adder", ("g9", 7)), ("segment_a", ("g8", 18))].into_iter().collect();

        let chosen: Vec<&ConditionCircuit> =
            cases.iter().filter(|case| wanted == "all" || wanted == case.name).collect();
        assert!(!chosen.is_empty(), "REDA_WEDGE_CIRCUIT names no circuit");

        eprintln!(
            "== the windowed solver on the measured wedges | margins {margins:?} \
             | depth cap {} | budget {budget} ==",
            if reach_cap == 0 { "exact".to_string() } else { reach_cap.to_string() }
        );
        eprintln!(
            "   everything already laid is FIXED. An UNSAT below is a proof about this \
             growth state, not about the circuit."
        );

        let mut verdicts: Vec<String> = Vec::new();
        for case in chosen {
            // --- what growth does, which is the state being asked about ---
            let mut oracle =
                Growth::seeded(&case.netlist, &growth_settings).expect("the seed layout");
            oracle.grow();
            let Some(wedge) = oracle.wedge.as_ref() else {
                panic!("{} did not wedge, so there is nothing here to attack", case.name)
            };
            let wedged_at = wedge.gate.clone();
            let placed = oracle.placed.iter().filter(|done| **done).count();
            let (expected_gate, expected_placed) = documented[case.name];
            assert_eq!(
                (wedged_at.as_str(), placed),
                (expected_gate, expected_placed),
                "{}'s wedge moved: the growth probe records {expected_gate} after \
                 {expected_placed}, and this run found {wedged_at} after {placed}. The \
                 measurement below would be about a different state.",
                case.name
            );
            eprintln!(
                "\n== {} : growth WEDGES at {wedged_at} ({} input(s)) after {placed}/{} gates ==",
                case.name,
                wedge.arity,
                case.netlist.gates.len()
            );
            eprintln!(
                "   growth's own funnel over windows {:?}: offered {} -> approaches met {} \
                 -> body fits {}",
                wedge.windows,
                wedge.funnel.offered,
                wedge.funnel.approaches_met,
                wedge.funnel.body_fits
            );

            // --- the same state again, with the wedged gate still unplaced ---
            //
            // `REDA_WEDGE_GATE` points the whole apparatus at a *different*
            // gate of the same circuit. It exists so the SAT arm below -- decode,
            // lay, grow the rest, verify, truth table -- can be exercised on
            // these circuits at all: on the wedge itself that arm never runs,
            // and an arm that never runs is an arm nobody has seen work.
            let target = match setting("REDA_WEDGE_GATE", "") {
                empty if empty.is_empty() => wedged_at.clone(),
                named => {
                    eprintln!(
                        "   REDA_WEDGE_GATE={named}: windowing {named} INSTEAD of the wedged \
                         {wedged_at}. Nothing below is about the wedge."
                    );
                    named
                }
            };
            let (growth, gate) =
                growth_paused_before(&case.netlist, &growth_settings, &target)
                    .unwrap_or_else(|why| panic!("could not pause before {}: {why}", wedge.gate));
            let drivers = case.netlist.gates[gate].inputs.clone();
            let seeds: Vec<BTreeSet<Anchor>> = drivers
                .iter()
                .map(|signal| {
                    let pin = growth.pins[signal];
                    growth.trees.get(signal).cloned().unwrap_or_default().seeds(pin)
                })
                .collect();
            let all: Vec<Anchor> = seeds.iter().flatten().copied().collect();

            // --- the seal, named rule by rule ---
            //
            // `why_not_in_domain` asks the model's domain filter one arm at a
            // time, and each arm of it is an arm of `anchor_is_free_for`. This
            // is the readable half of a core: the core says *which two groups*
            // cannot hold together, and this says *which cell and which rule*
            // underneath.
            let widest = growth_window(&all, *margins.iter().max().expect("a margin"));
            for (input, signal) in drivers.iter().enumerate() {
                let pin = growth.pins[signal];
                eprintln!(
                    "   net {signal} drives input {input}; its pin is ({}, {}, {}) and it \
                     holds {} cell(s) on the gate plane",
                    pin.x,
                    pin.y,
                    pin.z,
                    seeds[input].iter().filter(|cell| cell.y == PLANNER_Y).count()
                );
                for step_to in plane_neighbours(pin) {
                    eprintln!(
                        "     ({}, {}, {}): {}",
                        step_to.x,
                        step_to.y,
                        step_to.z,
                        why_not_in_domain(&growth, signal, pin, step_to, widest)
                    );
                }
            }

            // --- the sweep ---
            let mut answers: Vec<String> = Vec::new();
            for &margin in &margins {
                let window = growth_window(&all, margin);
                eprintln!(
                    "   -- margin {margin}: window ({}, {}, {})..({}, {}, {}) --",
                    window.0.x, window.0.y, window.0.z, window.1.x, window.1.y, window.1.z
                );

                // **The flat restriction, closed for this window rather than
                // declared.** The model gives new wire the gate plane only, so
                // one reading of an UNSAT is "the answer climbs". The shipping
                // flood is 3D -- `neighbours` is twelve cells -- so running it
                // here says whether a climbing route exists to be missed. It is
                // a call into the same function `Growth::land` floods with.
                for (input, signal) in drivers.iter().enumerate() {
                    let pin = growth.pins[signal];
                    let field = flood_from(
                        &seeds[input],
                        pin,
                        None,
                        signal,
                        &growth.reservation,
                        window,
                    );
                    eprintln!(
                        "      the shipping 3D flood for net {signal} reaches {} cell(s) from \
                         the {} it starts with{}",
                        field.travelled.len(),
                        seeds[input].len(),
                        if field.travelled.len() == seeds[input].len() {
                            " -- it cannot take ONE legal step, climbing or flat, so the \
                             flat restriction costs this window nothing"
                        } else {
                            ""
                        }
                    );
                }

                let started = Instant::now();
                let model = window_model(&growth, gate, window, reach_cap);
                let built = started.elapsed().as_secs_f64();
                eprintln!("      built in {built:.2}s | {}", model.summary());

                let started = Instant::now();
                let outcome = model.cnf.solve(budget);
                let solved = started.elapsed().as_secs_f64();
                report_solve(&format!("margin {margin}"), &model, &outcome, solved);

                match &outcome {
                    Outcome::Unknown => {
                        answers.push(format!("margin {margin}: UNKNOWN in {solved:.2}s"));
                        eprintln!(
                            "      the search did not decide this window inside {budget} \
                             conflicts. **That is a budget, not an infeasibility.**"
                        );
                    }
                    Outcome::Unsat => {
                        // **What this UNSAT is bounded by, said once and
                        // plainly.** `report_solve` prints "bounded: capped path
                        // length" whenever the depth cap is below the exact
                        // bound, which is the right default. It is too weak
                        // here: when every input net can reach no cell of the
                        // window *at any* path length, the cap is not what
                        // decided anything, and that distinction is the whole
                        // difference between a budget and an answer.
                        let capless = model.nets.iter().all(NetWindow::stranded_entirely);
                        if capless {
                            eprintln!(
                                "      this UNSAT does NOT depend on the depth cap: every input \
                                 net is stranded outright, so raising the cap changes nothing"
                            );
                        }
                        let started = Instant::now();
                        let core = model.cnf.core(budget);
                        let cored = started.elapsed().as_secs_f64();
                        match &core {
                            Some(groups) => {
                                eprintln!(
                                    "      CORE ({} group(s), minimised by deletion in \
                                     {cored:.2}s): {}",
                                    groups.len(),
                                    groups
                                        .iter()
                                        .map(|&group| format!(
                                            "`{}`",
                                            model.cnf.group_name(group)
                                        ))
                                        .collect::<Vec<_>>()
                                        .join(" + ")
                                );
                                answers.push(format!(
                                    "margin {margin}: UNSAT in {solved:.2}s, {} group core{}",
                                    groups.len(),
                                    if capless { ", cap-independent" } else { ", CAPPED" }
                                ));
                            }
                            None => {
                                eprintln!(
                                    "      no core: a solve inside the deletion loop exceeded \
                                     its budget, so nothing minimal is claimed"
                                );
                                answers.push(format!(
                                    "margin {margin}: UNSAT in {solved:.2}s, no core"
                                ));
                            }
                        }
                        eprintln!(
                            "      landings: {} offered, {} refused by BodyFit before becoming \
                             variables, {} became variables",
                            model.offered,
                            model.body_rejected,
                            model.landings.len()
                        );
                        // The landings a core can never mention: the ones whose
                        // socket approach is a sealed net's own pin -- the only
                        // ones any route could serve -- and which never became
                        // variables because the body was refused.
                        for (input, signal) in drivers.iter().enumerate() {
                            let pin = growth.pins[signal];
                            let refused: Vec<(Anchor, geometry::CellFacing)> = model
                                .body_rejected_at
                                .iter()
                                .copied()
                                .filter(|&(anchor, facing)| {
                                    socket_and_approach(anchor, facing, input).1 == pin
                                })
                                .collect();
                            if refused.is_empty() {
                                continue;
                            }
                            eprintln!(
                                "      the {} landing(s) that would put input {input}'s socket \
                                 approach ON net {signal}'s pin ({}, {}, {}) -- the only ones a \
                                 net that cannot move could be served by -- and why each body \
                                 was refused:",
                                refused.len(),
                                pin.x,
                                pin.y,
                                pin.z
                            );
                            let mut overlapping = 0usize;
                            let total = refused.len();
                            for (anchor, facing) in refused {
                                let (overlaps, why) =
                                    why_the_body_would_not_stand(&growth, gate, anchor, facing);
                                overlapping += usize::from(overlaps);
                                eprintln!(
                                    "        anchor ({}, {}, {}) facing {}:\n            {why}",
                                    anchor.x,
                                    anchor.y,
                                    anchor.z,
                                    facing.index()
                                );
                            }
                            // **How much this wedge owes to the conservative
                            // rule, and how much to the geometry.** `keep_out`
                            // is `dust_reach`'s conservative plan-time shape and
                            // the spec's section 8 keeps open whether it is too
                            // strict. A landing refused because a block of this
                            // gate would be written where another gate's block
                            // already stands owes nothing to that question.
                            eprintln!(
                                "      of those {total}, **{overlapping} are refused by BLOCK \
                                 OVERLAP** -- a cell of this gate's body landing where a block \
                                 already stands{}",
                                if overlapping == total {
                                    ". Every one of them, and on both circuits the ANCHOR CELL \
                                     ITSELF is already occupied by another gate's block. But \
                                     that does NOT make the wedge independent of `keep_out`: \
                                     the four landings are exhaustive only *given* the pin is \
                                     sealed, and the seal is exact in-plane and conservative \
                                     out of it -- the three flat steps out are refused by \
                                     same-layer offenders `dust_reach` joins unconditionally, \
                                     the three climbing steps only by climb/descend arms. \
                                     Measured: loosening `keep_out` to the unconditional arm \
                                     grows full_adder 22/22 -- and that circuit FAILS \
                                     verification with a cross-net short (dust at (36,2,161) \
                                     on net g16 electrically joined to g20's network), which \
                                     is the hazard the climb arm exists to prevent. So the \
                                     honest statement is that the wedge depends on the \
                                     conservatism, and the one relaxation measurable today \
                                     trades the wedge for a wrong circuit."
                                } else {
                                    ", and the rest only by the conservative clearance rules."
                                }
                            );
                        }
                    }
                    Outcome::Sat(assignment) => {
                        // Rule 6: routes without verifies is worth nothing. A
                        // SAT here is only an answer once it has been decoded,
                        // laid by `Growth::lay`, grown out, verified and put
                        // through the truth table.
                        let solution = decode(&model, assignment)
                            .unwrap_or_else(|why| panic!("SAT that will not decode: {why}"));
                        eprintln!(
                            "      decoded: ({}, {}, {}) facing {} | routes {} | {} stranded",
                            solution.anchor.x,
                            solution.anchor.y,
                            solution.anchor.z,
                            solution.facing.index(),
                            solution
                                .routes
                                .values()
                                .map(|path| path.len().to_string())
                                .collect::<Vec<_>>()
                                .join("+"),
                            solution.stranded
                        );
                        let (mut fresh, gate_again) =
                            growth_paused_before(&case.netlist, &growth_settings, &target)
                                .expect("the same pause again");
                        assert_eq!(gate, gate_again, "the same gate on the same state");
                        match fresh.land_solved(
                            gate_again,
                            solution.anchor,
                            solution.facing,
                            &solution.routes,
                            window,
                        ) {
                            Err(why) => {
                                eprintln!(
                                    "      **the answer would not lay: {why}** -- a solver \
                                     answer `Growth::lay` refuses is an encoding bug in the \
                                     model, not a result"
                                );
                                answers.push(format!(
                                    "margin {margin}: SAT in {solved:.2}s but WOULD NOT LAY"
                                ));
                            }
                            Ok(()) => {
                                fresh.grow_stopping_before(None);
                                let after =
                                    fresh.placed.iter().filter(|done| **done).count();
                                let gates = case.netlist.gates.len();
                                let standing: Vec<Anchor> = fresh
                                    .nodes
                                    .iter()
                                    .flatten()
                                    .map(|node| node.anchor)
                                    .collect();
                                let (width, depth, area) = anchor_box(&standing);
                                eprintln!(
                                    "      the answer LAYS, and growth carries on from it to \
                                     {after}/{gates} gates | anchor box {width}x{depth}={area}"
                                );
                                if let Some(next) = &fresh.wedge {
                                    eprintln!(
                                        "      and then wedges at {} -- so this is a landing, \
                                         not a finished circuit",
                                        next.gate
                                    );
                                    answers.push(format!(
                                        "margin {margin}: SAT in {solved:.2}s, laid, \
                                         {after}/{gates} then wedged at {}",
                                        next.gate
                                    ));
                                    continue;
                                }
                                let Some(candidate) = fresh.candidate() else {
                                    eprintln!("      grew every gate and built no candidate");
                                    answers.push(format!(
                                        "margin {margin}: SAT, grew out, no candidate"
                                    ));
                                    continue;
                                };
                                match realise_and_verify(
                                    &candidate,
                                    &case.netlist,
                                    candidate_world_size(&candidate),
                                ) {
                                    Err(error) => {
                                        eprintln!(
                                            "      COMPLETES AND DOES NOT VERIFY: {error}"
                                        );
                                        answers.push(format!(
                                            "margin {margin}: SAT, completed, FAILS \
                                             verify_candidate"
                                        ));
                                    }
                                    Ok(realised) => {
                                        let blocks = (0..realised.world.cells().len())
                                            .filter(|&flat| {
                                                let (x, y, z) = realised.world.decode(flat);
                                                realised.world.get(x, y, z).kind
                                                    != crate::redstone::world::block::BlockKind::Air
                                            })
                                            .count();
                                        match worst_settle_and_truth(
                                            &realised,
                                            case.inputs,
                                            &case.outputs,
                                            case.expected,
                                        ) {
                                            Err(error) => {
                                                eprintln!(
                                                    "      VERIFIES and COMPUTES THE WRONG \
                                                     FUNCTION: {error}"
                                                );
                                                answers.push(format!(
                                                    "margin {margin}: SAT, verifies, WRONG \
                                                     truth table"
                                                ));
                                            }
                                            Ok((worst, at, transitions)) => {
                                                eprintln!(
                                                    "      VERIFIES, {blocks} blocks | truth \
                                                     table Ok over {transitions} ordered \
                                                     transitions | worst settle {worst} ticks \
                                                     at {at} | anchor box {area}"
                                                );
                                                answers.push(format!(
                                                    "margin {margin}: SAT, COMPLETE, verifies, \
                                                     {blocks} blocks, {worst} ticks, area {area}"
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // --- the controls: the same construction on gates growth landed ---
            //
            // Without these, "UNSAT" is a word this harness could produce with a
            // broken landing enumeration or one over-strict rule, on every
            // window it was ever pointed at. So the same model, the same window
            // function and the same margin are run on gates of this same circuit
            // whose landing growth *found*, and at least one has to come back
            // SAT and lay through `Growth::lay`.
            //
            // **The scope filter is declared, not discovered.** The model gives
            // new wire the gate plane only ([`window_model`]), so a gate whose
            // input nets already hold cells off that plane has no flat answer to
            // find and is out of scope by the restriction rather than by any
            // property of the wedge. It is skipped with its off-plane count
            // printed, because "skipped" and "failed" must not look alike.
            //
            // The first control this harness ever ran was a single gate -- the
            // last one in netlist order that growth placed -- and it came back
            // **UNSAT**, on `segment_a`'s `g35`, whose nets `g5` and `g6` hold
            // 28 and 21 seed cells off the plane. That is the restriction
            // biting, and it is recorded here rather than deleted: a control
            // chosen without the scope filter measures the flat restriction and
            // not the instrument.
            let controls: Vec<String> = (0..case.netlist.gates.len())
                .filter(|&other| oracle.placed[other])
                .map(|other| case.netlist.gates[other].output.clone())
                .collect();
            eprintln!(
                "   -- controls: gates this growth landed, in netlist order, at margin \
                 {control_margin} (growth's own smallest), capped at {control_cap} in-scope --"
            );
            let mut in_scope = 0usize;
            let mut control_sat = 0usize;
            let mut control_laid = 0usize;
            let mut control_wired = 0usize;
            let mut control_lines: Vec<String> = Vec::new();
            for control in &controls {
                // The stopping rule, stated rather than tuned: `control_cap`
                // in-scope windows, **and at least one of them a window that
                // needed wire**. A gate whose socket lands straight onto its
                // driver's pin decides no route at all, so a control set made
                // only of those exercises the landing enumeration and nothing
                // of the connectivity encoding -- and `segment_a`'s first six
                // landed gates are all of that shape.
                if in_scope >= control_cap && control_wired > 0 {
                    break;
                }
                let (control_growth, control_gate) =
                    growth_paused_before(&case.netlist, &growth_settings, control)
                        .unwrap_or_else(|why| panic!("could not pause before {control}: {why}"));
                let mut off_plane = 0usize;
                let control_all: Vec<Anchor> = case.netlist.gates[control_gate]
                    .inputs
                    .iter()
                    .flat_map(|signal| {
                        let pin = control_growth.pins[signal];
                        control_growth
                            .trees
                            .get(signal)
                            .cloned()
                            .unwrap_or_default()
                            .seeds(pin)
                    })
                    .inspect(|cell| off_plane += usize::from(cell.y != PLANNER_Y))
                    .collect();
                if off_plane > 0 {
                    eprintln!(
                        "      {control}: OUT OF SCOPE -- its input nets hold {off_plane} \
                         cell(s) off the gate plane, which this model cannot grow from"
                    );
                    continue;
                }
                in_scope += 1;
                let control_window = growth_window(&control_all, control_margin);
                let control_model =
                    window_model(&control_growth, control_gate, control_window, reach_cap);
                let started = Instant::now();
                let control_outcome = control_model.cnf.solve(budget);
                let control_time = started.elapsed().as_secs_f64();
                let Outcome::Sat(assignment) = &control_outcome else {
                    eprintln!(
                        "      {control}: {} in {control_time:.2}s | {}",
                        match control_outcome {
                            Outcome::Unsat => "UNSAT",
                            _ => "UNKNOWN",
                        },
                        control_model.summary()
                    );
                    control_lines.push(format!("{control} {control_outcome:?}"));
                    continue;
                };
                control_sat += 1;
                let solution = decode(&control_model, assignment)
                    .unwrap_or_else(|why| panic!("{control}'s SAT will not decode: {why}"));
                let (mut control_growth, control_gate) =
                    growth_paused_before(&case.netlist, &growth_settings, control)
                        .expect("the same pause again");
                match control_growth.land_solved(
                    control_gate,
                    solution.anchor,
                    solution.facing,
                    &solution.routes,
                    control_window,
                ) {
                    Ok(()) => {
                        control_laid += 1;
                        let wired =
                            solution.routes.values().any(|path| path.len() > 1);
                        control_wired += usize::from(wired);
                        eprintln!(
                            "      {control}: SAT in {control_time:.2}s, decoded ({}, {}, {}) \
                             facing {}, routes {}, and it LAYS through `Growth::lay`",
                            solution.anchor.x,
                            solution.anchor.y,
                            solution.anchor.z,
                            solution.facing.index(),
                            solution
                                .routes
                                .values()
                                .map(|path| path.len().to_string())
                                .collect::<Vec<_>>()
                                .join("+")
                        );
                        control_lines.push(format!("{control} SAT+laid"));
                    }
                    Err(why) => {
                        // Not a soft failure. A model answer the shipping `lay`
                        // refuses is an encoding bug, and it is the exact defect
                        // that would make an UNSAT at the wedge worthless.
                        panic!("{control}'s answer would not lay, which is an encoding bug: {why}")
                    }
                }
            }
            eprintln!(
                "      controls: {in_scope} in scope of {} landed, {control_sat} SAT, \
                 {control_laid} laid, {control_wired} of those needed wire",
                controls.len()
            );
            assert!(
                control_laid > 0,
                "not one control window on {} came back SAT and laid ({}). This model cannot \
                 find a landing on a gate growth itself landed, so its UNSAT on the wedge is \
                 worth nothing.",
                case.name,
                control_lines.join(", ")
            );
            assert!(
                control_wired > 0,
                "every control window on {} was decided without laying a single cell of wire \
                 ({}), so the connectivity encoding -- the half an UNSAT at a sealed pin turns \
                 on -- was never exercised on this circuit at all.",
                case.name,
                control_lines.join(", ")
            );

            verdicts.push(format!(
                "{}: {} || controls {control_laid}/{in_scope} SAT and laid, {control_wired} \
                 with wire",
                case.name,
                answers.join(" | ")
            ));
        }

        eprintln!("\n== what the windowed solver concluded ==");
        for verdict in &verdicts {
            eprintln!("   {verdict}");
        }
    }

    /// What a rip-up budget buys, and what it costs the circuits it buys
    /// nothing for.
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::what_a_rip_up_budget_buys_and_what_it_costs \
    ///   -- --ignored --nocapture
    /// ```
    ///
    /// This is the measurement behind [`TRIAL_RIP_UP_ROUNDS`]. `compile` tries
    /// the planner on every netlist, so the price of the trial is paid by every
    /// circuit that ends up falling back, and picking that budget by taste
    /// rather than by measurement is how a compiler gets half a minute slower
    /// on the circuits that gain nothing from the change.
    ///
    /// It reports, per circuit: how long relaxation and `snap` take, then --
    /// at each budget -- whether the router finished, how many rounds it
    /// actually spent, and how long it took. **Rounds spent is the load-bearing
    /// column.** `route_every_net` returns on the first round that finishes, so
    /// a circuit that routes gets a bit-identical answer from every budget at
    /// or above what it spends; only the circuits that never finish care how
    /// big the budget is, and they are exactly the ones paying for it.
    ///
    /// Uses [`harvest_routing`], which is `route_every_net` line for line with
    /// counters around it, for the reason that function's own doc gives: a
    /// probe that measures a different router measures nothing.
    ///
    /// Asserts nothing. Wall clock is machine- and load-dependent, and a test
    /// that goes red when the machine is busy is a test people learn to ignore.
    /// What it produces is a table somebody re-runs before changing the
    /// constant.
    #[test]
    #[ignore = "measurement harness: asserts nothing, six circuits at six budgets, minutes"]
    fn what_a_rip_up_budget_buys_and_what_it_costs() {
        use std::time::Instant;

        for (name, netlist) in crate::compile::tests::the_six_condition_netlists() {
            let started = Instant::now();
            let placed = match relaxed_placement(&netlist, &PortPlacements::default(), SHIPPING_AXES)
            {
                Ok(placement) => placement,
                Err(error) => {
                    // A placement failure is not a routing failure, and no
                    // budget touches it. `verilog:seven_segment` lands here.
                    eprintln!(
                        "{name}: place ERR after {:.2}s -- no rip-up budget reaches this: {error}",
                        started.elapsed().as_secs_f64()
                    );
                    continue;
                }
            };
            let snapped = match relax::snap(&placed) {
                Ok(snapped) => snapped,
                Err(error) => {
                    eprintln!("{name}: snap ERR {error}");
                    continue;
                }
            };
            let candidate = candidate_from_snapped(&netlist, &PortPlacements::default(), &snapped);
            let mut line = format!("{name}: place {:.2}s", started.elapsed().as_secs_f64());
            for rounds in [5usize, TRIAL_RIP_UP_ROUNDS, 16, 32, RIP_UP_ROUNDS] {
                let started = Instant::now();
                let harvest = harvest_routing(candidate.clone(), &netlist, rounds);
                line.push_str(&format!(
                    " | {rounds}r {} spent={} {:.2}s",
                    if harvest.routed.is_some() { "ok" } else { "--" },
                    harvest.rounds,
                    started.elapsed().as_secs_f64()
                ));
            }
            eprintln!("{line}");
        }
    }
    // =====================================================================
    // The arithmetic of a component-aware keep-out: what it would cost, and
    // what it would buy.
    // =====================================================================
    //
    // `keep_out` takes a coordinate. `compile::energising::energises` takes a
    // block kind and a facing, and answers out of the two derived artifacts.
    // Everything below is the difference between those two answers, counted --
    // on the two measured wedges, on the 41 recorded extra edges, per circuit
    // and per component. **Nothing here changes what the router does**; the one
    // function it would change, `anchor_is_free_for`, is not touched.
    //
    // Read the four numbers with the one artifact line that governs all of
    // them: `docs/derived/dust-join-relation.md`'s summary says **12 of
    // `keep_out`'s 12 cells really join** for a dust cell on a stone floor. So
    // for dust -- which is most of what a route lays -- `keep_out` is not
    // conservative, it is exact, and no amount of component-awareness can make
    // it smaller. The whole saving, and the whole cost, is in the cells that
    // are *not* dust.

    use crate::compile::energising::{self, Offset};
    use crate::redstone::world::block::{BlockKind, Facing};
    use std::collections::HashMap;

    const EXTRAS_ARTIFACT: &str = include_str!("../../docs/derived/realised-graph-extras.md");

    /// What one cell's derived keep-out halo is, and where the answer came
    /// from.
    struct Derived {
        /// Everything the artifacts measure this block to reach, both hops.
        measured: BTreeSet<Offset>,
        /// The same, plus every offset no rig in the artifact could ask about
        /// -- a diode's rear, where the rig's own feed has to stand. This is
        /// what a reservation would actually have to use.
        conservative: BTreeSet<Offset>,
        /// Which relation answered.
        via: &'static str,
    }

    /// The derived halo of one realised block.
    ///
    /// Two relations, not one, and the split is the whole result. Dust against
    /// dust has its own artifact and its own closed form; everything else is
    /// the energising range. Asking `energises` about dust would be wrong in
    /// the flattering direction -- Table 1's `dust` row reads `x` in the column
    /// its own feed occupies, so it would answer three cells for something the
    /// dust-join artifact measures at twelve.
    fn derived_halo(state: &BlockState) -> Derived {
        if state.kind == BlockKind::RedstoneWire {
            let joins = energising::dust_join_offsets();
            return Derived {
                measured: joins.clone(),
                conservative: joins,
                via: "dust-join",
            };
        }
        let range = energising::energises(state.kind, state.facing);
        Derived {
            measured: range.measured(),
            conservative: range.conservative(),
            via: "energises",
        }
    }

    fn offset_between(from: Anchor, to: Anchor) -> Offset {
        (to.x - from.x, to.y - from.y, to.z - from.z)
    }

    /// `keep_out`'s answer as offsets, so it can be differenced against a
    /// derived halo. Taken from the function itself rather than written out,
    /// so a change to `keep_out` moves every count below.
    fn keep_out_offsets() -> BTreeSet<Offset> {
        let origin = Anchor { x: 0, y: 0, z: 0 };
        keep_out(origin)
            .into_iter()
            .map(|cell| offset_between(origin, cell))
            .collect()
    }

    /// What each cell of a placed gate body actually holds.
    ///
    /// `compile::gate_footprint` answers which cells a body occupies and which
    /// of them conduct; the question here is which *kind* of thing conducts,
    /// which is exactly the axis `keep_out` cannot see. Same scratch world,
    /// same three writes after it, so a cell this reports is a cell that one
    /// reports.
    fn body_states(
        origin: Anchor,
        gate: &Gate,
        facing: geometry::CellFacing,
    ) -> BTreeMap<Anchor, BlockState> {
        let mut scratch = World::new(64, 8, 64);
        let shifted_origin = (32, 1, 32);
        let cell = if gate.is_merge() {
            compile::place_merge_gate(&mut scratch, shifted_origin, gate.inputs.len(), facing)
        } else {
            compile::place_nor_gate(&mut scratch, shifted_origin, gate.inputs.len(), facing)
        };
        let torch = Position::new(
            shifted_origin.0 + cell.output_offset.0,
            shifted_origin.1 + cell.output_offset.1,
            shifted_origin.2 + cell.output_offset.2,
        );
        let pin = torch.offset(geometry::output_direction(facing));
        scratch.set(pin.x, pin.y, pin.z, compile::dust());
        for direction in geometry::input_directions(facing)
            .iter()
            .take(gate.inputs.len())
        {
            let socket = Position::new(shifted_origin.0, shifted_origin.1, shifted_origin.2)
                .offset(*direction);
            scratch.set(socket.x, socket.y, socket.z, compile::stone());
        }
        let mut states = BTreeMap::new();
        for flat in 0..scratch.cells().len() {
            let (x, y, z) = scratch.decode(flat);
            let state = scratch.get(x, y, z);
            if state.kind == BlockKind::Air {
                continue;
            }
            states.insert(
                Anchor {
                    x: origin.x + (x - shifted_origin.0),
                    y: origin.y + (y - shifted_origin.1),
                    z: origin.z + (z - shifted_origin.2),
                },
                state.clone(),
            );
        }
        states
    }

    /// The same, for a primary input's lever, through the same writer the
    /// emitter uses.
    fn lever_states(anchor: Anchor, facing: geometry::CellFacing) -> BTreeMap<Anchor, BlockState> {
        let mut scratch = World::new(16, 4, 16);
        let home = Position::new(8, 1, 8);
        compile::place_primary_input(&mut scratch, home, facing);
        let mut states = BTreeMap::new();
        for flat in 0..scratch.cells().len() {
            let (x, y, z) = scratch.decode(flat);
            let state = scratch.get(x, y, z);
            if state.kind == BlockKind::Air {
                continue;
            }
            states.insert(
                Anchor {
                    x: anchor.x + (x - home.x),
                    y: anchor.y + (y - home.y),
                    z: anchor.z + (z - home.z),
                },
                state.clone(),
            );
        }
        states
    }

    /// What stands in every claimed cell of a growth state.
    ///
    /// Bodies and levers are exact -- they are written by the same functions
    /// realisation writes them with. **Laid wire is not**: which cells of a
    /// path become repeaters is decided by `realise_branch_from`'s strength
    /// budget, which runs after `reserve_path` and therefore after the state
    /// this reads. Every laid cell is recorded as dust, and
    /// `the_energising_arithmetic` prints that as a caveat rather than
    /// swallowing it.
    fn growth_states(growth: &Growth) -> BTreeMap<Anchor, BlockState> {
        let mut states = BTreeMap::new();
        let gates = growth.netlist.gates.len();
        for (index, node) in growth.nodes.iter().enumerate() {
            let Some(node) = node else {
                continue;
            };
            if index < gates {
                states.extend(body_states(
                    node.anchor,
                    &growth.netlist.gates[index],
                    growth.facings[index],
                ));
            } else {
                states.extend(lever_states(node.anchor, growth.facings[index]));
            }
        }
        for laid in &growth.laid {
            for &cell in &laid.path {
                states.entry(cell).or_insert_with(compile::dust);
            }
        }
        states
    }

    /// How the derivation reads one ordered pair: an offender that already
    /// stands at `offender`, and a cell `target` a route wants.
    ///
    /// Returns the reason the pair is refused, or `None` if the derivation has
    /// nothing against it.
    fn derived_verdict(
        offender: Anchor,
        state: &BlockState,
        target: Anchor,
        reservation: &Reservation,
    ) -> Option<String> {
        let halo = derived_halo(state);
        let offset = offset_between(offender, target);
        // **The reverse direction, and it comes first because an
        // energising-only rule is blind to it.** A gate's support block and a
        // gate's socket are inert stone: `energises` asked of them answers
        // nothing, so a rule built from it alone would hand the cell back. What
        // actually happens is measured in
        // `energising::tests::a_powered_dust_against_a_gate_support_puts_its_
        // torch_out`: a powered dust one cardinal step from a support turns
        // that gate's torch off.
        if state.kind == BlockKind::Solid
            && matches!(
                reservation.occupancy(&offender),
                Some(Occupancy::GateConductor)
            )
        {
            let cardinal = offset.1 == 0 && offset.0.abs() + offset.2.abs() == 1;
            if cardinal && energising::BESIDE_A_SUPPORT_IS_READ {
                return Some(
                    "MECHANISM 4 (measured): a powered dust one cardinal step from a \
                     gate's support block turns that gate's torch off. Not in the \
                     energising range at all -- the offender is inert and the newcomer \
                     is the emitter."
                        .to_string(),
                );
            }
            return Some(format!(
                "NOT MEASURED at offset {offset:?}: an inert `GateConductor` cell, kept \
                 clear by one of the four rules `Occupancy::GateConductor` names, none \
                 of which the energising range expresses. Conservatively kept."
            ));
        }
        if !halo.measured.contains(&offset) {
            if halo.conservative.contains(&offset) {
                return Some(format!(
                    "UNMEASURABLE: the artifact's rig cannot ask a {:?} about {offset:?} \
                     (its own feed stands there), so a safe rule keeps it",
                    state.kind
                ));
            }
            return None;
        }
        if state.kind != BlockKind::RedstoneWire {
            return Some(format!(
                "{:?} energises {offset:?} ({})",
                state.kind, halo.via
            ));
        }
        // Dust against dust. Same layer is unconditional; a vertical pair is
        // apart only when the lid is committed stone, which is exactly
        // `join_lid` plus `stone_owner` -- the rule this tree already has and
        // already refuses to ship, for reasons that have nothing to do with
        // component-awareness.
        if energising::unconditional_dust_joins().contains(&offset) {
            return Some(
                "dust-join, SAME LAYER and UNCONDITIONAL: no content of any cell anywhere \
                 separates this pair"
                    .to_string(),
            );
        }
        match join_lid(target, offender) {
            Some(lid) if reservation.stone_owner(&lid).is_some() => None,
            Some(lid) => Some(format!(
                "dust-join, vertical: the lid at ({}, {}, {}) is not committed stone",
                lid.x, lid.y, lid.z
            )),
            None => Some("dust-join".to_string()),
        }
    }

    // ---------------------------------------------------------------
    // 1 -- the wedges
    // ---------------------------------------------------------------

    /// One cell a sealed pin cannot step into, with both verdicts.
    struct SealedStep {
        cell: Anchor,
        today: String,
        derived: Vec<String>,
        opens: bool,
    }

    /// Everything measured about one sealed pin.
    fn seal_report(
        growth: &Growth,
        states: &BTreeMap<Anchor, BlockState>,
        pin: Anchor,
        signal: &str,
    ) -> Vec<SealedStep> {
        let mut steps = Vec::new();
        for cell in neighbours(pin) {
            let mut today = String::new();
            let mut derived: Vec<String> = Vec::new();
            let mut opens = true;

            if let Some(owner) = growth.reservation.owner(&cell) {
                today = format!(
                    "OCCUPIED by `{owner}` [{}]",
                    growth.what_stands_at(cell)
                );
                // A cell another block already stands in is not a clearance
                // question at all. No relaxation of any keep-out rule puts two
                // blocks in one cell.
                derived.push("BLOCK OVERLAP -- outside every clearance rule".to_string());
                opens = false;
                steps.push(SealedStep { cell, today, derived, opens });
                continue;
            }
            let below = Anchor { y: cell.y - 1, ..cell };
            if let Some(owner) = growth.reservation.conductor_owner(&below) {
                today = format!(
                    "its floor at ({}, {}, {}) conducts for `{owner}`",
                    below.x, below.y, below.z
                );
                if let Some(state) = states.get(&below) {
                    if let Some(why) = derived_verdict(below, state, cell, &growth.reservation) {
                        derived.push(format!("({}, {}, {}) {why}", below.x, below.y, below.z));
                        opens = false;
                    }
                } else {
                    derived.push("floor conductor with no realised state recorded".to_string());
                    opens = false;
                }
            }
            for neighbour in keep_out(cell) {
                if neighbour == pin {
                    continue;
                }
                let Some(owner) = growth.reservation.conductor_owner(&neighbour) else {
                    continue;
                };
                if owner == signal {
                    continue;
                }
                let offset = offset_between(cell, neighbour);
                if today.is_empty() {
                    today = format!(
                        "keep_out sees `{owner}`'s conductor at ({}, {}, {}), offset {offset:?}",
                        neighbour.x, neighbour.y, neighbour.z
                    );
                }
                let Some(state) = states.get(&neighbour) else {
                    derived.push(format!(
                        "({}, {}, {}) `{owner}` -- conductor with no realised state recorded, \
                         so no derived verdict",
                        neighbour.x, neighbour.y, neighbour.z
                    ));
                    opens = false;
                    continue;
                };
                let taxonomy = growth.what_stands_at(neighbour);
                match derived_verdict(neighbour, state, cell, &growth.reservation) {
                    Some(why) => {
                        derived.push(format!(
                            "({}, {}, {}) `{owner}` [{taxonomy}] {:?}: {why}",
                            neighbour.x, neighbour.y, neighbour.z, state.kind
                        ));
                        opens = false;
                    }
                    None => derived.push(format!(
                        "({}, {}, {}) `{owner}` [{taxonomy}] {:?}: the derivation has NOTHING \
                         against this pair at offset {offset:?}",
                        neighbour.x, neighbour.y, neighbour.z, state.kind
                    )),
                }
            }
            if today.is_empty() {
                today = "no arm refuses it".to_string();
            }
            steps.push(SealedStep { cell, today, derived, opens });
        }
        steps
    }

    /// The four landings whose socket approach is `pin` -- the only landings a
    /// net that cannot leave its own pin could ever be served by.
    fn servable_landings(
        growth: &Growth,
        gate: usize,
        input: usize,
        pin: Anchor,
    ) -> Vec<(Anchor, geometry::CellFacing)> {
        let mut found = Vec::new();
        for index in 0..4u8 {
            let facing = geometry::CellFacing::from_index(index).expect("0..4 is horizontal");
            for dx in -2..=2 {
                for dy in -2..=2 {
                    for dz in -2..=2 {
                        let anchor = Anchor {
                            x: pin.x + dx,
                            y: pin.y + dy,
                            z: pin.z + dz,
                        };
                        if socket_and_approach(anchor, facing, input).1 == pin {
                            found.push((anchor, facing));
                        }
                    }
                }
            }
            let _ = growth;
            let _ = gate;
        }
        found
    }


    // ---------------------------------------------------------------
    // 2 -- the 41 extra edges
    // ---------------------------------------------------------------

    /// One `EXTRA EDGE` line of `docs/derived/realised-graph-extras.md`.
    #[derive(Debug, Clone)]
    struct ExtraEdge {
        circuit: String,
        path: String,
        ships: bool,
        from_net: String,
        from: Anchor,
        to_net: String,
        to: Anchor,
        across: Anchor,
    }

    fn parse_anchor(text: &str) -> Anchor {
        let inner = text.trim().trim_start_matches('(').trim_end_matches(')');
        let parts: Vec<i32> = inner
            .split(',')
            .map(|piece| piece.trim().parse().expect("a coordinate"))
            .collect();
        assert_eq!(parts.len(), 3, "an address has three coordinates");
        Anchor {
            x: parts[0],
            y: parts[1],
            z: parts[2],
        }
    }

    /// Every extra edge the artifact records, with the circuit and path each
    /// was found on.
    fn recorded_extra_edges() -> Vec<ExtraEdge> {
        let mut edges = Vec::new();
        let (mut circuit, mut path, mut ships) = (String::new(), String::new(), false);
        for line in EXTRAS_ARTIFACT.lines() {
            if let Some(heading) = line.strip_prefix("## ") {
                let head = heading.replace('`', "");
                let mut halves = head.split(" / ");
                circuit = halves.next().unwrap_or_default().trim().to_string();
                let tail = halves.next().unwrap_or_default();
                ships = tail.contains("SHIPS TODAY");
                path = tail
                    .split("--")
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                continue;
            }
            let Some(body) = line.trim().strip_prefix("EXTRA EDGE") else {
                continue;
            };
            // `<net> at (a) -> <net> at (b) across (c), mechanism ...`
            let body = body.trim();
            let (lhs, rest) = body.split_once(" -> ").expect("an arrow");
            let (from_net, from) = lhs.split_once(" at ").expect("`at`");
            let (rhs, tail) = rest.split_once(" across ").expect("`across`");
            let (to_net, to) = rhs.split_once(" at ").expect("`at`");
            // Not `split(',')`: the address itself has two commas in it.
            let across = &tail[..tail.find(')').expect("a closed address") + 1];
            edges.push(ExtraEdge {
                circuit: circuit.clone(),
                path: path.clone(),
                ships,
                from_net: from_net.trim().to_string(),
                from: parse_anchor(from),
                to_net: to_net.trim().to_string(),
                to: parse_anchor(to),
                across: parse_anchor(across),
            });
        }
        edges
    }

    /// A built circuit, on one path, with everything the counts need.
    struct Built {
        world: World,
        /// Plan-time ownership and occupancy -- the map `keep_out` is asked
        /// against.
        plan: Reservation,
        /// Realised ownership per cell, cell to net index. Kept because the
        /// plan-time reservation and the realised one are different maps and a
        /// count that conflated them would be a different measurement.
        nets: HashMap<Position, usize>,
    }

    fn build_on(netlist: &Netlist, legacy: bool) -> Option<Built> {
        let (candidate, size) = if legacy {
            let compiled = compile::compile_legacy(netlist).ok()?;
            let emission = compiled
                .legacy_emission()
                .expect("compile_legacy always keeps its emission");
            let seed = seed_from_legacy_parts(netlist, emission).ok()?;
            let size = compiled.world.size();
            (seed, size)
        } else {
            let candidate =
                plan_from_netlist_within(netlist, &PortPlacements::default(), TRIAL_RIP_UP_ROUNDS)
                    .ok()?;
            let size = candidate_world_size(&candidate);
            (candidate, size)
        };
        let incident = vec![false; candidate.routes.len()];
        let plan = candidate.live_reservation(&incident);
        let verified = verify_and_expose(&candidate, netlist, size).ok()?;
        Some(Built {
            world: verified.realised.world,
            plan,
            nets: verified.reservation,
        })
    }

    /// What one recorded extra edge would have looked like to a two-hop-aware
    /// reservation, asked at plan time.
    struct EdgeVerdict {
        edge: ExtraEdge,
        /// The block the artifact's source cell actually holds.
        source: BlockState,
        /// Whether today's twelve-cell halo covers the offset at all.
        in_keep_out: bool,
        /// Whether the derived range covers it.
        in_derived: bool,
        /// Whether the plan-time reservation gives the two cells to different
        /// owners, which is what a reservation rule compares.
        owners: Option<(String, String)>,
        /// The same question at *net* granularity, from the realised ownership
        /// the four physical invariants ran against. The two answers differ,
        /// and the difference is the whole result for these edges.
        nets: Option<(usize, usize)>,
        /// Whether the source's realisation is decided before `reserve_path`
        /// writes the entry the rule would read.
        knowable_at_reserve_time: bool,
        /// What the block in the middle is, and what the plan-time reservation
        /// calls it. A two-hop rule has to reach *through* this cell, so what
        /// it is decides whether the rule could have been asked about it.
        mediator: String,
    }

    fn judge_extra_edges(edges: &[ExtraEdge], built: &Built) -> Vec<EdgeVerdict> {
        let today = keep_out_offsets();
        let mut verdicts = Vec::new();
        for edge in edges {
            let source = built
                .world
                .get(edge.from.x, edge.from.y, edge.from.z)
                .clone();
            let offset = offset_between(edge.from, edge.to);
            let halo = derived_halo(&source);
            let owners = match (
                built.plan.owner(&edge.from).map(str::to_string),
                built.plan.owner(&edge.to).map(str::to_string),
            ) {
                (Some(a), Some(b)) if a != b => Some((a, b)),
                _ => None,
            };
            // A repeater in the middle of a route is chosen by
            // `realise_branch_from`'s strength budget, which runs *after*
            // `reserve_path` has written the entry a plan-time rule reads. A
            // gate's own component is placed before any of it.
            let knowable = !matches!(source.kind, BlockKind::Repeater | BlockKind::Comparator)
                || built.plan.owner(&edge.from).is_none();
            let across = built
                .world
                .get(edge.across.x, edge.across.y, edge.across.z)
                .clone();
            let mediator = format!(
                "{:?} claimed as {}",
                across.kind,
                match built.plan.occupancy(&edge.across) {
                    Some(occupancy) => format!("{occupancy:?}"),
                    None => "nothing".to_string(),
                }
            );
            let at = |cell: Anchor| Position::new(cell.x, cell.y, cell.z);
            let nets = match (
                built.nets.get(&at(edge.from)).copied(),
                built.nets.get(&at(edge.to)).copied(),
            ) {
                (Some(a), Some(b)) if a != b => Some((a, b)),
                _ => None,
            };
            verdicts.push(EdgeVerdict {
                edge: edge.clone(),
                mediator,
                nets,
                in_keep_out: today.contains(&offset),
                in_derived: halo.measured.contains(&offset),
                owners,
                knowable_at_reserve_time: knowable,
                source,
            });
        }
        verdicts
    }

    // ---------------------------------------------------------------
    // 3 and 4 -- per circuit, per component
    // ---------------------------------------------------------------

    /// The keep-out footprint of one circuit, today and derived.
    #[derive(Default)]
    struct Footprint {
        /// Conductors the rule is asked about.
        cells: usize,
        /// `sum |keep_out(C)|` over them -- twelve each.
        today: usize,
        /// `sum |derived halo(C)|`, measured only.
        derived: usize,
        /// The same with unanswerable offsets kept.
        conservative: usize,
        removed: usize,
        added: usize,
        /// Of `removed`, the cells another component's derived range still
        /// refuses -- the air cell above a torch, every one of whose twelve is
        /// inside that torch's own hop 2. A real saving.
        removed_recovered: usize,
        /// Of `removed`, the cells nothing in the energising relation speaks
        /// for: a gate's support block and its sockets, inert stone that
        /// `energises` answers zero for and that mechanism 4 says must stay.
        /// **Not** a saving; a hole.
        removed_unspoken: usize,
        /// Distinct cells no foreign conductor may enter, today and derived.
        union_today: BTreeSet<Anchor>,
        union_derived: BTreeSet<Anchor>,
    }

    fn footprint_of(built: &Built) -> (Footprint, BTreeMap<String, Footprint>) {
        let today_offsets = keep_out_offsets();
        let (size_x, size_y, size_z) = built.world.size();
        let mut total = Footprint::default();
        let mut by_kind: BTreeMap<String, Footprint> = BTreeMap::new();
        let lo = Anchor { x: 0, y: 0, z: 0 };
        let hi = Anchor {
            x: size_x - 1,
            y: size_y - 1,
            z: size_z - 1,
        };
        for cell in built.plan.cells_within(lo, hi) {
            if built.plan.conductor_owner(&cell).is_none() {
                continue;
            }
            if cell.x < 0
                || cell.y < 0
                || cell.z < 0
                || cell.x >= size_x
                || cell.y >= size_y
                || cell.z >= size_z
            {
                continue;
            }
            let state = built.world.get(cell.x, cell.y, cell.z);
            let halo = derived_halo(state);
            let occupancy = built.plan.occupancy(&cell);
            let label = component_label(state, occupancy);
            let dropped = today_offsets.difference(&halo.measured).count();
            // Which of the two `keep_out`-only classes this cell is, if either.
            // The air cell above a torch is re-covered: every cell of its
            // twelve is a cell that torch's own hop 2 already refuses. A gate's
            // support and its sockets are not covered by anything.
            let recovered = state.kind == BlockKind::Air;
            let unspoken = state.kind == BlockKind::Solid
                && matches!(occupancy, Some(Occupancy::GateConductor));
            let entry = by_kind.entry(label).or_default();
            for slot in [&mut total, entry] {
                slot.cells += 1;
                slot.today += today_offsets.len();
                slot.derived += halo.measured.len();
                slot.conservative += halo.conservative.len();
                slot.removed += dropped;
                slot.added += halo.measured.difference(&today_offsets).count();
                if recovered {
                    slot.removed_recovered += dropped;
                }
                if unspoken {
                    slot.removed_unspoken += dropped;
                }
                for offset in &today_offsets {
                    slot.union_today.insert(Anchor {
                        x: cell.x + offset.0,
                        y: cell.y + offset.1,
                        z: cell.z + offset.2,
                    });
                }
                for offset in &halo.measured {
                    slot.union_derived.insert(Anchor {
                        x: cell.x + offset.0,
                        y: cell.y + offset.1,
                        z: cell.z + offset.2,
                    });
                }
            }
        }
        (total, by_kind)
    }

    /// A block's name in the per-component table: kind, and facing where the
    /// artifact prints one row per facing.
    fn component_label(state: &BlockState, occupancy: Option<Occupancy>) -> String {
        let name = energising::artifact_name(state.kind)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{:?}", state.kind).to_lowercase());
        match (state.kind, state.facing) {
            (BlockKind::Repeater | BlockKind::Comparator | BlockKind::WallTorch, Some(facing)) => {
                format!("{name} {facing:?}")
            }
            // The two classes `keep_out` reserves for that hold no component at
            // all. Named apart because the arithmetic reads completely
            // differently for them -- see `Footprint::removed_recovered` and
            // `removed_unspoken`.
            (BlockKind::Air, _) => "air over a torch".to_string(),
            (BlockKind::Solid, _) if matches!(occupancy, Some(Occupancy::GateConductor)) => {
                "stone (gate support)".to_string()
            }
            _ => name,
        }
    }


    // ---------------------------------------------------------------
    // The run
    // ---------------------------------------------------------------

    /// The growth settings the wedge record was taken at, so the state this
    /// measures is the state the growth probe reports.
    fn wedge_settings() -> GrowthSettings {
        GrowthSettings {
            order: "depth".to_string(),
            lambda: 0.5,
            windows: vec![8, 16, 32, 64],
            tries: 8,
            escape: 0,
            seed_pitch: 0,
            verbose: false,
            settle: false,
            rip: 0,
            seed: 0,
            rip_whole: false,
        }
    }

    /// What one wedge's arithmetic came to.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SealCounts {
        /// In-plane steps out of the sealed pin that the derived range would
        /// hand back. **This is the number that decides the whole question.**
        opens_in_plane: usize,
        in_plane: usize,
        /// The same over all twelve steps, climbing included.
        opens_any: usize,
        steps: usize,
        /// Landings whose socket approach is that pin and that are refused by
        /// a cell of the new body landing where a block already stands.
        landings_overlapping: usize,
        landings: usize,
    }

    /// One wedge, measured: which cells seal the pin, what refuses each of them
    /// today, and what the derived range says about the same pair.
    fn measure_one_wedge(
        name: &str,
        netlist: &Netlist,
        expected: (&str, usize),
    ) -> SealCounts {
        let settings = wedge_settings();
        let mut oracle = Growth::seeded(netlist, &settings).expect("the seed layout");
        oracle.grow();
        let wedge = oracle
            .wedge
            .as_ref()
            .unwrap_or_else(|| panic!("{name} did not wedge, so there is nothing to measure"));
        let placed = oracle.placed.iter().filter(|done| **done).count();
        assert_eq!(
            (wedge.gate.as_str(), placed),
            expected,
            "{name}'s wedge moved; every number below would be about a different state"
        );
        let gate = netlist
            .gates
            .iter()
            .position(|g| g.output == wedge.gate)
            .expect("the wedged gate is in the netlist");
        let signal = netlist.gates[gate].inputs[0].clone();
        let pin = oracle.pins[&signal];
        let states = growth_states(&oracle);

        eprintln!(
            "\n== {name}: WEDGE `{}` after {placed}/{} gates, on net `{signal}`'s pin \
             ({}, {}, {}) ==",
            wedge.gate,
            netlist.gates.len(),
            pin.x,
            pin.y,
            pin.z
        );

        let steps = seal_report(&oracle, &states, pin, &signal);
        let in_plane: Vec<&SealedStep> = steps.iter().filter(|step| step.cell.y == pin.y).collect();
        eprintln!(
            "   the four in-plane steps out of the pin -- the four cells that seal it:"
        );
        let mut opened_plane = 0usize;
        for step in &in_plane {
            eprintln!(
                "     ({}, {}, {})\n       TODAY:   {}",
                step.cell.x, step.cell.y, step.cell.z, step.today
            );
            for line in &step.derived {
                eprintln!("       DERIVED: {line}");
            }
            if step.opens {
                opened_plane += 1;
                eprintln!("       => WOULD OPEN under the derived range");
            } else {
                eprintln!("       => still refused");
            }
        }
        let opened_all = steps.iter().filter(|step| step.opens).count();
        eprintln!(
            "   in-plane: {opened_plane} of {} open. All twelve steps (climbing included): \
             {opened_all} of {} open.",
            in_plane.len(),
            steps.len()
        );

        // The four landings whose socket approach is that pin.
        let landings = servable_landings(&oracle, gate, 0, pin);
        eprintln!(
            "   the {} landing(s) whose socket approach IS that pin -- the only ones a net \
             that cannot leave its pin could be served by:",
            landings.len()
        );
        let mut overlapping = 0usize;
        let mut stands = 0usize;
        for (anchor, facing) in &landings {
            let (overlaps, why) = why_the_body_would_not_stand(&oracle, gate, *anchor, *facing);
            overlapping += usize::from(overlaps);
            if why.starts_with("it stands") {
                stands += 1;
            }
            eprintln!(
                "     anchor ({}, {}, {}) facing {}: {}\n            {why}",
                anchor.x,
                anchor.y,
                anchor.z,
                facing.index(),
                if overlaps { "BLOCK OVERLAP" } else { "clearance only" }
            );
        }
        eprintln!(
            "   => {overlapping} of {} refused by BLOCK OVERLAP, which no clearance rule of \
             any kind can relax; {stands} stand today.",
            landings.len()
        );
        SealCounts {
            opens_in_plane: opened_plane,
            in_plane: in_plane.len(),
            opens_any: opened_all,
            steps: steps.len(),
            landings_overlapping: overlapping,
            landings: landings.len(),
        }
    }

    /// The 41 recorded extra edges, judged.
    ///
    /// Returned as counts rather than printed, so
    /// `the_derived_range_sees_every_recorded_extra_edge_and_keep_out_sees_none`
    /// asserts the same arithmetic
    /// [`measure_the_energising_arithmetic`] reports rather than a second,
    /// differently-wrong copy of it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ExtrasCounts {
        /// How many the artifact records.
        recorded: usize,
        /// How many were rebuilt here and judged.
        covered: usize,
        /// How many the derived two-hop range covers the offset of.
        seen: usize,
        /// ...and today's twelve-cell halo covers.
        seen_by_keep_out: usize,
        /// ...and a rule keyed on the plan-time `Reservation::owner` refuses.
        by_owner: usize,
        /// ...and a rule keyed on net ownership refuses.
        by_net: usize,
        /// Of `by_net`, how many are in a world `compile` ships.
        by_net_shipping: usize,
        /// How many have an emitter whose realisation is decided after
        /// `reserve_path` writes the entry such a rule reads.
        undecided_source: usize,
    }

    fn extras_arithmetic(verbose: bool) -> ExtrasCounts {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };
        macro_rules! say {
            ($($arg:tt)*) => {
                if verbose {
                    eprintln!($($arg)*);
                }
            };
        }
        say!("\n== THE 41 EXTRA EDGES ==");
        let edges = recorded_extra_edges();
        say!("   the artifact records {} extra edge(s)", edges.len());
        // The same six the extras record was taken over, with the same
        // lowerings -- `crate::compile::coupling::tests::circuits`. Anything
        // less would leave recorded edges uncounted, which is the one way this
        // number could be quietly too small.
        let mut all: Vec<(String, Netlist)> = vec![
            ("and4".to_string(), build_and4_netlist().0),
            ("full_adder".to_string(), build_full_adder_netlist().0),
            ("segment_a".to_string(), build_single_segment_netlist(0).0),
            ("seven_segment".to_string(), build_seven_segment_netlist().0),
        ];
        for circuit in crate::circuits::verilog::CIRCUITS {
            let (gate_level, _labels) = circuit.baked_netlist();
            let lowered = match circuit.name {
                "verilog:seven_segment" => crate::compile::lowering::lower_optimised(&gate_level),
                _ => crate::compile::lowering::lower(&gate_level),
            }
            .unwrap_or_else(|error| panic!("{} must lower: {error}", circuit.name));
            all.push((circuit.name.to_string(), lowered));
        }
        // Three counts, not one, because "would a two-hop-aware reservation
        // have refused it" has three different answers depending on what the
        // reservation is allowed to know.
        //
        // * `seen` -- the offset is inside the derived range and outside
        //   `keep_out`'s twelve. The *geometry* is caught.
        // * `refused_by_owner` -- and the plan-time `Reservation::owner`
        //   already gives the two cells to different owners, so today's map
        //   suffices.
        // * `refused_by_net` -- and *net* ownership, which the plan-time map
        //   does not carry, gives them to different nets.
        let mut seen = 0usize;
        let mut seen_by_keep_out = 0usize;
        let mut refused_by_owner = 0usize;
        let mut refused_by_net = 0usize;
        let mut refused_shipping = 0usize;
        let mut undecided_source = 0usize;
        let mut residue: Vec<String> = Vec::new();
        let mut covered = 0usize;
        for (name, netlist) in &all {
            for legacy in [false, true] {
                let mine: Vec<ExtraEdge> = edges
                    .iter()
                    .filter(|edge| {
                        edge.circuit == *name
                            && edge.path == if legacy { "legacy" } else { "relaxation" }
                    })
                    .cloned()
                    .collect();
                if mine.is_empty() {
                    continue;
                }
                let Some(built) = build_on(netlist, legacy) else {
                    residue.push(format!(
                        "{name}/{}: {} edge(s) -- this path could not be rebuilt here",
                        if legacy { "legacy" } else { "relaxation" },
                        mine.len()
                    ));
                    continue;
                };
                covered += mine.len();
                say!(
                    "\n   -- {name} / {} --",
                    if legacy { "legacy" } else { "relaxation" }
                );
                for verdict in judge_extra_edges(&mine, &built) {
                    let edge = &verdict.edge;
                    if verdict.in_derived {
                        seen += 1;
                    }
                    if verdict.in_keep_out {
                        seen_by_keep_out += 1;
                    }
                    if verdict.in_derived && verdict.owners.is_some() {
                        refused_by_owner += 1;
                    }
                    if verdict.in_derived && verdict.nets.is_some() {
                        refused_by_net += 1;
                        if edge.ships {
                            refused_shipping += 1;
                        }
                    }
                    if !verdict.knowable_at_reserve_time {
                        undecided_source += 1;
                    }
                    if !verdict.in_derived {
                        residue.push(format!(
                            "{name}/{}: {} at ({}, {}, {}) -> {}: the derived range does \
                             NOT cover this offset",
                            if legacy { "legacy" } else { "relaxation" },
                            edge.from_net,
                            edge.from.x,
                            edge.from.y,
                            edge.from.z,
                            edge.to_net
                        ));
                    }
                    say!(
                        "     {} ({}, {}, {}) {:?} -> {} ({}, {}, {}) | keep_out {} | derived \
                         {} | plan owner {} | net owner {} | knowable at reserve time {}",
                        edge.from_net,
                        edge.from.x,
                        edge.from.y,
                        edge.from.z,
                        verdict.source.kind,
                        edge.to_net,
                        edge.to.x,
                        edge.to.y,
                        edge.to.z,
                        if verdict.in_keep_out { "YES" } else { "no" },
                        if verdict.in_derived { "YES" } else { "no" },
                        match &verdict.owners {
                            Some((a, b)) => format!("`{a}` vs `{b}`"),
                            None => format!(
                                "SAME ({})",
                                built.plan.owner(&edge.from).unwrap_or("unclaimed")
                            ),
                        },
                        match &verdict.nets {
                            Some((a, b)) => format!("net {a} vs net {b}"),
                            None => "same or unclaimed".to_string(),
                        },
                        if verdict.knowable_at_reserve_time {
                            "yes"
                        } else {
                            "NO -- repeater chosen after reserve_path"
                        }
                    );
                    say!(
                        "        across ({}, {}, {}): {}",
                        edge.across.x, edge.across.y, edge.across.z, verdict.mediator
                    );
                }
            }
        }
        say!(
            "\n   {covered} of {} recorded edges rebuilt here.\n   \
             the derived two-hop range covers the offset in {seen} of them; today's \
             `keep_out` covers 0.\n   \
             a two-hop rule keyed on the plan-time `Reservation::owner` would have refused \
             {refused_by_owner}: in the rest the two cells belong to the SAME reservation \
             owner, because a gate's body and its input sockets are one owner and this \
             whole mechanism happens inside one of them.\n   \
             keyed on NET ownership -- which the plan-time reservation does not carry -- it \
             would have refused {refused_by_net} ({refused_shipping} in a world `compile` \
             ships).\n   \
             and in {undecided_source} of them the emitting cell realises as a REPEATER, \
             chosen by `realise_branch_from`'s strength budget AFTER `reserve_path` wrote \
             the entry such a rule would read -- the same undecidability that stopped \
             `keep_out_against`.",
            edges.len()
        );
        for line in &residue {
            say!("   RESIDUE: {line}");
        }
        ExtrasCounts {
            recorded: edges.len(),
            covered,
            seen,
            seen_by_keep_out,
            by_owner: refused_by_owner,
            by_net: refused_by_net,
            by_net_shipping: refused_shipping,
            undecided_source,
        }
    }

    /// **The four numbers.** Everything above, run and printed.
    ///
    /// # Re-running it
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::measure_the_energising_arithmetic \
    ///   -- --ignored --nocapture
    /// ```
    ///
    /// `#[ignore]` because it grows two circuits and builds six on both compile
    /// paths; the assertions that keep its headline numbers honest are the four
    /// tests below it, which are not ignored.
    ///
    /// # What it measured
    ///
    /// `--release`, whole run 4.7s, 2026-08-17. Every number below is one line
    /// of that run's output.
    ///
    /// ## 1 -- the wedges. **Neither opens. 0 of 4, twice.**
    ///
    /// ```text
    /// full_adder, g9 wedged on g8's pin (14,1,162)
    ///   (13,1,162)  keep_out sees primitive:0 at (12,1,162)  -> that cell is a NOR's
    ///               SUPPORT BLOCK. `energises` answers zero for inert stone; a powered
    ///               dust one step from it turns the gate's torch off, measured.
    ///   (15,1,162)  keep_out sees primitive:1's dust at (16,1,162), same layer
    ///               -> joined UNCONDITIONALLY
    ///   (14,1,161)  keep_out sees g1's dust at (15,1,161), same layer -> ditto
    ///   (14,1,163)  OCCUPIED outright by primitive:8's body -> not a clearance question
    /// segment_a, g8 wedged on g7's pin (26,1,146)
    ///   (25,1,146)  g4's dust at (24,1,146), same layer -> UNCONDITIONAL
    ///   (27,1,146)  primitive:5's SUPPORT BLOCK at (28,1,146) -> mechanism 4
    ///   (26,1,145)  g2's dust at (26,1,144), same layer -> UNCONDITIONAL
    ///   (26,1,147)  OCCUPIED outright by primitive:7's body
    /// ```
    ///
    /// 0 of 4 in-plane steps open on each, and 0 of all twelve including the
    /// climbing ones. The four servable landings stay 4-of-4 BLOCK OVERLAP.
    ///
    /// ## 2 -- the 41 extra edges. **The geometry is caught; the ownership is not.**
    ///
    /// 41 of 41 rebuilt and judged. `keep_out`'s twelve cells cover the offset
    /// of **0**; the derived two-hop range covers **41**. But a rule keyed on
    /// the plan-time [`Reservation::owner`] would have refused **0** -- in
    /// every one of the 41 the emitting cell and the contaminated cell have the
    /// *same* reservation owner, because a gate's body and its input sockets
    /// are one owner and the whole mechanism happens inside one of them. Keyed
    /// on **net** ownership, which the plan-time map does not carry: 41, of
    /// which 37 in a world `compile` ships. And 41 of 41 are emitted by a
    /// **repeater**, chosen by `realise_branch_from` after `reserve_path` wrote
    /// the entry such a rule reads.
    ///
    /// ## 3 -- per circuit, cell-reservations (conductors x halo)
    ///
    /// | circuit | conductors | today | derived | removed | added | net |
    /// |---|---|---|---|---|---|---|
    /// | and4 (relaxation) | 134 | 1,608 | 1,420 | 260 | 72 | **-188** |
    /// | full_adder (relaxation) | 578 | 6,936 | 6,196 | 926 | 186 | **-740** |
    /// | segment_a (legacy) | 279 | 3,348 | 2,104 | 1,454 | 210 | **-1,244** |
    ///
    /// And the split that matters more than the total: of `segment_a`'s 1,454
    /// removed, **600** are the air cell over a torch (re-covered by that
    /// torch's own hop 2 -- a real saving), **552** are a gate's support or
    /// socket (which the energising relation does not speak for at all, and
    /// mechanism 4 keeps), and only **302** are a genuine narrowing of a real
    /// component's halo. **Dust removes 0 and adds 0** on every circuit, and
    /// dust is 478 of full_adder's 578 conductors.
    ///
    /// ## 4 -- per component
    ///
    /// | block | today | hop 1 | hop 2 | measured | conservative |
    /// |---|---|---|---|---|---|
    /// | dust | 12 | (12, from the join artifact) | 0 | **12** | 12 |
    /// | repeater / comparator, any facing | 12 | 1 | 5 | **6** | 12 |
    /// | torch, wall torch any facing | 12 | 5 | 5 | **10** | 10 |
    /// | lever | 12 | 6 | 18 | **24** | 24 |
    /// | redstone block | 12 | 6 | 0 | **6** | 6 |
    /// | stone / glass / lamp | 12 | 0 | 0 | **0** | 0 |
    #[test]
    #[ignore = "measurement harness; run with --ignored --nocapture"]
    fn measure_the_energising_arithmetic() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::build_single_segment_netlist;

        eprintln!(
            "\n===== what a component-aware, two-hop keep-out would cost and would buy =====\n\
             `keep_out` reserves twelve cells for everything. The derived range is read out \
             of docs/derived/coupling-mechanisms.md (both hops) and \
             docs/derived/dust-join-relation.md (dust against dust), by \
             compile::energising."
        );

        // --- 4: per component ---------------------------------------
        eprintln!("\n== PER COMPONENT: twelve today, against what the derivation measures ==");
        eprintln!(
            "   {:<18} {:>6} {:>6} {:>6} {:>8} {:>8} {:>8}",
            "block", "today", "hop1", "hop2", "measured", "conserv.", "net"
        );
        let mut components: Vec<(String, BlockKind, Option<Facing>)> = Vec::new();
        for kind in [
            BlockKind::RedstoneWire,
            BlockKind::Repeater,
            BlockKind::Comparator,
            BlockKind::Torch,
            BlockKind::WallTorch,
            BlockKind::Lever,
            BlockKind::RedstoneBlock,
            BlockKind::Solid,
            BlockKind::Glass,
            BlockKind::Lamp,
        ] {
            let facings: Vec<Option<Facing>> = match kind {
                BlockKind::Repeater | BlockKind::Comparator | BlockKind::WallTorch => {
                    [Facing::North, Facing::South, Facing::East, Facing::West]
                        .into_iter()
                        .map(Some)
                        .collect()
                }
                _ => vec![None],
            };
            for facing in facings {
                let mut state = BlockState::air();
                state.kind = kind;
                state.facing = facing;
                components.push((component_label(&state, None), kind, facing));
            }
        }
        for (label, kind, facing) in &components {
            let mut state = BlockState::air();
            state.kind = *kind;
            state.facing = *facing;
            let halo = derived_halo(&state);
            let range = energising::energises(*kind, *facing);
            let (hop1, hop2) = if *kind == BlockKind::RedstoneWire {
                ("12*".to_string(), "0".to_string())
            } else {
                (range.hop1.len().to_string(), range.hop2.len().to_string())
            };
            eprintln!(
                "   {:<18} {:>6} {:>6} {:>6} {:>8} {:>8} {:>+8}",
                label,
                12,
                hop1,
                hop2,
                halo.measured.len(),
                halo.conservative.len(),
                halo.measured.len() as i64 - 12
            );
        }
        eprintln!(
            "   * dust's row is the dust-join artifact, not `energises`: Table 1's `dust` \
             row reads `x` where its own feed stands, and the join artifact's summary is \
             that all twelve of `keep_out`'s cells really join."
        );

        // --- 1: the wedges ------------------------------------------
        eprintln!("\n== THE WEDGES ==");
        let (adder, _) = build_full_adder_netlist();
        let (segment, _) = build_single_segment_netlist(0);
        let adder_seal = measure_one_wedge("full_adder", &adder, ("g9", 7));
        let segment_seal = measure_one_wedge("segment_a", &segment, ("g8", 18));

        // --- 3 and 4: per circuit -----------------------------------
        eprintln!("\n== PER CIRCUIT: the keep-out footprint, today and derived ==");
        let mut builds: BTreeMap<(String, bool), Built> = BTreeMap::new();
        let circuits: Vec<(&str, Netlist, bool)> = vec![
            ("and4", build_and4_netlist().0, false),
            ("full_adder", build_full_adder_netlist().0, false),
            ("segment_a", build_single_segment_netlist(0).0, true),
        ];
        for (name, netlist, legacy) in &circuits {
            let Some(built) = build_on(netlist, *legacy) else {
                eprintln!("   {name}: this path cannot build it");
                continue;
            };
            let (total, by_kind) = footprint_of(&built);
            eprintln!(
                "\n   -- {name} ({}) : {} conductor(s) --",
                if *legacy { "legacy" } else { "relaxation" },
                total.cells
            );
            eprintln!(
                "      today {} cell-reservations ({} distinct cells) | derived {} ({} \
                 distinct) | conservative {}",
                total.today,
                total.union_today.len(),
                total.derived,
                total.union_derived.len(),
                total.conservative
            );
            eprintln!(
                "      removed {} | added {} | net {:+}",
                total.removed,
                total.added,
                total.derived as i64 - total.today as i64
            );
            eprintln!(
                "      of the {} removed: {} are the air cell over a torch, RE-COVERED \
                 by that torch's own hop 2; {} are a gate's support or socket, which \
                 the energising relation does not speak for AT ALL (mechanism 4 keeps \
                 them); {} are a genuine narrowing of a real component's halo.",
                total.removed,
                total.removed_recovered,
                total.removed_unspoken,
                total.removed - total.removed_recovered - total.removed_unspoken
            );
            eprintln!(
                "      {:<18} {:>7} {:>8} {:>9} {:>8} {:>8}",
                "block", "cells", "today", "derived", "removed", "added"
            );
            for (label, part) in &by_kind {
                eprintln!(
                    "      {:<18} {:>7} {:>8} {:>9} {:>8} {:>8}",
                    label, part.cells, part.today, part.derived, part.removed, part.added
                );
            }
            builds.insert(((*name).to_string(), *legacy), built);
        }

        // --- 2: the 41 extra edges ----------------------------------
        eprintln!("
== THE 41 EXTRA EDGES ==");
        let extras = extras_arithmetic(true);
        eprintln!("   {extras:#?}");

        eprintln!(
            "\n== VERDICT INPUTS ==\n   full_adder: {} of {} in-plane steps open, {} of {} \
             landings BLOCK OVERLAP\n   segment_a:  {} of {} in-plane steps open, {} of {} \
             landings BLOCK OVERLAP",
            adder_seal.opens_in_plane,
            adder_seal.in_plane,
            adder_seal.landings_overlapping,
            adder_seal.landings,
            segment_seal.opens_in_plane,
            segment_seal.in_plane,
            segment_seal.landings_overlapping,
            segment_seal.landings
        );
    }



    /// **The decisive number, pinned so it cannot drift unnoticed.**
    ///
    /// The hypothesis this phase was sent to test is that `keep_out` is at once
    /// too big and too small, and that the too-big half is what seals
    /// `segment_a`'s `g7` pin and `full_adder`'s `g8` pin. It is not. Every
    /// in-plane step out of both pins is refused by something the derived range
    /// refuses too, and the reasons are measured one cell at a time by
    /// [`measure_one_wedge`]:
    ///
    /// * a **same-layer dust** offender -- `docs/derived/dust-join-relation.md`
    ///   joins those unconditionally, so no component-awareness reaches them;
    /// * a **gate's support block** -- inert stone, which `energises` answers
    ///   *nothing* for, and which
    ///   `energising::tests::a_powered_dust_against_a_gate_support_puts_its_
    ///   torch_out` measures to turn that gate's torch off anyway;
    /// * or a cell **another block already stands in**, which is not a
    ///   clearance question at all.
    ///
    /// Run under `--release`; it grows two circuits and takes about 3 seconds.
    ///
    /// # Rule 2: injected, confirmed red, reverted (`--release`)
    ///
    /// * **Delete [`derived_verdict`]'s inert-`GateConductor` arm** -- the one
    ///   that keeps a gate's support block, which `energises` answers zero for.
    ///   `full_adder` goes to **1 of 4 open** and this goes red. That arm is
    ///   the difference between the honest answer and the flattering one.
    /// * **Delete its unconditional same-layer dust arm as well**, and stop the
    ///   vertical arm refusing a pair with no lid. `full_adder` goes to **2 of
    ///   4 open** and this goes red again.
    /// * Narrowing the mechanism-4 arm to nothing (`cardinal = false`) does
    ///   **not** turn it red, and that null result is recorded rather than
    ///   dropped: the cell is then kept by the `NOT MEASURED` arm instead, so
    ///   what this test pins is that the cell stays refused, not which of the
    ///   two sentences refuses it.
    #[test]
    fn neither_sealed_pin_opens_under_the_derived_range() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::build_single_segment_netlist;

        let (adder, _) = build_full_adder_netlist();
        let (segment, _) = build_single_segment_netlist(0);
        for (name, netlist, expected) in [
            ("full_adder", adder, ("g9", 7usize)),
            ("segment_a", segment, ("g8", 18usize)),
        ] {
            let counts = measure_one_wedge(name, &netlist, expected);
            assert_eq!(
                counts.in_plane, 4,
                "{name}: a pin has four in-plane steps and they are the four cells the \
                 wedge report calls the seal"
            );
            assert_eq!(
                counts.opens_in_plane, 0,
                "{name}: NOT ONE of the four sealed cells is handed back by the derived \
                 range. If this ever goes non-zero the verdict changes."
            );
            assert_eq!(
                counts.opens_any, 0,
                "{name}: nor any of the twelve steps including the climbing ones"
            );
            assert_eq!(
                (counts.landings_overlapping, counts.landings),
                (4, 4),
                "{name}: and all four servable landings are refused by BLOCK OVERLAP, \
                 which no clearance rule of any kind relaxes"
            );
        }
    }

    /// **The other side of the hypothesis, and this half holds.**
    ///
    /// Every one of the 41 recorded extra edges is at an offset today's twelve
    /// cells do not cover and the derived two-hop range does. What that buys is
    /// then bounded by two things this pins as well: the plan-time reservation's
    /// ownership is per *gate body*, not per net, and every one of the 41 is
    /// emitted by a repeater whose existence is decided after the entry such a
    /// rule would read is written.
    #[test]
    fn the_derived_range_sees_every_recorded_extra_edge_and_keep_out_sees_none() {
        let counts = extras_arithmetic(false);
        let ExtrasCounts {
            recorded,
            covered,
            seen,
            seen_by_keep_out,
            by_owner,
            by_net,
            by_net_shipping,
            undecided_source,
        } = counts;
        assert_eq!(covered, recorded, "every recorded edge is rebuilt and judged");
        assert_eq!(
            seen, recorded,
            "the derived two-hop range covers the offset of every recorded extra edge"
        );
        assert_eq!(
            seen_by_keep_out, 0,
            "and today's twelve-cell halo covers the offset of NONE of them -- every one 
             of the 41 is two cells away, which is the hop `keep_out` does not have"
        );
        assert_eq!(
            by_net_shipping, 37,
            "37 of them are in a world `compile` ships, which is the artifact's own count"
        );
        assert_eq!(
            by_owner, 0,
            "and a rule keyed on the plan-time `Reservation::owner` would have refused \
             NONE of them: in every one the emitting cell and the contaminated cell \
             belong to the same owner, because a gate's body and its input sockets are \
             one owner and the whole mechanism happens inside one of them"
        );
        assert_eq!(
            by_net, recorded,
            "keyed on net ownership -- which the plan-time map does not carry -- it \
             would have refused all of them"
        );
        assert_eq!(
            undecided_source, recorded,
            "and in every one the emitter realises as a repeater, chosen after \
             `reserve_path` wrote the entry the rule reads"
        );
    }

    /// The per-circuit footprint, pinned. `check.sh`'s four block counts do not
    /// move because nothing here is wired into the router; these are the numbers
    /// that say what wiring it in would change.
    #[test]
    fn the_keep_out_footprint_per_circuit_is_what_is_recorded() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::build_single_segment_netlist;

        let cases: Vec<(&str, Netlist, bool, [usize; 6])> = vec![
            // conductors, today, derived, removed, added, removed-but-unspoken
            ("and4", build_and4_netlist().0, false, [134, 1608, 1420, 260, 72, 84]),
            (
                "full_adder",
                build_full_adder_netlist().0,
                false,
                [578, 6936, 6196, 926, 186, 264],
            ),
            (
                "segment_a",
                build_single_segment_netlist(0).0,
                true,
                [279, 3348, 2104, 1454, 210, 552],
            ),
        ];
        for (name, netlist, legacy, expected) in cases {
            let built = build_on(&netlist, legacy)
                .unwrap_or_else(|| panic!("{name} must build on the path `compile` ships"));
            let (total, _) = footprint_of(&built);
            assert_eq!(
                [
                    total.cells,
                    total.today,
                    total.derived,
                    total.removed,
                    total.added,
                    total.removed_unspoken,
                ],
                expected,
                "{name}: the footprint arithmetic moved"
            );
            assert_eq!(
                total.today,
                total.cells * 12,
                "{name}: today's rule is twelve cells per conductor, whatever stands there"
            );
        }
    }

    /// Dust is where the saving is not, and this is the one line of the whole
    /// exercise that decides how big it could ever be.
    #[test]
    fn dust_is_most_of_what_a_route_lays_and_keep_out_is_exact_for_it() {
        use crate::circuits::full_adder::build_full_adder_netlist;

        let built = build_on(&build_full_adder_netlist().0, false).expect("full_adder builds");
        let (_, by_kind) = footprint_of(&built);
        let dust = by_kind.get("dust").expect("a full_adder is mostly dust");
        assert_eq!(
            (dust.removed, dust.added),
            (0, 0),
            "`docs/derived/dust-join-relation.md`: all twelve of `keep_out`'s cells really \
             join for a dust cell on a stone floor, so component-awareness neither takes \
             one away nor adds one"
        );
        let all: usize = by_kind.values().map(|part| part.cells).sum();
        assert!(
            dust.cells * 2 > all,
            "and dust is the majority of the conductors the rule is asked about: {} of \
             {all}",
            dust.cells
        );
    }


    // -----------------------------------------------------------------------
    // Negotiated congestion
    //
    // `route_negotiated` and the switch that keeps it out of the shipping path.
    // Everything here is measured on this tree at HEAD `9d0707d`; every number
    // quoted comes out of `what_the_two_routers_do_to_the_six_condition_circuits`
    // below, which is in the tree and re-runnable.

    /// `verilog:and4` placed by relaxation and routed by whichever router.
    ///
    /// The placement is shared deliberately: the two routers are then the only
    /// difference between the two plans, which is what makes any difference
    /// between them attributable.
    fn verilog_and4_both_ways() -> (Netlist, PlanCandidate, PlanCandidate) {
        let circuit = crate::circuits::verilog::find("verilog:and4")
            .expect("the catalog ships verilog:and4");
        let (gate_level, _) = circuit.baked_netlist();
        // `lower`, not `lower_optimised`: it is what `compile` runs for this
        // circuit and what `every_reference_circuit_records_which_path_produced_it`
        // pins, so this measures the circuit the compiler actually builds.
        let netlist = crate::compile::lowering::lower(&gate_level)
            .expect("verilog:and4 must lower");
        // Through the switch rather than around it, so this measures the two
        // routers as a caller reaches them and a broken `plan_with_axes` arm
        // cannot pass.
        let rip_up = plan_from_netlist_with_router(
            &netlist,
            &PortPlacements::default(),
            RIP_UP_ROUNDS,
            RouterKind::RipUp,
        )
        .expect("verilog:and4 routes through the rip-up router");
        let negotiated = plan_from_netlist_with_router(
            &netlist,
            &PortPlacements::default(),
            NEGOTIATION_ROUNDS,
            RouterKind::Negotiated,
        )
        .expect("verilog:and4 routes through the negotiated router");
        (netlist, rip_up, negotiated)
    }

    fn route_named<'a>(candidate: &'a PlanCandidate, id: &str) -> &'a Route {
        candidate
            .routes()
            .iter()
            .find(|route| route.id() == id)
            .unwrap_or_else(|| panic!("this plan has a route `{id}`"))
    }

    /// The first target of the negotiation work, and the smallest instance of
    /// the defect it exists to remove.
    ///
    /// **The brief's coordinates for this case do not reproduce at this HEAD
    /// and the numbers below are re-measured rather than quoted.** The brief
    /// describes `n1` running `(58,1,36) -> (58,1,30)`, a straight line six
    /// cells long, laid in 11 cells against a wanted 8. At `9d0707d`
    /// relaxation puts `n1`'s source at `(61,1,49)` and `g0`'s second socket at
    /// `(65,1,44)`, so the net is a diagonal and not a straight line, and the
    /// rip-up router lays it in **14** cells. Same circuit, same net, same
    /// mechanism, different placement -- and the mechanism is what this test is
    /// about, so it is stated in the numbers this tree produces.
    ///
    /// What the rip-up router does, measured: `n0` is laid first (the order is
    /// alphabetical) and takes the corridor at `x = 62..63`; `n1` then has no
    /// way through, because `anchor_is_free_for` refuses a contested cell
    /// outright and cannot be told *which* cell was wanted. So it climbs --
    /// `(60,1,48) -> (60,2,47) -> y=3` -- and spends two cells and a repeater
    /// going round.
    ///
    /// What negotiation does: both nets are laid every iteration and priced
    /// rather than refused, and after four iterations (contested cells
    /// `8 -> 5 -> 6 -> 0`) they have divided the space between them. `n1` comes
    /// out at **12** cells, which is `manhattan + 1` and one cell off the
    /// unobstructed best.
    ///
    /// The two nets genuinely cross -- `(66,51) -> (62,44)` and
    /// `(61,49) -> (66,44)` are two segments that intersect, and
    /// `anchor_is_free_for` will not let one lay its floor on the other's dust
    /// -- so neither of them can have a flat straight line and the question is
    /// only which of them pays and how much. Under the rip-up router the answer
    /// is decided by net order; under negotiation it is decided by price.
    #[test]
    fn negotiation_shortens_the_route_the_rip_up_router_sent_over_the_top() {
        let (netlist, rip_up, negotiated) = verilog_and4_both_ways();

        assert_eq!(
            route_named(&rip_up, "n1").anchors().len(),
            14,
            "the rip-up router's n1, for the record: {:?}",
            route_named(&rip_up, "n1").anchors()
        );
        assert_eq!(
            route_named(&negotiated, "n1").anchors().len(),
            12,
            "negotiation must win n1 room the rip-up router could not ask for: {:?}",
            route_named(&negotiated, "n1").anchors()
        );

        // Not just this one net: the whole circuit is shorter, so nothing was
        // paid for it elsewhere.
        let cells = |candidate: &PlanCandidate| -> usize {
            candidate.routes().iter().map(|route| route.anchors().len()).sum()
        };
        assert_eq!((cells(&rip_up), cells(&negotiated)), (131, 129));

        // ROUTES WITHOUT VERIFIES IS WORTH NOTHING. A congestion probe once
        // routed segment_a below legacy's anchor box and failed verification;
        // `verify_candidate` is the judge, not the cell count.
        verify_candidate(&negotiated, &netlist)
            .expect("the negotiated plan must pass all four physical invariants");
    }

    /// The switch, and the thing that would make this whole commit a silent
    /// change of what ships.
    ///
    /// Two claims, and the second is why the first is worth asserting: the
    /// shipping router is the rip-up one, **and** the two routers genuinely
    /// disagree on a circuit `compile` builds -- so a flip of
    /// [`SHIPPING_ROUTER`] is a change anyone can see rather than a constant
    /// nobody reads. `the_hand_written_circuits_keep_their_measured_size`
    /// covers the block counts; this covers the reason they do not move.
    #[test]
    fn the_shipping_router_is_the_rip_up_one_and_the_two_do_not_agree() {
        assert_eq!(SHIPPING_ROUTER, RouterKind::RipUp);

        let (netlist, rip_up, negotiated) = verilog_and4_both_ways();
        assert_ne!(
            rip_up.routes(),
            negotiated.routes(),
            "if the two routers agreed, nothing below would be evidence of anything"
        );

        let shipped = plan_from_netlist(&netlist, &PortPlacements::default())
            .expect("verilog:and4 plans");
        assert_eq!(
            shipped.routes(),
            rip_up.routes(),
            "`plan_from_netlist` must still be the rip-up router, cell for cell"
        );
    }

    /// The priced set is exactly the set of ways one net can refuse a cell to
    /// another, and this is what says so.
    ///
    /// [`exclusion_zone`] is the whole of what the negotiation trades in: a
    /// price is charged where a foreign net's wire would have made
    /// [`anchor_is_free_for`] refuse, and nowhere else. Too small and the loop
    /// converges on a plan the router would not accept; too large and it
    /// demands separation physics does not, and refuses circuits that route.
    ///
    /// So the equivalence is swept rather than argued: one foreign wire cell is
    /// placed at every offset in a `5 x 5 x 5` box around the cell under test,
    /// with the floor that wire's realisation would lay, and the rule's verdict
    /// is compared against membership of the zone. 125 rows, both directions.
    #[test]
    fn the_priced_zone_is_exactly_what_a_foreign_wire_makes_anchor_is_free_for_refuse() {
        let cell = Anchor { x: 40, y: 4, z: 40 };
        // Far enough away that none of `anchor_is_free_for`'s three exemptions
        // -- start, goal, and the socket's own support -- can fire.
        let elsewhere = Anchor { x: 0, y: 0, z: 0 };
        let zone: BTreeSet<Anchor> = exclusion_zone(cell).into_iter().collect();

        let mut disagreements = Vec::new();
        for dx in -2..=2 {
            for dy in -2..=2 {
                for dz in -2..=2 {
                    let foreign = Anchor {
                        x: cell.x + dx,
                        y: cell.y + dy,
                        z: cell.z + dz,
                    };
                    let mut reservation = Reservation::new();
                    // Exactly what `reserve_path` writes for one cell of a
                    // foreign net: the wire, and the stone floor under it.
                    reservation.insert(foreign, "theirs", Occupancy::Wire);
                    reservation.insert(
                        Anchor { y: foreign.y - 1, ..foreign },
                        "theirs",
                        Occupancy::Stone,
                    );
                    let free = anchor_is_free_for(
                        cell,
                        elsewhere,
                        elsewhere,
                        elsewhere,
                        "mine",
                        &reservation,
                    );
                    if free == zone.contains(&foreign) {
                        disagreements.push(format!(
                            "  offset ({dx}, {dy}, {dz}): the zone says {} and the rule says {}",
                            if zone.contains(&foreign) { "priced" } else { "free" },
                            if free { "free" } else { "refused" },
                        ));
                    }
                }
            }
        }
        assert!(
            disagreements.is_empty(),
            "the priced set and the rule must agree cell for cell:\n{}",
            disagreements.join("\n")
        );
    }

    /// **A plan in which any cell is shared is illegal and must never be
    /// returned**, and this is the guard that says so rather than the loop's
    /// own bookkeeping.
    ///
    /// Three routes, hand-built so nothing about the placer or the search is
    /// involved: a straight run, a second run two cells away, and a third one
    /// cell away. The first pair is legal and the sweep admits it; the second
    /// is inside `keep_out` and the sweep refuses it, naming the cell.
    #[test]
    fn a_plan_where_two_nets_stand_within_keep_out_is_refused_by_the_sweep() {
        let run = |x: i32| -> Vec<Anchor> {
            (0..6).map(|z| Anchor { x, y: 1, z }).collect()
        };
        let apart = PlanCandidate::new(
            Vec::new(),
            vec![Route::new("a", run(10)), Route::new("b", run(13))],
        );
        negotiation_left_nothing_shared(&apart)
            .expect("three cells apart is three cells apart");

        let beside = PlanCandidate::new(
            Vec::new(),
            vec![Route::new("a", run(10)), Route::new("b", run(11))],
        );
        let refused = negotiation_left_nothing_shared(&beside)
            .expect_err("two nets one cell apart are inside each other's keep_out");
        assert!(
            refused.to_string().contains("spacing violation"),
            "the refusal must name the cell and both nets: {refused}"
        );

        // And the literal case `verify_spacing` already answers, kept here so
        // this guard is known to cover it rather than assumed to.
        let same = PlanCandidate::new(
            Vec::new(),
            vec![Route::new("a", run(10)), Route::new("b", run(10))],
        );
        negotiation_left_nothing_shared(&same)
            .expect_err("one cell, two nets, is the violation this whole design is about");
    }

    /// Sharing is a tool the search uses mid-iteration, and a budget that runs
    /// out with cells still contested is a failure, not a plan.
    ///
    /// `verilog:and4` needs four iterations (`8 -> 5 -> 6 -> 0`). Given one, the
    /// router must fail the way it fails today -- an error -- rather than hand
    /// back the iteration-0 plan, whose nets run through each other.
    #[test]
    fn a_negotiation_that_has_not_converged_returns_an_error_and_not_a_plan() {
        let circuit = crate::circuits::verilog::find("verilog:and4")
            .expect("the catalog ships verilog:and4");
        let (gate_level, _) = circuit.baked_netlist();
        let netlist =
            crate::compile::lowering::lower(&gate_level).expect("verilog:and4 must lower");
        let placement = relaxed_placement(&netlist, &PortPlacements::default(), SHIPPING_AXES)
            .expect("verilog:and4 places");
        let snapped = relax::snap(&placement).expect("verilog:and4 snaps");
        let bare = candidate_from_snapped(&netlist, &PortPlacements::default(), &snapped);

        let mut trace = Vec::new();
        let one = negotiate(bare.clone(), &netlist, 1, PresentSchedule::SHIPPING, &mut trace);
        assert_eq!(
            trace.iter().map(|round| round.contested).collect::<Vec<_>>(),
            vec![8],
            "iteration 0 is priced at zero, so the nets run straight through each other"
        );
        assert!(
            one.is_err(),
            "eight contested cells is not a plan, however short its routes are"
        );

        let mut trace = Vec::new();
        negotiate(bare, &netlist, NEGOTIATION_ROUNDS, PresentSchedule::SHIPPING, &mut trace)
            .expect("four iterations is enough for verilog:and4");
        assert_eq!(
            trace.iter().map(|round| round.contested).collect::<Vec<_>>(),
            vec![8, 5, 6, 0],
            "and the sequence it converges along is the evidence it negotiated at all"
        );
    }

    /// What both routers do to all six condition circuits, side by side.
    ///
    /// The measurement behind every number this branch's negotiation work
    /// quotes, in the tree because rule 4 says a cited number needs a
    /// reproducible method in it. Asserts nothing; `--ignored --nocapture`.
    ///
    /// **Verifying and computing are two columns, not one.** A plan the judge
    /// passes is not thereby a circuit, and this branch has shipped two that
    /// passed every invariant and computed the wrong function, so every row
    /// that verifies is also driven through the real `Simulator` for its whole
    /// truth table and its worst settle.
    ///
    /// Re-measured 2026-08-18 with the strength walk's block-mediated step
    /// repaired (`the_strength_verifier_follows_a_repeater_that_feeds_a_climb`),
    /// `--release`. The **only** column the repair moved is `full_adder`
    /// negotiated, from `VERIFY REFUSED` to what is below; no emission changed,
    /// so no block count or tick count could:
    ///
    /// | circuit | rip-up | negotiated | contested |
    /// |---|---|---|---|
    /// | and4 | 104 cells, 232 blocks, verifies, truth 16/16, 14 ticks | 106 cells, 236 blocks, verifies, truth 16/16, 14 ticks | 6 5 4 0 |
    /// | full_adder | 507 cells, 1,065 blocks, verifies, truth 8/8, 46 ticks | 522 cells, 1,096 blocks, verifies, truth 8/8, 44 ticks (**was VERIFY REFUSED**) | 108 43 15 0 |
    /// | verilog:and4 | 131 cells, 290 blocks, verifies, truth 16/16, 14 ticks | 129 cells, 286 blocks, verifies, truth 16/16, 14 ticks | 8 5 6 0 |
    /// | segment_a | ERR 36.3s | ERR 43.1s | 474 337 163 109 17 61 118 41 22 27 16 10 70 38 16 14 15 15 16 15 15 16 17 15 18 15 11 12 11 11 12 11 |
    /// | seven_segment | ERR 20.7s | ERR 195.2s | 1155 1013 600 377 290 187 122 163 164 57 164 135 160 136 83 73 78 241 226 80 169 203 93 48 143 98 88 34 179 26 122 97 |
    ///
    /// Both `ERR` rows are the routers' own failures on this schedule, not the
    /// judge's: neither plan ever reaches `verify_candidate`. `segment_a` used
    /// to route under `PresentSchedule::starting_at(8)` -- into nine latched
    /// rings and a sealed climb -- and since 2026-08-19 the ring and lid rules
    /// refuse that schedule's every candidate too; the history and the refusal
    /// are pinned in `negotiated_segment_a_routes_and_still_does_not_compute`.
    ///
    /// Three things this says, and the second is the one the spec asked for.
    ///
    /// 1. **The mechanism works on the small circuits.** All three converge, in
    ///    four iterations or fewer, all three verify, and all three compute
    ///    their function in the simulator.
    /// 2. **`segment_a` does NOT converge with the four dropped constraints
    ///    restored.** The probe in
    ///    `docs/superpowers/specs/2026-08-15-routing-at-scale.md` §5.1 converged
    ///    it at iteration 7 (`260 -> 66 -> 37 -> 21 -> 9 -> 6 -> 2 -> 0`) with
    ///    the strength budget, staircase clearance, the terminal guards and the
    ///    socket pre-claim dropped. With all four back it falls from 474 to
    ///    about 11 and then oscillates between 10 and 18 for twenty iterations.
    ///    §6 names re-running the probe with them restored as the *first*
    ///    milestone of this work and says that if `segment_a` no longer
    ///    converges the document's recommendation is wrong. **This is that
    ///    measurement, and it is one schedule, not a swept one** -- §8 item 5
    ///    already records that the schedule was never swept, and that caveat
    ///    now applies to this row as well as to the probe's `seven_segment`.
    /// 3. **`full_adder` computes `full_adder`, on all eight vectors, in the
    ///    real `Simulator`, and `verify_signal_strength` used to refuse it
    ///    anyway.** That was a defect in the judge and it is fixed; see
    ///    `the_strength_verifier_follows_a_repeater_that_feeds_a_climb`.
    #[test]
    #[ignore = "measurement harness: asserts nothing, routes six circuits twice, about four minutes"]
    fn what_the_two_routers_do_to_the_six_condition_circuits() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };
        use std::time::Instant;

        // The netlist *and* the signal names its declared outputs lower to --
        // guessing the latter is how the first version of this harness reported
        // `verilog:and4` as computing the wrong function when it computes the
        // right one.
        let lowered = |name: &str| -> (Netlist, Vec<String>) {
            let circuit = crate::circuits::verilog::find(name).expect("the catalog has it");
            let (gate_level, labels) = circuit.baked_netlist();
            (
                crate::compile::lowering::lower(&gate_level).expect("it lowers"),
                labels.into_iter().map(|(_, signal)| signal).collect(),
            )
        };
        let (verilog_and4, verilog_and4_outputs) = lowered("verilog:and4");
        let (and4, and4_output) = build_and4_netlist();
        let (adder, adder_outputs) = build_full_adder_netlist();
        let (segment_a, segment_a_output) = build_single_segment_netlist(0);
        let (decoder, decoder_outputs) = build_seven_segment_netlist();
        let cases: Vec<ConditionCircuit> = vec![
            ConditionCircuit {
                name: "and4",
                netlist: and4,
                inputs: &crate::circuits::and4::INPUT_NAMES[..],
                outputs: vec![and4_output],
                expected: and4_expected,
            },
            ConditionCircuit {
                name: "full_adder",
                netlist: adder,
                inputs: &crate::circuits::full_adder::INPUT_NAMES[..],
                outputs: vec![
                    adder_outputs["sum"].clone(),
                    adder_outputs["cout"].clone(),
                ],
                expected: full_adder_expected,
            },
            ConditionCircuit {
                name: "verilog:and4",
                netlist: verilog_and4,
                inputs: &crate::circuits::and4::INPUT_NAMES[..],
                outputs: verilog_and4_outputs,
                expected: and4_expected,
            },
            ConditionCircuit {
                name: "segment_a",
                netlist: segment_a,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: vec![segment_a_output],
                expected: segment_a_expected,
            },
            ConditionCircuit {
                name: "seven_segment",
                netlist: decoder,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: crate::circuits::seven_segment::SEGMENT_NAMES
                    .iter()
                    .map(|name| decoder_outputs[name].clone())
                    .collect(),
                expected: seven_segment_expected,
            },
        ];

        for case in &cases {
            let (name, netlist) = (case.name, &case.netlist);
            let placement = relaxed_placement(netlist, &PortPlacements::default(), SHIPPING_AXES)
                .expect("every circuit here places");
            let snapped = relax::snap(&placement).expect("and snaps");
            let bare = candidate_from_snapped(netlist, &PortPlacements::default(), &snapped);

            let started = Instant::now();
            let rip_up = route_every_net(bare.clone(), netlist, RIP_UP_ROUNDS);
            let rip_up_seconds = started.elapsed().as_secs_f64();

            let started = Instant::now();
            let mut trace = Vec::new();
            let negotiated = negotiate(bare, netlist, NEGOTIATION_ROUNDS, PresentSchedule::SHIPPING, &mut trace);
            let negotiated_seconds = started.elapsed().as_secs_f64();

            let report = |plan: &Result<PlanCandidate, PlannerError>, seconds: f64| match plan {
                Err(error) => format!("ERR {seconds:.1}s {error}"),
                Ok(plan) => {
                    let cells: usize =
                        plan.routes().iter().map(|route| route.anchors().len()).sum();
                    let verdict = match verify_candidate(plan, netlist) {
                        Err(error) => format!("VERIFY REFUSED: {error}"),
                        Ok(()) => {
                            let realised =
                                emit_candidate(plan, netlist, candidate_world_size(plan))
                                    .expect("a verified plan realises");
                            let (sx, sy, sz) = realised.world.size();
                            let mut blocks = 0usize;
                            for x in 0..sx {
                                for y in 0..sy {
                                    for z in 0..sz {
                                        if realised.world.get(x, y, z).kind
                                            != crate::redstone::world::block::BlockKind::Air
                                        {
                                            blocks += 1;
                                        }
                                    }
                                }
                            }
                            // The judge passing it is not the same claim as
                            // the simulator agreeing with it, and this branch
                            // has shipped circuits where those came apart. Both
                            // columns, always.
                            let compiled = compile::CompiledCircuit {
                                world: realised.world,
                                input_positions: realised.ports.input_positions,
                                output_positions: realised.ports.output_positions,
                                gate_output_positions: realised.ports.gate_output_positions,
                                gate_facings: (0..netlist.gates.len())
                                    .map(|g| plan.facing_of(g))
                                    .collect(),
                                planner_kind: compile::PlannerKind::Unified3d,
                                legacy_emission: None,
                            };
                            let truth = simulated_truth_table(
                                &compiled,
                                case.inputs,
                                &case.outputs,
                                case.expected,
                            );
                            let ticks = worst_settle_game_ticks(&compiled, case.inputs);
                            format!(
                                "verifies, {blocks} blocks, truth {}, worst settle {}",
                                match &truth {
                                    Ok(vectors) => format!("Ok ({vectors} vectors)"),
                                    Err(error) => format!("**WRONG** {error}"),
                                },
                                match &ticks {
                                    Ok(ticks) => format!("{ticks} game ticks"),
                                    Err(error) => format!("**{error}**"),
                                },
                            )
                        }
                    };
                    format!("ok {seconds:.1}s {cells} cells, {verdict}")
                }
            };

            eprintln!(
                "{name}\n  rip-up      {}\n  negotiated  {}\n  contested   {:?}\n  unlaid      {:?}",
                report(&rip_up, rip_up_seconds),
                report(&negotiated, negotiated_seconds),
                trace.iter().map(|round| round.contested).collect::<Vec<_>>(),
                trace.iter().map(|round| round.unlaid).collect::<Vec<_>>(),
            );
        }
    }

    /// A defect that **was** in the verifier, found by the negotiated router
    /// and now fixed. This test is the record of both halves.
    ///
    /// `full_adder` routed by negotiation computes `full_adder` -- all eight
    /// vectors, in the real `Simulator`, which is the oracle every circuit in
    /// this tree is judged against -- and `verify_signal_strength` used to
    /// refuse it:
    ///
    /// ```text
    /// signal-strength violation: net `cin` never delivers a non-zero signal
    /// to gate `g16`'s support block (57, 1, 91)
    /// ```
    ///
    /// **The mechanism, traced rather than guessed.** `cin`'s second branch
    /// climbs, and `realise_branch_from` puts a mandatory refresh on the last
    /// flat cell before every climb -- here a repeater at `(55, 1, 108)` facing
    /// `-z`. Its output lands on `(55, 1, 107)`, which is the **floor** of the
    /// climbing cell `(55, 2, 107)`, and a strongly powered floor drives the
    /// dust standing on it (`docs/derived/coupling-mechanisms.md` mechanism 3,
    /// 31 measured couplings). `compile::net_signal_strength` had that rule
    /// written into it and could never reach it: its `deliver` decided whether
    /// to walk onward from a cell by asking `own_cells`, and `own_cells` is
    /// `verify_spacing`'s reservation, which holds route **anchors** -- an
    /// anchor holds dust or a repeater, never a block, and a route's floor is
    /// not an anchor at all. So no conductive block was ever enqueued, the
    /// radiate-from-a-block arm never fired, and every cell of the branch past
    /// the climb read zero.
    ///
    /// Latent, not new: nothing about negotiation creates the geometry, and
    /// `plan_from_netlist` has been able to produce it since Task 10. What the
    /// negotiated router does is *reach* it, because it separates nets by going
    /// over them where the rip-up router separates them in the plane.
    ///
    /// **The fix, and why it is not the one-line one.** The one-line change
    /// that removes the symptom is `|| own_cells.contains(&target.up())` --
    /// "or this is the floor of one of the route's own cells". What is actually
    /// wrong is that `net_signal_strength` had a second, divergent copy of a
    /// step relation `net_reach` twenty lines above it already gets right, so
    /// the fix is that they now share it: `structural_output_in_world` is one
    /// statement of the block half of the physics, and `deliver` decides
    /// acceptance and continuation from **what stands at the target and which
    /// of `PowerOutput`'s two channels arrived**, never from list membership.
    /// `compile::strength_differential` is the measurement that says so, in both
    /// directions, cell by cell.
    ///
    /// What the divergence had also cost, and what the one-liner would have
    /// left: `net_reach` corrects the dust rule with `dust_powers_block_toward`
    /// in all six directions and the strength walk looped `HORIZONTAL` only, so
    /// the block a run **stands on** was invisible to one of the two.
    #[test]
    fn the_strength_verifier_follows_a_repeater_that_feeds_a_climb() {
        use crate::circuits::full_adder::{build_full_adder_netlist, INPUT_NAMES};

        let (netlist, outputs) = build_full_adder_netlist();
        let sinks = vec![outputs["sum"].clone(), outputs["cout"].clone()];
        let placement = relaxed_placement(&netlist, &PortPlacements::default(), SHIPPING_AXES)
            .expect("full_adder places");
        let snapped = relax::snap(&placement).expect("full_adder snaps");
        let bare = candidate_from_snapped(&netlist, &PortPlacements::default(), &snapped);
        let plan = route_negotiated(bare, &netlist, NEGOTIATION_ROUNDS, PresentSchedule::SHIPPING)
            .expect("full_adder converges in three iterations");

        // The repeater the walk could not see past, named rather than
        // described -- the geometry is still exactly the one the refusal was
        // about, so this test cannot pass by the plan having changed shape.
        let cin = route_named(&plan, "cin");
        let climb_refresh = cin
            .anchors()
            .iter()
            .zip(cin.realisation().iter())
            .zip(cin.anchors().iter().skip(1))
            .find(|((_, block), next)| {
                block.kind == crate::redstone::world::block::BlockKind::Repeater && next.y > 1
            })
            .map(|((anchor, _), next)| (*anchor, *next));
        assert!(
            climb_refresh.is_some(),
            "the mechanism needs a repeater whose next cell climbs; `cin` lays {:?}",
            cin.anchors()
        );

        verify_candidate(&plan, &netlist)
            .expect("the judge no longer refuses the plan it used to refuse at (57, 1, 91)");

        // And the circuit is right, which is what made this the verifier's
        // finding and not the router's.
        let realised = emit_candidate(&plan, &netlist, candidate_world_size(&plan))
            .expect("a verified plan realises");
        let compiled = compile::CompiledCircuit {
            world: realised.world,
            input_positions: realised.ports.input_positions,
            output_positions: realised.ports.output_positions,
            gate_output_positions: realised.ports.gate_output_positions,
            gate_facings: (0..netlist.gates.len()).map(|g| plan.facing_of(g)).collect(),
            planner_kind: compile::PlannerKind::Unified3d,
            legacy_emission: None,
        };
        assert_eq!(
            simulated_truth_table(&compiled, &INPUT_NAMES[..], &sinks, full_adder_expected),
            Ok(8),
            "the simulator is the oracle, and it says this circuit is a full adder"
        );
    }

    /// **What the fixed judge does not unblock -- and, since 2026-08-19, what
    /// the router itself refuses.**
    ///
    /// The history, in order, because each layer was measured before the next
    /// existed:
    ///
    /// 1. The case for fixing the judge included a claim: that a negotiated
    ///    router at `PresentSchedule::starting_at(8)` *routes* `segment_a` --
    ///    2,127 cells, all 47 nets laid, no shared cell -- and that the only
    ///    thing refusing it was the strength walk.
    /// 2. Measured with the judge fixed: it routed -- 2,127 cells, 4,356
    ///    blocks, box 91x5x95, worst settle 42 game ticks, against legacy's
    ///    6,416 blocks and 68 ticks -- **and it did not compute**: 8 of 16
    ///    vectors wrong in the real `Simulator`, dark exactly where segment
    ///    `a` should light. `g0` closed a ring -- the repeater at (93,4,110)
    ///    feeding its own input through five of its own cells, pre-lit by
    ///    `emit`, latched at every vector -- and `g0`'s real feed died at the
    ///    climb (86,1,109) -> (87,2,109), sealed by the floor of `g0`'s own
    ///    third branch. Not one ring: nine, across six nets, with 751 cells
    ///    still powered after every source was deleted
    ///    (`rings_and_wire_under_wire_swept_across_every_buildable_plan`,
    ///    run at 78d5185 with the rules disabled).
    /// 3. Those two shapes are game physics, not contention -- a route that
    ///    latches is a different circuit, and a climb with a conductive lid
    ///    does not conduct -- so they are hard refusals now: the ring rule
    ///    ([`ring_closed_in`], in [`lay_net`]) and the lid rule
    ///    ([`anchor_is_free_for`]'s air arm). **This schedule's every
    ///    surviving candidate contained one of them, so the router now
    ///    returns an error where it used to return a latch.** That is the
    ///    right trade by standing rule 7: a plan that routes and does not
    ///    compute is worth nothing.
    ///
    /// What this test pins: `starting_at(8)` yields **no plan** -- the
    /// refusal is the router's own, before the judge is ever asked -- and the
    /// recorded failure is one of the two rules by name. If this ever turns
    /// green-with-a-plan again, that is news: it means negotiation found a
    /// ring-free, lid-safe `segment_a`, and the plan deserves the full
    /// measurement battery (rings, latch oracle, truth table) before anyone
    /// calls it progress.
    ///
    /// NOT MEASURED: whether any other schedule routes `segment_a` under the
    /// two rules (no sweep, unchanged from the ledger), and whether a bigger
    /// iteration budget would converge it.
    #[test]
    #[ignore = "measurement: runs segment_a through 32 negotiation iterations, about 40 seconds"]
    fn negotiated_segment_a_routes_and_still_does_not_compute() {
        use crate::circuits::seven_segment::build_single_segment_netlist;

        let (netlist, _output) = build_single_segment_netlist(0);
        let outcome = plan_negotiated_on_schedule(
            &netlist,
            &PortPlacements::default(),
            NEGOTIATION_ROUNDS,
            PresentSchedule::starting_at(8),
        );

        match outcome {
            Err(error) => {
                let refusal = error.to_string();
                eprintln!(
                    "negotiated segment_a at PresentSchedule::starting_at(8): no plan.\n  \
                     last recorded failure: {refusal}"
                );
                assert!(
                    refusal.contains("closes a ring") || refusal.contains("did not separate"),
                    "the refusal should be the ring rule's or plain non-convergence, \
                     not some new failure mode: {refusal}"
                );
            }
            Ok(plan) => {
                let cells: usize = plan.routes().iter().map(|route| route.anchors().len()).sum();
                panic!(
                    "segment_a routed under the ring and lid rules ({cells} cells): \
                     that is news, not a regression -- measure it (rings, latch oracle, \
                     truth table) before celebrating, then rewrite this test to pin it"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Ship review of the negotiated router (2026-08-18)
    //
    // Written to decide GO/NO_GO against pre-registered criteria. Nothing here
    // is tuned; everything asserts or prints what it measured.

    /// **The shared-cell gate**, derived from [`anchor_is_free_for`] itself
    /// rather than from [`exclusion_zone`].
    ///
    /// `negotiation_left_nothing_shared` re-derives contention from
    /// `exclusion_zone`, which is the same set the price is charged over -- so
    /// it cannot catch a zone that is wrong, only bookkeeping that is. This
    /// asks the *rule* instead: for each route, lay every other net's wire and
    /// the stone floor realisation puts under it into a bare `Reservation`
    /// exactly as `reserve_path` would, and then ask `anchor_is_free_for`
    /// whether this net's cells were ever free. Start, goal and terminal
    /// support are pointed far away so none of the rule's three exemptions can
    /// fire.
    fn cells_the_rule_would_have_refused(candidate: &PlanCandidate) -> Vec<String> {
        // A socket is a primitive's own cell: every net is kept out of it by
        // `reserve_primitives` and the pre-claims, and both routers append it
        // to the path after the search. It is not a negotiated cell, so it is
        // out of scope here -- the same scoping
        // `negotiation_left_nothing_shared` uses.
        let sockets: BTreeSet<Anchor> = candidate
            .routes
            .iter()
            .flat_map(|route| route.terminals.iter().map(|terminal| terminal.sink.anchor))
            .collect();
        let wire_of = |route: &Route| -> Vec<Anchor> {
            route
                .anchors
                .iter()
                .copied()
                .filter(|anchor| !sockets.contains(anchor))
                .collect()
        };
        let elsewhere = Anchor {
            x: -10_000,
            y: -10_000,
            z: -10_000,
        };

        let mut faults = Vec::new();
        for (mine, route) in candidate.routes.iter().enumerate() {
            let mut theirs = Reservation::new();
            for (other, foreign) in candidate.routes.iter().enumerate() {
                if other == mine {
                    continue;
                }
                for cell in wire_of(foreign) {
                    theirs.insert(cell, &foreign.id, Occupancy::Wire);
                    theirs.insert(
                        Anchor {
                            y: cell.y - 1,
                            ..cell
                        },
                        &foreign.id,
                        Occupancy::Stone,
                    );
                }
            }
            for cell in wire_of(route) {
                if !anchor_is_free_for(cell, elsewhere, elsewhere, elsewhere, &route.id, &theirs) {
                    let mut around = Vec::new();
                    for neighbour in exclusion_zone(cell) {
                        if let Some(owner) = theirs.owner(&neighbour) {
                            around.push(format!(
                                "`{owner}` at ({}, {}, {})",
                                neighbour.x, neighbour.y, neighbour.z
                            ));
                        }
                    }
                    faults.push(format!(
                        "({}, {}, {}) laid by `{}` against {}",
                        cell.x,
                        cell.y,
                        cell.z,
                        route.id,
                        around.join(" + ")
                    ));
                }
            }
        }
        faults
    }

    /// NO_GO criterion 1: **no returned plan may have a cell shared by two
    /// nets** -- and "shared" is the router's own rule, not just identity.
    #[test]
    fn no_plan_either_router_returns_shares_a_cell() {
        use crate::circuits::full_adder::build_full_adder_netlist;

        let lowered = |name: &str| {
            let circuit = crate::circuits::verilog::find(name).expect("the catalog has it");
            let (gate_level, _) = circuit.baked_netlist();
            crate::compile::lowering::lower(&gate_level).expect("it lowers")
        };
        let cases: Vec<(&str, Netlist)> = vec![
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("verilog:and4", lowered("verilog:and4")),
        ];

        for (name, netlist) in &cases {
            for (label, router, budget) in [
                ("rip-up", RouterKind::RipUp, RIP_UP_ROUNDS),
                ("negotiated", RouterKind::Negotiated, NEGOTIATION_ROUNDS),
            ] {
                let plan =
                    plan_from_netlist_with_router(netlist, &PortPlacements::default(), budget, router)
                        .unwrap_or_else(|error| panic!("{name} must route through {label}: {error}"));
                let faults = cells_the_rule_would_have_refused(&plan);
                assert!(
                    faults.is_empty(),
                    "{name} via {label} returned a plan whose nets stand inside each other:\n  {}",
                    faults.join("\n  ")
                );
            }
        }
    }

    /// Rule 2: the gate above must be able to fail against the defect it names.
    #[test]
    fn the_shared_cell_gate_is_able_to_fail() {
        let run = |x: i32| -> Vec<Anchor> { (0..6).map(|z| Anchor { x, y: 1, z }).collect() };

        let apart = PlanCandidate::new(
            Vec::new(),
            vec![Route::new("a", run(10)), Route::new("b", run(13))],
        );
        assert_eq!(
            cells_the_rule_would_have_refused(&apart),
            Vec::<String>::new(),
            "three cells apart is legal and the gate must admit it"
        );

        let beside = PlanCandidate::new(
            Vec::new(),
            vec![Route::new("a", run(10)), Route::new("b", run(11))],
        );
        assert_eq!(
            cells_the_rule_would_have_refused(&beside).len(),
            12,
            "six cells of each net, each inside the other's keep_out: {:?}",
            cells_the_rule_would_have_refused(&beside)
        );

        let same = PlanCandidate::new(
            Vec::new(),
            vec![Route::new("a", run(10)), Route::new("b", run(10))],
        );
        assert_eq!(
            cells_the_rule_would_have_refused(&same).len(),
            12,
            "one cell, two nets, is the violation the whole design is about"
        );

        // And the same gate against a plan a real router produced, with one
        // cell of one net handed to another. This is what says the gate would
        // catch a negotiator that returned an overused cell, rather than only a
        // hand-built fixture.
        let circuit =
            crate::circuits::verilog::find("verilog:and4").expect("the catalog ships verilog:and4");
        let (gate_level, _) = circuit.baked_netlist();
        let netlist = crate::compile::lowering::lower(&gate_level).expect("verilog:and4 lowers");
        let mut plan = plan_from_netlist_with_router(
            &netlist,
            &PortPlacements::default(),
            NEGOTIATION_ROUNDS,
            RouterKind::Negotiated,
        )
        .expect("verilog:and4 routes through the negotiated router");
        assert!(
            cells_the_rule_would_have_refused(&plan).is_empty(),
            "the plan is clean before the injection"
        );
        let stolen = plan.routes[1].anchors[0];
        plan.routes[0].anchors.push(stolen);
        assert!(
            !cells_the_rule_would_have_refused(&plan).is_empty(),
            "one cell given to two nets must be visible to the gate"
        );
    }

    /// Worst-case settle, in game ticks, over the same sweep the truth table
    /// runs: toggle every lever through every input combination and take the
    /// longest single transition.
    fn worst_settle_game_ticks(compiled: &CompiledCircuit, inputs: &[&str]) -> Result<u64, String> {
        const MAX_TICKS: u64 = 2000;
        let mut levers = Vec::with_capacity(inputs.len());
        for name in inputs {
            match compiled.input_positions.get(*name) {
                Some(position) => levers.push(*position),
                None => return Err(format!("no lever for input `{name}`")),
            }
        }
        let mut simulator = crate::redstone::simulator::Simulator::new(compiled.world.clone());
        simulator
            .run_until_stable(MAX_TICKS)
            .map_err(|error| format!("did not settle before the sweep: {error:?}"))?;

        let mut worst = 0u64;
        for combination in 0..(1usize << inputs.len()) {
            let bits: Vec<bool> = (0..inputs.len())
                .map(|index| (combination >> (inputs.len() - 1 - index)) & 1 == 1)
                .collect();
            for (position, &bit) in levers.iter().zip(bits.iter()) {
                let mut state = simulator.world().get(position.0, position.1, position.2).clone();
                if state.lit == bit {
                    continue;
                }
                state.lit = bit;
                simulator.world_mut().set(position.0, position.1, position.2, state);
                let started = simulator.current_tick();
                simulator
                    .run_until_stable(MAX_TICKS)
                    .map_err(|error| format!("did not settle at {bits:?}: {error:?}"))?;
                worst = worst.max(simulator.current_tick() - started);
            }
        }
        Ok(worst)
    }

    fn blocks_in(world: &crate::redstone::world::storage::World) -> usize {
        let (sx, sy, sz) = world.size();
        let mut blocks = 0usize;
        for x in 0..sx {
            for y in 0..sy {
                for z in 0..sz {
                    if world.get(x, y, z).kind != crate::redstone::world::block::BlockKind::Air {
                        blocks += 1;
                    }
                }
            }
        }
        blocks
    }

    /// The ship review's main panel: all six condition circuits through the
    /// **real** `compile()`, whichever router [`SHIPPING_ROUTER`] names.
    ///
    /// Per circuit: what the planner path alone does (`plan_from_netlist` plus
    /// `verify_candidate`, no fallback -- this is where a router regression is
    /// visible), then what `compile` ships: which placer, blocks, the full
    /// truth table through the real simulator, and worst settle in game ticks.
    ///
    /// Run once with `SHIPPING_ROUTER = RipUp` and once with `Negotiated`.
    /// Asserts nothing; `--ignored --nocapture`.
    #[test]
    #[ignore = "measurement harness: compiles and simulates all six condition circuits"]
    fn the_ship_review_panel() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist, SEGMENT_NAMES,
        };
        use crate::compile::lowering::{lower, lower_optimised};
        use std::time::Instant;

        let (and4, and4_output) = build_and4_netlist();
        let (adder, adder_outputs) = build_full_adder_netlist();
        let (segment_a, segment_a_output) = build_single_segment_netlist(0);
        let (decoder, decoder_outputs) = build_seven_segment_netlist();
        let lowered_verilog = |name: &str, optimised: bool| -> (Netlist, Vec<String>) {
            let circuit = crate::circuits::verilog::find(name).expect("in the catalog");
            let (netlist, labels) = circuit.baked_netlist();
            let lowered = if optimised { lower_optimised(&netlist) } else { lower(&netlist) }
                .expect("it lowers");
            (lowered, labels.into_iter().map(|(_, signal)| signal).collect())
        };
        let (verilog_and4, verilog_and4_outputs) = lowered_verilog("verilog:and4", false);
        let (verilog_decoder, verilog_decoder_outputs) =
            lowered_verilog("verilog:seven_segment", true);

        let cases = [
            ConditionCircuit {
                name: "and4",
                netlist: and4,
                inputs: &crate::circuits::and4::INPUT_NAMES[..],
                outputs: vec![and4_output],
                expected: and4_expected,
            },
            ConditionCircuit {
                name: "full_adder",
                netlist: adder,
                inputs: &crate::circuits::full_adder::INPUT_NAMES[..],
                outputs: vec![adder_outputs["sum"].clone(), adder_outputs["cout"].clone()],
                expected: full_adder_expected,
            },
            ConditionCircuit {
                name: "segment_a",
                netlist: segment_a,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: vec![segment_a_output],
                expected: segment_a_expected,
            },
            ConditionCircuit {
                name: "seven_segment",
                netlist: decoder,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: SEGMENT_NAMES.iter().map(|name| decoder_outputs[name].clone()).collect(),
                expected: seven_segment_expected,
            },
            ConditionCircuit {
                name: "verilog:and4",
                netlist: verilog_and4,
                inputs: &crate::circuits::and4::INPUT_NAMES[..],
                outputs: verilog_and4_outputs,
                expected: and4_expected,
            },
            ConditionCircuit {
                name: "verilog:seven_segment",
                netlist: verilog_decoder,
                inputs: &crate::circuits::seven_segment::INPUT_NAMES[..],
                outputs: verilog_decoder_outputs,
                expected: seven_segment_expected,
            },
        ];

        eprintln!("SHIPPING_ROUTER = {:?}", SHIPPING_ROUTER);
        for case in &cases {
            let started = Instant::now();
            let planned = match plan_from_netlist(&case.netlist, &PortPlacements::default()) {
                Err(error) => format!("ROUTE ERR {:.1}s: {error}", started.elapsed().as_secs_f64()),
                Ok(candidate) => {
                    let cells: usize =
                        candidate.routes().iter().map(|route| route.anchors().len()).sum();
                    let faults = cells_the_rule_would_have_refused(&candidate);
                    let shared = if faults.is_empty() {
                        "no shared cell".to_string()
                    } else {
                        format!("**{} SHARED CELLS** {}", faults.len(), faults.join("; "))
                    };
                    match verify_candidate(&candidate, &case.netlist) {
                        Err(error) => format!(
                            "routed {:.1}s {cells} cells, {shared}, VERIFY REFUSED: {error}",
                            started.elapsed().as_secs_f64()
                        ),
                        Ok(()) => format!(
                            "routed {:.1}s {cells} cells, {shared}, verifies",
                            started.elapsed().as_secs_f64()
                        ),
                    }
                }
            };

            let started = Instant::now();
            let shipped = match compile::compile(&case.netlist) {
                Err(error) => format!("ERR {:.1}s: {error}", started.elapsed().as_secs_f64()),
                Ok(compiled) => {
                    let seconds = started.elapsed().as_secs_f64();
                    let blocks = blocks_in(&compiled.world);
                    let truth = match simulated_truth_table(
                        &compiled,
                        case.inputs,
                        &case.outputs,
                        case.expected,
                    ) {
                        Ok(vectors) => format!("truth Ok ({vectors} vectors)"),
                        Err(error) => format!("**TRUTH TABLE WRONG**: {error}"),
                    };
                    let settle = match worst_settle_game_ticks(&compiled, case.inputs) {
                        Ok(ticks) => format!("worst settle {ticks} game ticks"),
                        Err(error) => format!("settle unmeasured: {error}"),
                    };
                    format!(
                        "{:?} {blocks} blocks, {truth}, {settle}, {seconds:.1}s",
                        compiled.planner_kind()
                    )
                }
            };

            eprintln!("{}\n  planner  {planned}\n  compile  {shipped}", case.name);
        }
    }

    /// `n1`, the smallest instance, printed rather than summarised.
    #[test]
    #[ignore = "measurement: prints both routers' n0 and n1"]
    fn what_n1_actually_is() {
        let (netlist, rip_up, negotiated) = verilog_and4_both_ways();
        for (label, plan) in [("rip-up", &rip_up), ("negotiated", &negotiated)] {
            for id in ["n0", "n1"] {
                let route = route_named(plan, id);
                eprintln!(
                    "{label} {id}: {} cells {:?}",
                    route.anchors().len(),
                    route.anchors().iter().map(|a| (a.x, a.y, a.z)).collect::<Vec<_>>()
                );
            }
            let cells: usize = plan.routes().iter().map(|r| r.anchors().len()).sum();
            eprintln!(
                "{label} whole circuit: {cells} cells, verify {:?}, shared {:?}",
                verify_candidate(plan, &netlist).map(|_| "ok"),
                cells_the_rule_would_have_refused(plan)
            );
        }
    }


    /// Criterion 3, the part the 125-row zone sweep does not answer: a
    /// **staircase**'s mandatory-air cells are a physics rule the negotiated
    /// router prices rather than refuses against foreign nets, and
    /// `negotiation_left_nothing_shared` re-derives only the `exclusion_zone`
    /// relation. So either the air relation is inside the zone relation, or the
    /// final guard has a hole.
    ///
    /// It is inside it, and this is what says so rather than an argument. For
    /// every staircase a route can take -- four horizontal directions by climb
    /// and descend -- every cell `staircase_clearance` demands, **and** the cell
    /// one above it whose floor would fill it, lies in the `exclusion_zone` of
    /// one of the two anchors of that very step. A foreign net that broke a
    /// staircase would therefore already be caught as a shared cell.
    #[test]
    fn every_staircase_clearance_cell_is_inside_the_zone_the_final_guard_sweeps() {
        let from = Anchor { x: 40, y: 4, z: 40 };
        let mut escapes = Vec::new();
        for to in neighbours(from) {
            if to.y == from.y {
                continue;
            }
            let zone: BTreeSet<Anchor> = exclusion_zone(from)
                .into_iter()
                .chain(exclusion_zone(to))
                .collect();
            for cell in staircase_clearance(from, to) {
                // The cell itself, taken by a foreign wire.
                if !zone.contains(&cell) {
                    escapes.push(format!("{to:?}: clearance cell {cell:?} is outside the zone"));
                }
                // And the cell a foreign wire would stand in to lay its floor
                // into this one -- which is how a route passing overhead seals
                // a climb without owning anything that conducts.
                let overhead = Anchor { y: cell.y + 1, ..cell };
                if !zone.contains(&overhead) {
                    escapes.push(format!(
                        "{to:?}: a wire at {overhead:?} lays its floor into clearance cell                          {cell:?} and is outside the zone"
                    ));
                }
            }
        }
        assert!(
            escapes.is_empty(),
            "the final guard sweeps `exclusion_zone` only, so anything here is a hole in it:
{}",
            escapes.join("
")
        );
    }


    // =======================================================================
    // ADVERSARIAL VERIFICATION HARNESS (2026-08-18, second reviewer).
    //
    // Deliberately shares NO code with the router or with the first reviewer's
    // gate. `cells_the_rule_would_have_refused` asks `anchor_is_free_for`;
    // `negotiation_left_nothing_shared` asks `exclusion_zone`. Both are the
    // compiler's own statements of the physics. This one is written out by hand
    // from `docs/derived/dust-join-relation.md` and from the two sentences the
    // planner repeats everywhere -- "every cell stands on a stone floor one
    // level below it" and "dust joins its four horizontal neighbours and each
    // of those one level up or down" -- so that a wrong `keep_out`, a wrong
    // `exclusion_zone` and a wrong `anchor_is_free_for` would all still be
    // caught here.

    /// One cell, as a bare triple -- deliberately not [`Anchor`], so this
    /// harness cannot accidentally reuse a planner helper that takes one.
    type ReviewCell = (i32, i32, i32);
    /// Net name -> the wire cells it owns, sockets dropped.
    type ReviewWires = Vec<(String, Vec<ReviewCell>)>;
    /// Net name -> every cell it fills, wire and stone floor alike.
    type ReviewFilled = Vec<(String, std::collections::BTreeSet<ReviewCell>)>;

    /// Every way two nets can collide, spelled out rather than looked up.
    ///
    /// Returns one line per fault. Sockets are excluded on both sides for the
    /// same reason the other two gates exclude them: a socket is a primitive's
    /// cell, hard-reserved for every net in every iteration of both routers.
    fn review_two_nets_on_one_cell(candidate: &PlanCandidate) -> Vec<String> {
        let sockets: std::collections::BTreeSet<ReviewCell> = candidate
            .routes
            .iter()
            .flat_map(|route| route.terminals.iter())
            .map(|terminal| {
                let a = terminal.sink.anchor;
                (a.x, a.y, a.z)
            })
            .collect();
        let wires: ReviewWires = candidate
            .routes
            .iter()
            .map(|route| {
                (
                    route.id.clone(),
                    route
                        .anchors
                        .iter()
                        .map(|a| (a.x, a.y, a.z))
                        .filter(|c| !sockets.contains(c))
                        .collect(),
                )
            })
            .collect();

        // Hand-written physics.
        let horizontally_adjacent =
            |a: ReviewCell, b: ReviewCell| (a.0 - b.0).abs() + (a.2 - b.2).abs() == 1;
        let floor_of = |c: ReviewCell| (c.0, c.1 - 1, c.2);

        let mut faults = Vec::new();
        for i in 0..wires.len() {
            for j in 0..wires.len() {
                if i == j {
                    continue;
                }
                let (mine, my_cells) = &wires[i];
                let (theirs, their_cells) = &wires[j];
                let their_set: std::collections::BTreeSet<ReviewCell> =
                    their_cells.iter().copied().collect();
                for &a in my_cells {
                    // 1. The literal violation: one cell, two nets' wire.
                    if i < j && their_set.contains(&a) {
                        faults.push(format!(
                            "SAME CELL   {a:?} is wire for both {mine} and {theirs}"
                        ));
                    }
                    // 2. My stone floor lands on their conductor and deletes it.
                    if their_set.contains(&floor_of(a)) {
                        faults.push(format!(
                            "FLOOR KILLS {mine}'s wire at {a:?} floors {:?}, {theirs}'s wire",
                            floor_of(a)
                        ));
                    }
                    // 3. Dust joins its four horizontal neighbours and each of
                    //    those one level up or down: two nets so placed are one
                    //    net.
                    if i < j {
                        for &b in their_cells {
                            if horizontally_adjacent(a, b) && (a.1 - b.1).abs() <= 1 {
                                faults.push(format!(
                                    "DUST JOIN   {mine} at {a:?} joins {theirs} at {b:?}"
                                ));
                            }
                        }
                    }
                }
            }
        }
        faults.sort();
        faults.dedup();
        faults
    }

    /// The staircase half, also hand-written: a climb needs the cell over the
    /// head of the cell it leaves to stay air, and a descent needs the cell it
    /// falls past to stay air. "Air" means no foreign wire and no foreign
    /// floor.
    fn review_foreign_wire_in_a_staircase(candidate: &PlanCandidate) -> Vec<String> {
        let sockets: std::collections::BTreeSet<ReviewCell> = candidate
            .routes
            .iter()
            .flat_map(|route| route.terminals.iter())
            .map(|t| (t.sink.anchor.x, t.sink.anchor.y, t.sink.anchor.z))
            .collect();
        let occupied: ReviewFilled = candidate
            .routes
            .iter()
            .map(|route| {
                let mut cells = std::collections::BTreeSet::new();
                for a in &route.anchors {
                    let c = (a.x, a.y, a.z);
                    if sockets.contains(&c) {
                        continue;
                    }
                    cells.insert(c);
                    cells.insert((c.0, c.1 - 1, c.2)); // its stone floor
                }
                (route.id.clone(), cells)
            })
            .collect();

        let mut faults = Vec::new();
        for (index, route) in candidate.routes.iter().enumerate() {
            for pair in route.anchors.windows(2) {
                let (p, q) = (pair[0], pair[1]);
                if q.y == p.y {
                    continue;
                }
                // `route.anchors` is the concatenation of this route's
                // branches, so consecutive entries are NOT always consecutive
                // cells: the seam between one branch's last cell and the next
                // branch's first is a pair that can differ in y while being
                // nowhere near each other. Measured on `segment_a` at
                // `present_term(0) = 8`, where three such seams were reported
                // as staircase faults and none of them was a staircase. A real
                // step is one cardinal move across and one level.
                if (p.x - q.x).abs() + (p.z - q.z).abs() != 1 || (p.y - q.y).abs() != 1 {
                    continue;
                }
                let must_be_air = if q.y > p.y {
                    (p.x, p.y + 1, p.z) // headroom over the climb
                } else {
                    (q.x, p.y, q.z) // the cell the drop falls past
                };
                for (other, cells) in occupied.iter().enumerate().filter_map(|(k, v)| {
                    if k == index {
                        None
                    } else {
                        Some(v)
                    }
                }) {
                    if cells.contains(&must_be_air) {
                        faults.push(format!(
                            "STAIR       {}'s step {:?}->{:?} needs {must_be_air:?} air, {other} fills it",
                            route.id,
                            (p.x, p.y, p.z),
                            (q.x, q.y, q.z)
                        ));
                    }
                }
            }
        }
        faults.sort();
        faults.dedup();
        faults
    }

    /// Rule 2 for the harness above: it must go red against the defect it
    /// names, on hand-built fixtures AND on a plan a real router produced.
    #[test]
    fn review_my_own_shared_cell_gate_can_fail() {
        let run = |x: i32| -> Vec<Anchor> { (0..6).map(|z| Anchor { x, y: 1, z }).collect() };
        let plan = |a: Vec<Anchor>, b: Vec<Anchor>| {
            PlanCandidate::new(Vec::new(), vec![Route::new("a", a), Route::new("b", b)])
        };

        assert!(
            review_two_nets_on_one_cell(&plan(run(10), run(13))).is_empty(),
            "three cells apart is legal; a gate that always fires proves nothing"
        );
        let beside = review_two_nets_on_one_cell(&plan(run(10), run(11)));
        assert!(
            beside.iter().any(|f| f.starts_with("DUST JOIN")),
            "one cell apart is a dust join: {beside:?}"
        );
        let same = review_two_nets_on_one_cell(&plan(run(10), run(10)));
        assert!(
            same.iter().any(|f| f.starts_with("SAME CELL")),
            "the same six cells twice is the violation itself: {same:?}"
        );
        let over: Vec<Anchor> = (0..6).map(|z| Anchor { x: 10, y: 2, z }).collect();
        let stacked = review_two_nets_on_one_cell(&plan(run(10), over));
        assert!(
            stacked.iter().any(|f| f.starts_with("FLOOR KILLS")),
            "a wire directly over another net's wire floors it away: {stacked:?}"
        );

        // And against a plan the negotiated router really returned.
        let circuit =
            crate::circuits::verilog::find("verilog:and4").expect("the catalog ships verilog:and4");
        let (gate_level, _) = circuit.baked_netlist();
        let netlist = crate::compile::lowering::lower(&gate_level).expect("verilog:and4 lowers");
        let mut real = plan_from_netlist_with_router(
            &netlist,
            &PortPlacements::default(),
            NEGOTIATION_ROUNDS,
            RouterKind::Negotiated,
        )
        .expect("verilog:and4 routes negotiated");
        assert!(
            review_two_nets_on_one_cell(&real).is_empty()
                && review_foreign_wire_in_a_staircase(&real).is_empty(),
            "the real plan is clean before the injection"
        );
        let stolen = real.routes[1].anchors[0];
        real.routes[0].anchors.push(stolen);
        assert!(
            !review_two_nets_on_one_cell(&real).is_empty(),
            "a cell handed to a second net must be visible"
        );
    }

    /// **NO_GO criterion 1, verified independently.** Every plan the negotiated
    /// router returns, on every circuit it can route, against a hand-written
    /// statement of the physics.
    #[test]
    #[ignore = "review harness: routes every circuit through both routers"]
    fn review_no_negotiated_plan_shares_a_cell() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };

        let lowered = |name: &str| {
            let circuit = crate::circuits::verilog::find(name).expect("the catalog has it");
            let (gate_level, _) = circuit.baked_netlist();
            match name {
                "verilog:seven_segment" => {
                    crate::compile::lowering::lower_optimised(&gate_level).expect("it lowers")
                }
                _ => crate::compile::lowering::lower(&gate_level).expect("it lowers"),
            }
        };
        let cases: Vec<(String, Netlist)> = vec![
            ("and4".to_string(), build_and4_netlist().0),
            ("full_adder".to_string(), build_full_adder_netlist().0),
            ("segment_a".to_string(), build_single_segment_netlist(0).0),
            ("seven_segment".to_string(), build_seven_segment_netlist().0),
            ("verilog:and4".to_string(), lowered("verilog:and4")),
            (
                "verilog:seven_segment".to_string(),
                lowered("verilog:seven_segment"),
            ),
        ];

        let mut any_plan = 0usize;
        let mut faults_seen = 0usize;
        for (name, netlist) in &cases {
            for (label, router, budget) in [
                ("rip-up    ", RouterKind::RipUp, RIP_UP_ROUNDS),
                ("negotiated", RouterKind::Negotiated, RIP_UP_ROUNDS),
                ("negotiated", RouterKind::Negotiated, TRIAL_RIP_UP_ROUNDS),
            ] {
                let started = std::time::Instant::now();
                match plan_from_netlist_with_router(
                    netlist,
                    &PortPlacements::default(),
                    budget,
                    router,
                ) {
                    Ok(plan) => {
                        any_plan += 1;
                        let shared = review_two_nets_on_one_cell(&plan);
                        let stairs = review_foreign_wire_in_a_staircase(&plan);
                        faults_seen += shared.len() + stairs.len();
                        let cells: usize = plan.routes.iter().map(|r| r.anchors.len()).sum();
                        eprintln!(
                            "{name:24} {label} budget {budget:3}  {cells:5} cells  {:3} shared-cell fault(s)  {:3} staircase fault(s)  {:.1}s",
                            shared.len(),
                            stairs.len(),
                            started.elapsed().as_secs_f64()
                        );
                        for fault in shared.iter().chain(stairs.iter()) {
                            eprintln!("      {fault}");
                        }
                    }
                    Err(error) => eprintln!(
                        "{name:24} {label} budget {budget:3}  NO PLAN  ({error})  {:.1}s",
                        started.elapsed().as_secs_f64()
                    ),
                }
            }
        }
        assert!(any_plan > 0, "the gate must have seen at least one plan");
        eprintln!(
            "review_no_negotiated_plan_shares_a_cell: {any_plan} plan(s) checked, {faults_seen} fault(s)"
        );
        assert_eq!(faults_seen, 0, "some returned plan is illegal");
    }

    /// **Criterion 4's fragility, swept rather than argued.**
    ///
    /// The negotiated router has three tuned numbers: `HISTORY_WEIGHT`,
    /// `present_term`'s iteration-0 value, and `deterministic_astar`'s
    /// `CLIMB_COST`. This prints, for whatever those constants currently are,
    /// what the negotiated router does to four circuits -- so an outer script
    /// can edit one constant, rebuild, and re-run.
    ///
    /// Run with `--ignored --nocapture`. Every line is `circuit | cells |
    /// verify | contested trace`.
    #[test]
    #[ignore = "review harness: one row per circuit for the constant currently compiled in"]
    fn review_tuning_probe() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::build_single_segment_netlist;

        let lowered = |name: &str| {
            let circuit = crate::circuits::verilog::find(name).expect("the catalog has it");
            let (gate_level, _) = circuit.baked_netlist();
            crate::compile::lowering::lower(&gate_level).expect("it lowers")
        };
        let cases: Vec<(String, Netlist)> = vec![
            ("and4".to_string(), build_and4_netlist().0),
            ("verilog:and4".to_string(), lowered("verilog:and4")),
            ("full_adder".to_string(), build_full_adder_netlist().0),
            ("segment_a".to_string(), build_single_segment_netlist(0).0),
        ];

        eprintln!(
            "CONFIG HISTORY_WEIGHT={HISTORY_WEIGHT} present_term(0)={} present_term(1)={} present_term(2)={}",
            present_term(0),
            present_term(1),
            present_term(2),
        );
        for (name, netlist) in &cases {
            let placement = relaxed_placement(netlist, &PortPlacements::default(), SHIPPING_AXES)
                .expect("it places");
            let snapped = relax::snap(&placement).expect("it snaps");
            let bare = candidate_from_snapped(netlist, &PortPlacements::default(), &snapped);

            let mut trace = Vec::new();
            let started = std::time::Instant::now();
            let outcome = negotiate(bare, netlist, NEGOTIATION_ROUNDS, PresentSchedule::SHIPPING, &mut trace);
            let contested: Vec<usize> = trace.iter().map(|round| round.contested).collect();
            match outcome {
                Ok(plan) => {
                    let cells: usize = plan.routes.iter().map(|r| r.anchors.len()).sum();
                    let n1 = plan
                        .routes
                        .iter()
                        .find(|r| r.id == "n1")
                        .map(|r| r.anchors.len().to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let cell_faults = review_two_nets_on_one_cell(&plan);
                    let stair_faults = review_foreign_wire_in_a_staircase(&plan);
                    let shared = format!(
                        "{} cell / {} stair",
                        cell_faults.len(),
                        stair_faults.len()
                    );
                    for fault in cell_faults.iter().chain(stair_faults.iter()) {
                        eprintln!("      FAULT {fault}");
                    }
                    // Is each "staircase" pair actually one step, or two
                    // branches meeting end to end in `route.anchors`? Print the
                    // adjacency so a false positive is visible as one.
                    for route in &plan.routes {
                        for pair in route.anchors.windows(2) {
                            let (p, q) = (pair[0], pair[1]);
                            if q.y != p.y && (p.x - q.x).abs() + (p.z - q.z).abs() != 1 {
                                eprintln!(
                                    "      NOT-A-STEP {} {:?} -> {:?} (branch boundary)",
                                    route.id,
                                    (p.x, p.y, p.z),
                                    (q.x, q.y, q.z)
                                );
                            }
                        }
                    }
                    // And the verifier's own verdict, in full.
                    if let Err(error) = verify_candidate(&plan, netlist) {
                        eprintln!("      VERIFY {error}");
                    }
                    eprintln!(
                        "  {name:16} ROUTED {cells:5} cells  n1={n1:>3}  verify {:?}  shared {shared}  iters {}  {:.1}s  trace {contested:?}",
                        verify_candidate(&plan, netlist).map(|_| "ok").map_err(|_| "REFUSED"),
                        trace.len(),
                        started.elapsed().as_secs_f64(),
                    );
                }
                Err(error) => eprintln!(
                    "  {name:16} NO PLAN after {} iters  {:.1}s  ({error})  trace {contested:?}",
                    trace.len(),
                    started.elapsed().as_secs_f64(),
                ),
            }
        }
    }

    /// **The realised graph of a plan `compile` refuses.**
    ///
    /// `docs/derived/realised-graph-extras.md` is built from `realise()`, which
    /// returns `None` the moment `verify_realised_world` refuses -- so the
    /// negotiated `full_adder`, which the strength walk rejects, contributes
    /// nothing to that record and its coupling has never been looked at. The
    /// three inputs `extra_edges` wants all exist regardless of that refusal:
    /// `verify_spacing` builds the reservation, `verification_nets` the nets,
    /// `emit_candidate` the world. So extract it anyway.
    #[test]
    #[ignore = "review harness: coupling of the negotiated plans compile will not ship"]
    fn review_extra_edges_of_the_plans_compile_refuses() {
        use crate::circuits::full_adder::build_full_adder_netlist;

        let lowered = |name: &str| {
            let circuit = crate::circuits::verilog::find(name).expect("the catalog has it");
            let (gate_level, _) = circuit.baked_netlist();
            crate::compile::lowering::lower(&gate_level).expect("it lowers")
        };
        let cases: Vec<(String, Netlist)> = vec![
            ("and4".to_string(), build_and4_netlist().0),
            ("full_adder".to_string(), build_full_adder_netlist().0),
            ("verilog:and4".to_string(), lowered("verilog:and4")),
        ];

        for (name, netlist) in &cases {
            for (label, router, budget) in [
                ("rip-up    ", RouterKind::RipUp, RIP_UP_ROUNDS),
                ("negotiated", RouterKind::Negotiated, TRIAL_RIP_UP_ROUNDS),
                ("negotiated", RouterKind::Negotiated, NEGOTIATION_ROUNDS),
                ("negotiated", RouterKind::Negotiated, RIP_UP_ROUNDS),
            ] {
                let Ok(plan) = plan_from_netlist_with_router(
                    netlist,
                    &PortPlacements::default(),
                    budget,
                    router,
                ) else {
                    eprintln!("{name:16} {label} budget {budget:3}  NO PLAN");
                    continue;
                };
                let verdict = verify_candidate(&plan, netlist);
                let reservation = verify_spacing(&plan).expect("spacing holds on any returned plan");
                let nets = verification_nets(&plan, netlist).expect("the nets derive");
                let realised = emit_candidate(&plan, netlist, candidate_world_size(&plan))
                    .expect("the plan realises");
                let report = crate::compile::coupling::extra_edges(
                    &realised.world,
                    &reservation,
                    netlist,
                    &nets,
                    &realised.ports.gate_output_positions,
                    &realised.ports.input_positions,
                );
                eprintln!(
                    "{name:16} {label} budget {budget:3}  verify {:8}  {:3} extra edge(s)  {:3} contaminated  {:2} foreign read(s)",
                    if verdict.is_ok() { "ok" } else { "REFUSED" },
                    report.extra_edges.len(),
                    report.contaminated_cells,
                    report.foreign_readers.len(),
                );
                for edge in &report.extra_edges {
                    eprintln!("      {edge:?}");
                }
                for read in &report.foreign_readers {
                    eprintln!("      {read:?}");
                }
            }
        }
    }

    // =======================================================================
    // 2026-08-19: the dead climb, the rings, and the wire-under-wire shape,
    // measured across every buildable plan. Measurement harnesses only --
    // nothing here changes production behaviour, and each prints what it ran.
    // =======================================================================

    /// Every plan this branch can produce and realise, with the world each
    /// realises: both routers on the three circuits both carry, negotiated
    /// `segment_a` on the one schedule it routes under, and legacy seeds for
    /// the three circuits neither router carries.
    fn every_buildable_plan() -> Vec<(String, Netlist, PlanCandidate, World)> {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };
        let lowered = |name: &str, optimised: bool| -> Netlist {
            let circuit = crate::circuits::verilog::find(name).expect("the catalog has it");
            let (gate_level, _) = circuit.baked_netlist();
            if optimised {
                crate::compile::lowering::lower_optimised(&gate_level)
            } else {
                crate::compile::lowering::lower(&gate_level)
            }
            .expect("it lowers")
        };

        let mut plans: Vec<(String, Netlist, PlanCandidate)> = Vec::new();
        for (name, netlist) in [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("verilog:and4", lowered("verilog:and4", false)),
        ] {
            let rip_up = plan_from_netlist_with_router(
                &netlist,
                &PortPlacements::default(),
                RIP_UP_ROUNDS,
                RouterKind::RipUp,
            )
            .expect("the rip-up router carries this circuit");
            plans.push((format!("{name} [rip-up]"), netlist.clone(), rip_up));
            let negotiated = plan_negotiated_on_schedule(
                &netlist,
                &PortPlacements::default(),
                NEGOTIATION_ROUNDS,
                PresentSchedule::SHIPPING,
            )
            .expect("the negotiated router carries this circuit at SHIPPING");
            plans.push((format!("{name} [negotiated 0]"), netlist, negotiated));
        }
        {
            // Until 2026-08-19 this schedule was the one plan the negotiated
            // router produced at scale, and it was a latched wrong circuit --
            // 9 rings, 751 self-sustaining cells, 8 of 16 vectors dark. The
            // ring and lid rules refuse those shapes at plan time now, so
            // whether anything is buildable here is a measurement, not a
            // premise.
            let (netlist, _) = build_single_segment_netlist(0);
            match plan_negotiated_on_schedule(
                &netlist,
                &PortPlacements::default(),
                NEGOTIATION_ROUNDS,
                PresentSchedule::starting_at(8),
            ) {
                Ok(negotiated) => {
                    plans.push(("segment_a [negotiated 8]".to_string(), netlist, negotiated));
                }
                Err(error) => {
                    eprintln!(
                        "segment_a [negotiated 8]: no plan -- the router refuses rather than \
                         lay the latch it used to: {error}"
                    );
                }
            }
        }
        for (name, netlist) in [
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
            ("verilog:seven_segment", lowered("verilog:seven_segment", true)),
        ] {
            let compiled = compile::compile_legacy(&netlist).expect("legacy compiles everything");
            let seed = seed_from_legacy(&netlist, &compiled).expect("the legacy seed rebuilds");
            plans.push((format!("{name} [legacy seed]"), netlist, seed));
        }

        plans
            .into_iter()
            .map(|(name, netlist, plan)| {
                let world = emit_candidate(&plan, &netlist, candidate_world_size(&plan))
                    .expect("every buildable plan realises")
                    .world;
                (name, netlist, plan, world)
            })
            .collect()
    }

    /// Rings in one route: a repeater whose output cell reaches its own input
    /// cell through this route's own realised cells, walked with the
    /// simulator's own `dust_connections` in the realised world plus the
    /// repeater's two edges and the strongly-powered-block coupling -- never
    /// plain adjacency, which would count vertical pairs the world keeps
    /// apart.
    fn rings_of(world: &World, route: &Route) -> Vec<(Anchor, usize)> {
        use crate::redstone::simulator::connectivity::dust_connections;
        use crate::redstone::simulator::position::{ALL_SIX, HORIZONTAL};
        use crate::redstone::world::block::BlockKind;

        let own: BTreeSet<Anchor> = route.anchors().iter().copied().collect();
        let at = |cell: Anchor| Position::new(cell.x, cell.y, cell.z);
        let to_anchor = |position: Position| Anchor {
            x: position.x,
            y: position.y,
            z: position.z,
        };

        // One step of realised physics out of `cell`, kept to this route's
        // own cells.
        let steps = |cell: Anchor| -> Vec<Anchor> {
            let mut out = Vec::new();
            match world.get(cell.x, cell.y, cell.z).kind {
                BlockKind::RedstoneWire => {
                    for direction in HORIZONTAL {
                        for target in dust_connections(world, at(cell), direction).iter() {
                            let target = to_anchor(target);
                            if own.contains(&target) {
                                out.push(target);
                            }
                        }
                        // Dust drives a repeater standing beside it whose
                        // input side faces this cell.
                        let beside = to_anchor(at(cell).offset(direction));
                        if own.contains(&beside) {
                            let block = world.get(beside.x, beside.y, beside.z);
                            if block.kind == BlockKind::Repeater
                                && block
                                    .facing
                                    .is_some_and(|f| to_anchor(at(beside).offset(f)) == cell)
                            {
                                out.push(beside);
                            }
                        }
                    }
                }
                BlockKind::Repeater => {
                    if let Some(facing) = world.get(cell.x, cell.y, cell.z).facing {
                        let output = to_anchor(at(cell).offset(facing.opposite()));
                        if own.contains(&output) {
                            out.push(output);
                        }
                        // A repeater strongly powers the block in front of it,
                        // and a strongly powered block drives dust on every
                        // face -- coupling-mechanisms.md mechanism 3, the ramp.
                        if world.get(output.x, output.y, output.z).kind == BlockKind::Solid {
                            for direction in ALL_SIX {
                                let lit = to_anchor(at(output).offset(direction));
                                if own.contains(&lit)
                                    && world.get(lit.x, lit.y, lit.z).kind
                                        == BlockKind::RedstoneWire
                                {
                                    out.push(lit);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            out
        };

        let mut rings = Vec::new();
        for anchor in route.anchors() {
            let block = world.get(anchor.x, anchor.y, anchor.z);
            if block.kind != BlockKind::Repeater {
                continue;
            }
            let Some(facing) = block.facing else { continue };
            let input = to_anchor(at(*anchor).offset(facing));
            let output = to_anchor(at(*anchor).offset(facing.opposite()));
            if !own.contains(&input) || !own.contains(&output) {
                continue;
            }
            let mut seen: BTreeSet<Anchor> = BTreeSet::from([*anchor]);
            let mut frontier = vec![output];
            let mut closed = false;
            while let Some(cell) = frontier.pop() {
                if !seen.insert(cell) {
                    continue;
                }
                if cell == input {
                    closed = true;
                    break;
                }
                for step in steps(cell) {
                    if !seen.contains(&step) {
                        frontier.push(step);
                    }
                }
            }
            if closed {
                rings.push((*anchor, seen.len()));
            }
        }
        rings
    }

    /// Every conductor that stays powered after every source is deleted:
    /// torches removed, levers thrown off, the world settled by the real
    /// `Simulator`. Nothing legitimate survives that -- whatever does is a
    /// self-sustaining loop, which is the physics definition of a latch and
    /// needs no propagation rule restated here (standing rule 6).
    fn latched_cells(world: &World) -> Vec<(Anchor, crate::redstone::world::block::BlockKind, u8)> {
        use crate::redstone::world::block::BlockKind;

        let mut sourceless = world.clone();
        let (sx, sy, sz) = sourceless.size();
        for x in 0..sx {
            for y in 0..sy {
                for z in 0..sz {
                    let block = sourceless.get(x, y, z).clone();
                    match block.kind {
                        BlockKind::Torch | BlockKind::WallTorch | BlockKind::RedstoneBlock => {
                            sourceless.set(x, y, z, BlockState::air());
                        }
                        BlockKind::Lever => {
                            let mut lever = block;
                            lever.lit = false;
                            sourceless.set(x, y, z, lever);
                        }
                        _ => {}
                    }
                }
            }
        }
        let mut simulator = crate::redstone::simulator::Simulator::new(sourceless);
        simulator
            .run_until_stable(2000)
            .expect("the sourceless world settles");
        let mut out = Vec::new();
        for x in 0..sx {
            for y in 0..sy {
                for z in 0..sz {
                    let block = simulator.world().get(x, y, z);
                    let alive = match block.kind {
                        BlockKind::RedstoneWire => block.power > 0,
                        BlockKind::Repeater => block.lit,
                        _ => false,
                    };
                    if alive {
                        out.push((Anchor { x, y, z }, block.kind, block.power));
                    }
                }
            }
        }
        out
    }

    /// **(A) and (C) of the 2026-08-19 diagnosis**: every ring and every
    /// wire-under-wire pair in every buildable plan, from the realised worlds.
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::rings_and_wire_under_wire_swept_across_every_buildable_plan \
    ///   -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "measurement harness: builds ten plans (negotiated segment_a alone is ~40s) and sweeps each"]
    fn rings_and_wire_under_wire_swept_across_every_buildable_plan() {
        let mut total_rings = 0usize;
        let mut total_stacks = 0usize;
        for (name, _netlist, plan, world) in every_buildable_plan() {
            let mut owner_of: BTreeMap<Anchor, String> = BTreeMap::new();
            for route in plan.routes() {
                for anchor in route.anchors() {
                    owner_of.insert(*anchor, route.id().to_string());
                }
            }

            let mut plan_rings = Vec::new();
            let mut plan_stacks = Vec::new();
            for route in plan.routes() {
                for (anchor, size) in rings_of(&world, route) {
                    plan_rings.push((route.id().to_string(), anchor, size));
                }
                let own: BTreeSet<Anchor> = route.anchors().iter().copied().collect();
                // A pair counts only when both cells hold a conductor in the
                // realised world -- a legacy route's anchor list also names
                // cells that realise as something else, and a "stack" whose
                // upper half is stone is not the shape under measurement.
                let conducts = |cell: &Anchor| {
                    matches!(
                        world.get(cell.x, cell.y, cell.z).kind,
                        crate::redstone::world::block::BlockKind::RedstoneWire
                            | crate::redstone::world::block::BlockKind::Repeater
                    )
                };
                for anchor in route.anchors() {
                    let above = Anchor {
                        y: anchor.y + 1,
                        ..*anchor
                    };
                    if !conducts(anchor) || !conducts(&above) {
                        continue;
                    }
                    if own.contains(&above) {
                        plan_stacks.push((
                            format!("{} under itself", route.id()),
                            *anchor,
                            above,
                        ));
                    } else if let Some(other) = owner_of.get(&above) {
                        if other != route.id() {
                            plan_stacks.push((
                                format!("{} under {other}", route.id()),
                                *anchor,
                                above,
                            ));
                        }
                    }
                }
            }

            let latched = latched_cells(&world);
            total_rings += plan_rings.len();
            total_stacks += plan_stacks.len();
            eprintln!(
                "{name}: {} route(s); {} ring(s); {} wire-under-wire pair(s); {} latched cell(s) with every source deleted",
                plan.routes().len(),
                plan_rings.len(),
                plan_stacks.len(),
                latched.len(),
            );
            for (net, anchor, size) in &plan_rings {
                eprintln!(
                    "    RING  net {net}: repeater ({}, {}, {}) closes through {size} of its own cells",
                    anchor.x, anchor.y, anchor.z
                );
            }
            for (label, lower, upper) in &plan_stacks {
                eprintln!(
                    "    STACK {label}: wire ({}, {}, {}) directly under wire ({}, {}, {})",
                    lower.x, lower.y, lower.z, upper.x, upper.y, upper.z
                );
            }
            for (cell, kind, power) in &latched {
                eprintln!(
                    "    LATCH ({}, {}, {}) {kind:?} power {power} owned by {:?}",
                    cell.x,
                    cell.y,
                    cell.z,
                    owner_of.get(cell)
                );
            }
        }
        eprintln!("total: {total_rings} ring(s), {total_stacks} wire-under-wire pair(s)");
    }

    /// **(B) of the 2026-08-19 diagnosis**: the dead climb of negotiated
    /// `segment_a`, read to the cell -- who holds the lid, who put stone in
    /// it, what the simulator says, and what plan-time legality asked.
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::the_dead_climb_of_negotiated_segment_a_read_to_the_cell \
    ///   -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "measurement harness: routes segment_a through 32 negotiation iterations and reads the climb, ~60s"]
    fn the_dead_climb_of_negotiated_segment_a_read_to_the_cell() {
        use crate::circuits::seven_segment::{build_single_segment_netlist, INPUT_NAMES};
        use crate::redstone::simulator::connectivity::dust_connections;
        use crate::redstone::simulator::position::HORIZONTAL;

        let (netlist, _output) = build_single_segment_netlist(0);
        // Since 2026-08-19 the ring and lid rules refuse every plan this
        // schedule used to produce, so the shape this harness reads no longer
        // exists on the shipping code. The diagnosis it records was measured
        // with both rules disabled (the injection runs of
        // `the_lid_rule_refuses_the_floor_that_cut_g0s_climb` and
        // `lay_net_refuses_the_branch_that_closes_a_ring`); to replay it,
        // disable those two arms and run this again.
        let plan = match plan_negotiated_on_schedule(
            &netlist,
            &PortPlacements::default(),
            NEGOTIATION_ROUNDS,
            PresentSchedule::starting_at(8),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                eprintln!(
                    "the dead climb is unbuildable now: the router refuses this schedule's \
                     every plan rather than lay the shapes that produced it: {error}"
                );
                return;
            }
        };

        let q = Anchor { x: 86, y: 1, z: 109 }; // the lower dust of the dead climb
        let p = Anchor { x: 87, y: 2, z: 109 }; // the upper dust
        let lid = Anchor { x: 86, y: 2, z: 109 }; // Q.up() -- the cell the climb rule reads
        let step_cell = Anchor { x: 87, y: 1, z: 109 }; // P.down() -- the step
        let over = Anchor { x: 86, y: 3, z: 109 }; // the only cell whose floor is the lid

        // 1. Who holds each cell as a route anchor, at which index. The index
        // is the lay order: `lay_net` appends `path[shared..]` per branch.
        for cell in [q, p, lid, step_cell, over] {
            let mut holders = Vec::new();
            for route in plan.routes() {
                if let Some(index) = route.anchors().iter().position(|anchor| anchor == &cell) {
                    holders.push(format!(
                        "net {} anchor[{index}] holds {:?}",
                        route.id(),
                        route.realisation()[index].kind
                    ));
                }
            }
            eprintln!(
                "plan ({}, {}, {}): {}",
                cell.x,
                cell.y,
                cell.z,
                if holders.is_empty() {
                    "no route anchor".to_string()
                } else {
                    holders.join("; ")
                }
            );
        }
        for (index, node) in plan.primitive_nodes.iter().enumerate() {
            if node.footprint.contains(&lid) || node.anchor == lid {
                eprintln!("primitive {index} `{}` claims the lid", node.id);
            }
        }

        // g0 near the climb, in anchor (= lay) order, and its branch ends.
        let g0 = route_named(&plan, "g0");
        for (index, (anchor, block)) in
            g0.anchors().iter().zip(g0.realisation().iter()).enumerate()
        {
            if (anchor.x - 86).abs() <= 2 && (anchor.z - 109).abs() <= 2 {
                eprintln!(
                    "g0 anchor[{index}] ({}, {}, {}) {:?}",
                    anchor.x, anchor.y, anchor.z, block.kind
                );
            }
        }
        for (index, terminal) in g0.terminals().iter().enumerate() {
            let socket = terminal.sink.anchor;
            let position = g0.anchors().iter().position(|anchor| *anchor == socket);
            eprintln!(
                "g0 terminal[{index}] -> {}.in[{}] socket ({}, {}, {}) at anchor index {position:?}",
                terminal.sink.gate,
                terminal.sink.input_index,
                socket.x,
                socket.y,
                socket.z
            );
        }

        // 2. The realised world: both columns.
        let realised = emit_candidate(&plan, &netlist, candidate_world_size(&plan))
            .expect("the plan realises");
        for (x, z) in [(86, 109), (87, 109)] {
            for y in 0..5 {
                let block = realised.world.get(x, y, z);
                eprintln!("world ({x}, {y}, {z}) = {:?}", block.kind);
            }
        }

        // 3. The join relation in that world, asked through the simulator's
        // own `dust_connections`: every direction out of Q and out of P.
        for (label, cell) in [("Q(86,1,109)", q), ("P(87,2,109)", p)] {
            for direction in HORIZONTAL {
                let targets: Vec<Position> =
                    dust_connections(&realised.world, Position::new(cell.x, cell.y, cell.z), direction)
                        .iter()
                        .collect();
                eprintln!("dust_connections({label}, {direction:?}) = {targets:?}");
            }
        }

        // 4. The simulator, all sixteen vectors: what Q and P carry.
        let compiled = compile::CompiledCircuit {
            world: realised.world.clone(),
            input_positions: realised.ports.input_positions.clone(),
            output_positions: realised.ports.output_positions.clone(),
            gate_output_positions: realised.ports.gate_output_positions.clone(),
            gate_facings: (0..netlist.gates.len()).map(|g| plan.facing_of(g)).collect(),
            planner_kind: compile::PlannerKind::Unified3d,
            legacy_emission: None,
        };
        let levers: Vec<(i32, i32, i32)> = INPUT_NAMES
            .iter()
            .map(|name| *compiled.input_positions.get(*name).expect("a lever per input"))
            .collect();
        let mut simulator = crate::redstone::simulator::Simulator::new(compiled.world.clone());
        simulator.run_until_stable(2000).expect("settles");
        let mut q_readings = BTreeSet::new();
        let mut p_readings = BTreeSet::new();
        for combination in 0..16usize {
            let bits: Vec<bool> = (0..4).map(|i| (combination >> (3 - i)) & 1 == 1).collect();
            for (position, &bit) in levers.iter().zip(bits.iter()) {
                let mut state = simulator
                    .world()
                    .get(position.0, position.1, position.2)
                    .clone();
                state.lit = bit;
                simulator
                    .world_mut()
                    .set(position.0, position.1, position.2, state);
                simulator.run_until_stable(2000).expect("settles");
            }
            q_readings.insert(simulator.world().get(q.x, q.y, q.z).power);
            p_readings.insert(simulator.world().get(p.x, p.y, p.z).power);
        }
        eprintln!("simulator, all 16 vectors: Q reads {q_readings:?}, P reads {p_readings:?}");

        // 5. Plan-time legality, replayed. Reserve the climb exactly as
        // `reserve_path` does, then ask `anchor_is_free_for` about the cell
        // over the lid -- under this net's own name and under a foreign one.
        let elsewhere = Anchor {
            x: -10_000,
            y: -10_000,
            z: -10_000,
        };
        let mut reservation = Reservation::new();
        reserve_path(&mut reservation, "g0", &[q, p]);
        eprintln!(
            "after the climb is reserved: lid ({}, {}, {}) owner {:?} occupancy {:?}; step ({}, {}, {}) owner {:?} occupancy {:?}",
            lid.x,
            lid.y,
            lid.z,
            reservation.owner(&lid),
            reservation.occupancy(&lid),
            step_cell.x,
            step_cell.y,
            step_cell.z,
            reservation.owner(&step_cell),
            reservation.occupancy(&step_cell),
        );
        eprintln!(
            "anchor_is_free_for(over-the-lid (86,3,109), owner g0)      = {}",
            anchor_is_free_for(over, elsewhere, elsewhere, elsewhere, "g0", &reservation)
        );
        eprintln!(
            "anchor_is_free_for(over-the-lid (86,3,109), foreign owner) = {}",
            anchor_is_free_for(over, elsewhere, elsewhere, elsewhere, "somebody_else", &reservation)
        );
    }

    /// Whether the wire-under-wire shape is already in a world `compile()`
    /// ships today: every condition circuit through the real shipping path,
    /// the world scanned for dust standing directly on dust.
    #[test]
    #[ignore = "measurement harness: compiles all six condition circuits through the shipping path, ~5s"]
    fn shipped_worlds_scanned_for_wire_under_wire() {
        use crate::circuits::full_adder::build_full_adder_netlist;
        use crate::circuits::seven_segment::{
            build_seven_segment_netlist, build_single_segment_netlist,
        };
        use crate::redstone::world::block::BlockKind;

        let lowered = |name: &str, optimised: bool| -> Netlist {
            let circuit = crate::circuits::verilog::find(name).expect("the catalog has it");
            let (gate_level, _) = circuit.baked_netlist();
            if optimised {
                crate::compile::lowering::lower_optimised(&gate_level)
            } else {
                crate::compile::lowering::lower(&gate_level)
            }
            .expect("it lowers")
        };
        for (name, netlist) in [
            ("and4", build_and4_netlist().0),
            ("full_adder", build_full_adder_netlist().0),
            ("verilog:and4", lowered("verilog:and4", false)),
            ("segment_a", build_single_segment_netlist(0).0),
            ("seven_segment", build_seven_segment_netlist().0),
            ("verilog:seven_segment", lowered("verilog:seven_segment", true)),
        ] {
            let compiled = compile::compile(&netlist).expect("every condition circuit compiles");
            let (sx, sy, sz) = compiled.world.size();
            let mut pairs = Vec::new();
            for x in 0..sx {
                for y in 1..sy {
                    for z in 0..sz {
                        if compiled.world.get(x, y, z).kind == BlockKind::RedstoneWire
                            && compiled.world.get(x, y - 1, z).kind == BlockKind::RedstoneWire
                        {
                            pairs.push((x, y - 1, z));
                        }
                    }
                }
            }
            eprintln!(
                "{name} [{:?}]: {} dust-on-dust pair(s) in the shipped world{}{}",
                compiled.planner_kind(),
                pairs.len(),
                if pairs.is_empty() { "" } else { ": lower cells " },
                if pairs.is_empty() {
                    String::new()
                } else {
                    format!("{pairs:?}")
                },
            );
        }
    }

    /// **The class question of the 2026-08-19 diagnosis**: what the plan
    /// certified (`realise_branch_from`'s `carries`, true for every branch of
    /// every returned plan) against what the simulator delivers, sink by sink,
    /// through `strength_differential`'s isolation worlds -- the corrected
    /// oracle machinery from `2e0594f`, adapted from walk-vs-simulator to
    /// plan-vs-simulator.
    ///
    /// ```bash
    /// cargo test --release --lib \
    ///   compile::planner::tests::plan_time_carry_certification_reconciled_with_the_simulator \
    ///   -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "measurement harness: ten plans, each swept through every vector plus per-origin isolation worlds, several minutes"]
    fn plan_time_carry_certification_reconciled_with_the_simulator() {
        use crate::compile::strength_differential::measure;

        for (name, netlist, plan, _world) in every_buildable_plan() {
            let size = candidate_world_size(&plan);
            let parts = realise_without_verifying(&plan, &netlist, size).expect("it realises");
            let ports = &parts.realised.ports;
            let measurement = match measure(
                &parts.realised.world,
                &parts.reservation,
                &netlist,
                &parts.nets,
                &ports.gate_output_positions,
                &ports.input_positions,
                &ports.output_positions,
            ) {
                Ok(measurement) => measurement,
                Err(error) => {
                    eprintln!("{name}: NOT MEASURED: {error}");
                    continue;
                }
            };

            let terminals: usize = plan
                .routes()
                .iter()
                .map(|route| route.terminals().len())
                .sum();
            let mut dead = Vec::new();
            let mut sinks = 0usize;
            for group in &measurement.groups {
                for reading in group.readings.iter().filter(|reading| reading.is_sink) {
                    sinks += 1;
                    if !reading.attributed() {
                        dead.push(format!(
                            "nets [{}] deliver nothing to sink ({}, {}, {}) in any isolation world \
                             (walk {}, live {}, control {}, every origin live: {})",
                            group.nets.join(", "),
                            reading.cell.x,
                            reading.cell.y,
                            reading.cell.z,
                            reading.walk,
                            reading.live,
                            reading.control,
                            group.every_origin_live(),
                        ));
                    }
                }
            }
            eprintln!(
                "{name}: {terminals} branch(es) certified carrying at plan time; {sinks} sink support(s) measured; {} dead in the world; judge: {}",
                dead.len(),
                measurement
                    .shipping_verdict
                    .clone()
                    .unwrap_or_else(|| "passes".to_string()),
            );
            for line in &dead {
                eprintln!("    DEAD  {line}");
            }
            for line in &measurement.unmeasured {
                eprintln!("    NOT MEASURED: {line}");
            }
        }
    }

    // =======================================================================
    // 2026-08-19: the two hard rules the diagnosis bought -- the lid rule (B)
    // and the ring rule (A). Each test here can fail against the defect it
    // names: disable the rule's arm and the assertion goes red on exactly the
    // shape the diagnosis measured.
    // =======================================================================

    /// **THE LID RULE, replayed on the diagnosed cells.** Negotiated
    /// `segment_a`'s g0 laid wire at (86,3,109), one storey above its own
    /// certified climb (86,1,109) -> (87,2,109); the wire's floor is the
    /// climb's lid, `anchor_is_free_for` answered `true` (measured in
    /// `the_dead_climb_of_negotiated_segment_a_read_to_the_cell`), and the
    /// stone `emit_routes` wrote there cut the climb at all 16 vectors.
    ///
    /// Injection (standing rule 2): with `anchor_is_free_for`'s air arm
    /// commented out, the two refusal asserts below go red -- the exact call
    /// the diagnosis replayed as `true` is `true` again. Confirmed red on
    /// 2026-08-19, then the arm restored.
    #[test]
    fn the_lid_rule_refuses_the_floor_that_cut_g0s_climb() {
        let q = Anchor { x: 86, y: 1, z: 109 };
        let p = Anchor { x: 87, y: 2, z: 109 };
        let lid = Anchor { x: 86, y: 2, z: 109 };
        let over = Anchor { x: 86, y: 3, z: 109 };
        let elsewhere = Anchor { x: -10_000, y: -10_000, z: -10_000 };

        let mut reservation = Reservation::new();
        reserve_path(&mut reservation, "g0", &[q, p]);

        // The commitment is air by name now, not `Solid` like a torch.
        assert_eq!(
            reservation.occupancy(&lid),
            Some(Occupancy::Air),
            "the climb's headroom is a mandatory-air commitment"
        );
        assert_eq!(reservation.air_owner(&lid), Some("stair:g0"));

        // The exact call the diagnosis replayed as `true`: the net's OWN later
        // branch may not lay a floor into its own climb's lid...
        assert!(
            !anchor_is_free_for(over, elsewhere, elsewhere, elsewhere, "g0", &reservation),
            "the cell over the lid must refuse wire to the climb's own net: \
             its floor is the stone that cuts the climb"
        );
        // ...and the rule is symmetric: a foreign net is refused too (the
        // rip-up router shares one reservation, so foreign guards are visible
        // there; under negotiation the same relation is priced and gated to
        // zero by `contested`).
        assert!(
            !anchor_is_free_for(over, elsewhere, elsewhere, elsewhere, "somebody_else", &reservation),
            "the lid rule is symmetric across owners"
        );

        // Control: one cell west, whose floor is nobody's commitment, stays
        // free -- the arm refuses the lid and nothing else.
        let control = Anchor { x: 85, y: 3, z: 109 };
        assert!(
            anchor_is_free_for(control, elsewhere, elsewhere, elsewhere, "g0", &reservation),
            "the refusal is the lid's, not the whole storey's"
        );

        // A descent's drop cell carries the same commitment: the cell beside
        // the upper cell has to stay air or the fall is a wall.
        let mut descent = Reservation::new();
        reserve_path(
            &mut descent,
            "n",
            &[Anchor { x: 0, y: 2, z: 0 }, Anchor { x: 1, y: 1, z: 0 }],
        );
        let drop = Anchor { x: 1, y: 2, z: 0 };
        assert_eq!(descent.air_owner(&drop), Some("stair:n"));
        assert!(
            !anchor_is_free_for(
                Anchor { x: 1, y: 3, z: 0 },
                elsewhere,
                elsewhere,
                elsewhere,
                "n",
                &descent
            ),
            "a floor into a descent's drop cell is the same cut"
        );
    }

    /// **THE RING RULE's detector, on the measured shape.** The first case is
    /// byte-for-byte the diagnosed g0 ring: the repeater at (93,4,110), its
    /// output running (94,4,110) -> (94,4,111) -> (93,4,111) -> (92,4,111) ->
    /// (92,4,110), and (92,4,110) is the repeater's own input cell. The other
    /// cases prove the detector's edges are the join relation and not plain
    /// adjacency: an open loop is no ring, a ring through a vertical join
    /// exists exactly while the pair's lid is uncommitted, and a committed
    /// stone lid breaks it.
    #[test]
    fn a_ring_through_a_repeater_is_found_and_a_sealed_lid_breaks_it() {
        use crate::redstone::world::block::Facing;

        let dust = compile::dust();
        let route_of = |cells: Vec<(Anchor, BlockState)>| -> Route {
            let (anchors, blocks): (Vec<Anchor>, Vec<BlockState>) = cells.into_iter().unzip();
            let floors = vec![compile::stone(); anchors.len()];
            Route::from_legacy("g0".to_string(), anchors, Vec::new(), blocks, floors)
        };
        let at = |x: i32, y: i32, z: i32| Anchor { x, y, z };

        // Case 1: the measured g0 ring, same level throughout. The signal
        // travels east through the repeater, so `compile::repeater(East)`
        // stores facing West -- input (92,4,110), output (94,4,110).
        let measured = route_of(vec![
            (at(92, 4, 110), dust.clone()),
            (at(93, 4, 110), compile::repeater(Facing::East)),
            (at(94, 4, 110), dust.clone()),
            (at(94, 4, 111), dust.clone()),
            (at(93, 4, 111), dust.clone()),
            (at(92, 4, 111), dust.clone()),
        ]);
        let (repeater, ring) = ring_closed_in(&measured, &Reservation::new())
            .expect("the measured g0 shape is a ring");
        assert_eq!(repeater, at(93, 4, 110));
        assert!(ring.contains(&at(92, 4, 110)), "the flood reached the input");

        // Case 2: the same route with the closing cell gone is a tree.
        let open = route_of(vec![
            (at(92, 4, 110), dust.clone()),
            (at(93, 4, 110), compile::repeater(Facing::East)),
            (at(94, 4, 110), dust.clone()),
            (at(94, 4, 111), dust.clone()),
            (at(93, 4, 111), dust.clone()),
        ]);
        assert_eq!(
            ring_closed_in(&open, &Reservation::new()),
            None,
            "no closure, no ring"
        );

        // Case 3: a ring that needs a vertical join -- the output side climbs
        // at (4,1,0) -> (4,2,1), runs back west one storey up and two cells
        // over, and descends onto the input side at (0,2,1) -> (0,1,0). Both
        // joins' lids -- (4,2,0) and (0,2,0) -- are open, so the ring
        // closes...
        let climb_ring = || {
            route_of(vec![
                (at(0, 1, 0), dust.clone()),
                (at(1, 1, 0), compile::repeater(Facing::East)),
                (at(2, 1, 0), dust.clone()),
                (at(3, 1, 0), dust.clone()),
                (at(4, 1, 0), dust.clone()),
                (at(4, 2, 1), dust.clone()),
                (at(4, 2, 2), dust.clone()),
                (at(3, 2, 2), dust.clone()),
                (at(2, 2, 2), dust.clone()),
                (at(1, 2, 2), dust.clone()),
                (at(0, 2, 2), dust.clone()),
                (at(0, 2, 1), dust.clone()),
            ])
        };
        assert!(
            ring_closed_in(&climb_ring(), &Reservation::new()).is_some(),
            "the climb's lid (4,2,0) and the descent's lid (0,2,0) are open, \
             so the vertical pairs join and the ring closes"
        );

        // ...and committing one lid to stone breaks exactly that join, which
        // is the same closed form the lid rule stands on: a vertical pair is
        // apart iff its lid is a conductive full block.
        let mut sealed = Reservation::new();
        sealed.insert(at(4, 2, 0), "anyone", Occupancy::Stone);
        assert_eq!(
            ring_closed_in(&climb_ring(), &sealed),
            None,
            "a committed stone lid seals the climb join and the ring cannot close"
        );
    }

    /// **THE RING RULE, wired: `lay_net` refuses the branch that closes a
    /// ring.** A two-gate net in a walled corridor. The first branch runs a
    /// trunk of twenty-one cells, so the strength budget puts a refresh at
    /// (14,1,0). The second branch is forced up a staircase at the trunk's
    /// far end and back along the storey above:
    ///
    /// * In the **ring** corridor the return run is directly beside the
    ///   trunk (z = 1) with open lids at x = 8..=12, so it joins back onto
    ///   the trunk *west* of the refresh -- trunk, climb, return run and
    ///   descent-joins close a cycle with the repeater inside it, which is
    ///   the latch `emit` would light. `lay_net` must refuse the branch.
    ///
    /// * In the **control** corridor the return run is two cells over
    ///   (z = 2) and rejoins the trunk through a single open lid at
    ///   x = 16 -- *east* of the refresh. The same kind of cycle closes, and
    ///   the route still carries a repeater, but the repeater is outside the
    ///   loop: dust alone has no gain, so this is not a latch and `lay_net`
    ///   must allow it. The rule keys on the repeater inside the ring, not
    ///   on loops and not on repeaters.
    ///
    /// Injection (standing rule 2): with the `ring_closed_in` call in
    /// `lay_net` commented out, the first half goes red (`lay_net` returns
    /// `Ok` and the ring ships in the returned route). Confirmed red on
    /// 2026-08-19, then the call restored.
    #[test]
    fn lay_net_refuses_the_branch_that_closes_a_ring() {
        let at = |x: i32, y: i32, z: i32| Anchor { x, y, z };
        let trunk_end = 20;

        // Gate 0 faces north at (22,1,0): socket (21,1,0), approach (20,1,0)
        // -- the trunk's own east end. Gate 1 faces east, so its socket is
        // one cell north of its support and the approach one further.
        let lay_through = |open: &BTreeSet<Anchor>,
                           support1: Anchor|
         -> Result<Route, Box<RoutingFailure>> {
            let netlist = Netlist {
                inputs: vec!["n".to_string()],
                outputs: vec!["s0".to_string(), "s1".to_string()],
                gates: vec![Gate::nor("s0", &["n"]), Gate::nor("s1", &["n"])],
            };
            let support0 = at(trunk_end + 2, 1, 0);
            let candidate = PlanCandidate::with_facings(
                vec![support0, support1],
                Vec::new(),
                Vec::new(),
                vec![geometry::CellFacing::NORTH, geometry::CellFacing::EAST],
            );
            let socket0 = step(
                support0,
                compile::geometry::input_directions(candidate.facing_of(0))[0],
            );
            let socket1 = step(
                support1,
                compile::geometry::input_directions(candidate.facing_of(1))[0],
            );
            assert_eq!(socket0, at(trunk_end + 1, 1, 0));

            let mut walled = Reservation::new();
            for x in -25..=45 {
                for y in 1..=5 {
                    for z in -25..=25 {
                        let cell = at(x, y, z);
                        let is_open = open.contains(&cell)
                            || cell == socket0
                            || cell == support0
                            || cell == socket1
                            || cell == support1;
                        if !is_open {
                            walled.insert(cell, "wall", Occupancy::Solid);
                        }
                    }
                }
            }

            let congestion = Congestion::default();
            let prices = Prices::RipUp(&congestion);
            lay_net(
                "n",
                at(0, 1, 0),
                &[(0, 0), (1, 0)],
                &netlist,
                &candidate,
                &mut walled,
                &prices,
            )
        };

        let trunk_climb_and = |upper: &[Anchor]| -> BTreeSet<Anchor> {
            let mut open: BTreeSet<Anchor> = BTreeSet::new();
            for x in 0..=trunk_end {
                open.insert(at(x, 1, 0)); // the trunk
            }
            open.insert(at(trunk_end, 1, 1)); // the climb's riser
            open.insert(at(trunk_end, 2, 0)); // the climb's headroom
            open.extend(upper.iter().copied());
            open
        };

        // The ring corridor: return run beside the trunk, lids open west of
        // the refresh. Gate 1's approach (8,2,1) is the run's west end.
        let mut ring_upper: Vec<Anchor> = (8..=trunk_end).map(|x| at(x, 2, 1)).collect();
        ring_upper.extend((8..=12).map(|x| at(x, 2, 0)));
        let failure = match lay_through(&trunk_climb_and(&ring_upper), at(8, 2, 3)) {
            Err(failure) => failure,
            Ok(route) => panic!(
                "the return run closes a ring around the trunk's refresh, but lay_net \
                 returned a {}-cell route -- the ring rule is not wired in",
                route.anchors().len()
            ),
        };
        let reason = failure.error.to_string();
        assert!(
            reason.contains("closes a ring"),
            "the refusal is the ring rule's, not a routing dead end: {reason}"
        );
        assert!(
            !failure.charge_outright.is_empty(),
            "the refused branch charges the cells that closed the ring, \
             so the next iteration prices this corridor"
        );

        // The control corridor: return run two cells over, one open lid at
        // x = 16, east of the refresh. Gate 1's approach (16,2,1) is the
        // join column itself.
        let mut control_upper: Vec<Anchor> = vec![at(trunk_end, 2, 1)];
        control_upper.extend((16..=trunk_end).map(|x| at(x, 2, 2)));
        control_upper.push(at(16, 2, 1)); // the descent-join, and the approach
        control_upper.push(at(16, 2, 0)); // its open lid
        let route = match lay_through(&trunk_climb_and(&control_upper), at(16, 2, 3)) {
            Ok(route) => route,
            Err(failure) => panic!(
                "a loop whose repeater sits outside it is not a latch, but lay_net \
                 refused it: {}",
                failure.error
            ),
        };
        assert!(
            route
                .realisation()
                .iter()
                .any(|block| block.kind == crate::redstone::world::block::BlockKind::Repeater),
            "the control is only a control while the route still carries a refresh \
             somewhere outside the loop"
        );
    }
}

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
            Occupancy::Conductor
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
                &Congestion::default(),
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
    congestion: &Congestion,
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
                .saturating_add(congestion.price(&next));
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
            reservation.insert(cell, &guard, Occupancy::Solid);
        }
    }
    for &anchor in path {
        reservation.insert(anchor, owner, Occupancy::Conductor);
        // The floor this cell stands on is this route's too. Solid, because a
        // floor is inert: another net may run beside it, just not through it.
        reservation.insert(
            Anchor {
                y: anchor.y - 1,
                ..anchor
            },
            owner,
            Occupancy::Solid,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    Conductor,
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
            matches!(occupancy, Occupancy::Conductor).then_some(owner.as_str())
        })
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
    plan_with_axes(netlist, placements, SHIPPING_AXES)
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

fn plan_with_axes(
    netlist: &Netlist,
    placements: &PortPlacements,
    axes: relax::Axes,
) -> Result<PlanCandidate, PlannerError> {
    let placement = relaxed_placement(netlist, placements, axes)?;
    let snapped = relax::snap(&placement).map_err(PlannerError::Relaxation)?;

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
    for node in &snapped {
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

    let candidate = PlanCandidate::with_facings(anchors, primitive_nodes, Vec::new(), facings);
    route_every_net(candidate, netlist)
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
const RIP_UP_ROUNDS: usize = 64;

fn route_every_net(
    candidate: PlanCandidate,
    netlist: &Netlist,
) -> Result<PlanCandidate, PlannerError> {
    let mut order: Vec<String> = net_sinks(netlist).into_keys().collect();
    let mut congestion = Congestion::default();
    let mut last: Option<PlannerError> = None;

    for _ in 0..RIP_UP_ROUNDS {
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

fn route_in_order(
    mut candidate: PlanCandidate,
    netlist: &Netlist,
    order: &[String],
    congestion: &Congestion,
) -> Result<PlanCandidate, Box<RoutingFailure>> {
    let mut reservation = reserve_primitives(&candidate.primitive_nodes);

    let sinks = net_sinks(netlist);

    // Every socket has exactly one cell a signal can enter it from -- the one
    // collinear with socket and support, because a terminal only reads from
    // directly behind itself. Which net will use it is on the netlist, so it
    // is claimed for that net now. Left free, it goes to whichever route is
    // laid first, and the net that actually needs it can never reach its own
    // gate: that is what made seven_segment unroutable, and no amount of
    // spare room above the plane fixes it, because there is no second way in.
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
            reservation.insert(approach, driver, Occupancy::Conductor);
        }
    }

    let mut routes = Vec::with_capacity(sinks.len());
    for signal in order {
        let signal = signal.clone();
        let consumers = sinks
            .get(&signal)
            .cloned()
            .expect("the order is built from these very keys");
        let source = candidate
            .primitive_nodes
            .iter()
            .find(|node| {
                node.id == format!("gate:{signal}") || node.id == format!("input:{signal}")
            })
            .map(|node| node.source())
            .ok_or_else(|| {
                Box::new(RoutingFailure {
                    blocked: signal.clone(),
                    corridor: (Anchor { x: 0, y: 0, z: 0 }, Anchor { x: 0, y: 0, z: 0 }),
                    reservation: Reservation::new(),
                    charge_outright: Vec::new(),
                    error: PlannerError::UnrealisableNode {
                        id: signal.clone(),
                        reason: "no gate or primary input drives this signal".to_string(),
                    },
                })
            })?;

        let mut route = Route::new(signal.clone(), Vec::new());
        route.owner = Some(signal.clone());
        for &(gate, input_index) in &consumers {
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
                &reservation,
                congestion,
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
            reserve_path(&mut reservation, &signal, &path);

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
                        terminal_is_isolated(&reservation, &signal, predecessor, socket, support),
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
        }
        routes.push(route);
    }

    candidate.routes = routes;
    Ok(candidate)
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

    Ok(realised)
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
            &Congestion::default(),
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
        let compiled = compile::compile(&netlist).expect("and4 fixture must compile");
        let seed = seed_from_legacy(&netlist, &compiled).expect("compiled fixture must seed");
        (seed, netlist)
    }

    fn legacy_fanout_seed() -> PlanCandidate {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["left".to_string(), "right".to_string()],
            gates: vec![Gate::nor("left", &["a"]), Gate::nor("right", &["a"])],
        };
        let compiled = compile::compile(&netlist).expect("fanout fixture must compile");
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
            let compiled = compile::compile(&netlist).expect("reference circuits compile");
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
        let compiled = compile::compile(&netlist).expect("full_adder compiles");
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
        let compiled = compile::compile(&netlist).expect("and4 compiles");
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
        let compiled = compile::compile(&netlist).expect("full_adder compiles");
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
        let compiled = compile::compile(&netlist).expect("and4 compiles");
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
                match plan_with_axes(&netlist, &PortPlacements::default(), axes) {
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
        let compiled = crate::compile::compile(&netlist).expect("and4 compiles the legacy way");
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
                    Occupancy::Conductor,
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
                if node.occupancy_of(cell) != Occupancy::Conductor {
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
            let compiled = compile::compile(&netlist).expect("compiles");
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
        dust_below.insert(Anchor { y: 0, ..neighbour }, "other", Occupancy::Conductor);
        assert!(
            !anchor_is_free_for(cell, cell, cell, cell, "mine", &dust_below),
            "another net's dust one level down is exactly what keep-out is for"
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

    #[test]
    fn compile_ships_the_world_the_planner_realised() {
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
        let compiled = compile::compile(&netlist).expect("merge fixture must compile");
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
}


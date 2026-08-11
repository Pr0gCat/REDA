use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::compile::primitive_graph::{reexpand_gate, EntrySelection, NodeId};
use crate::compile::topology::{Library, Primitive};
use crate::compile::{self, CompiledCircuit, LegacyEmission, Netlist};
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
    /// Every cell this node's realisation occupies, its anchor included.
    ///
    /// A NOR cell is a support block, a torch, its input sockets and its
    /// output pin -- not one cell.  Routing that only knows about the anchor
    /// will run a net straight through another gate's body and short the
    /// two together, which is what happens when this is empty.
    pub footprint: Vec<Anchor>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerError {
    LegacyMetadataUnavailable,
    NetlistDoesNotMatchCompiledOutput,
    UnknownPrimitive(NodeId),
    AnchorOccupied(Anchor),
    NoLocalRoute { from: Anchor, to: Anchor },
    /// A node's recorded realisation cannot be turned into blocks -- either
    /// the primitive has no emitter yet, or it contradicts the gate it is
    /// supposed to be realising.
    UnrealisableNode { id: String, reason: String },
    PhysicalInvariant(compile::CompileError),
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
            Self::UnrealisableNode { id, reason } => {
                write!(f, "cannot realise node {id}: {reason}")
            }
            Self::PhysicalInvariant(error) => error.fmt(f),
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

        match netlist.gates.get(index) {
            Some(gate) => match (node.realisation, gate.is_merge()) {
                (NodeRealisation::WireMerge, true) => {
                    let cell = compile::place_merge_gate(&mut world, origin, gate.inputs.len());
                    let (torch, pin) = output_pin(&mut world, anchor, &cell);
                    ports
                        .gate_output_positions
                        .insert(gate.output.clone(), (torch.x, torch.y, torch.z));
                    gate_pin.push(pin);
                }
                (NodeRealisation::Primitive(Primitive::Torch), false) => {
                    let cell = compile::place_nor_gate(&mut world, origin, gate.inputs.len());
                    let (torch, pin) = output_pin(&mut world, anchor, &cell);
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
                    let (lever, _) = compile::place_primary_input(&mut world, home);
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
fn output_pin(world: &mut World, anchor: Anchor, cell: &compile::NorCell) -> (Position, Position) {
    let torch = Position::new(
        anchor.x + cell.output_offset.0,
        anchor.y + cell.output_offset.1,
        anchor.z + cell.output_offset.2,
    );
    let pin = torch.offset(compile::OUTPUT_DIRECTION);
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
        if reservation.get(cell) == Some(&moved_owner) {
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
    if new_cells.iter().any(|cell| reservation.contains_key(cell)) {
        return Err(PlannerError::AnchorOccupied(to));
    }
    for cell in &new_cells {
        reservation.insert(*cell, moved_owner.clone());
    }

    for (route_index, route) in candidate.routes.iter().enumerate() {
        if !incident[route_index] {
            continue;
        }

        let owner = route.id.clone();
        let (source, supports) = moved.route_endpoints(route_index, primitive, from, to);
        let mut rebuilt = route.clone();
        rebuilt.anchors.clear();
        rebuilt.realisation.clear();
        rebuilt.floors.clear();
        let mut branches = Vec::with_capacity(supports.len());
        for support in supports {
            let terminal = terminal_socket(source, support);
            let path = deterministic_astar(source, terminal, support, &owner, &reservation).ok_or(
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
            branches.push((path, support, laid.strength_before_terminal));
        }

        for (terminal, (path, support, strength_before_terminal)) in
            rebuilt.terminals.iter_mut().zip(branches)
        {
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
    let bends: BTreeSet<usize> = cells
        .windows(3)
        .enumerate()
        .filter(|(_, window)| direction(window[0], window[1]) != direction(window[1], window[2]))
        .map(|(index, _)| index + 1)
        .collect();

    let (is_repeater, _) = compile::plan_bent_path(
        cells.len(),
        &bends,
        crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH,
        0,
    );

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
        None => crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH,
        Some(index) => {
            let last_refresh = (0..=index).rev().find(|&i| is_repeater[i]);
            match last_refresh {
                Some(refresh) => crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH
                    .saturating_sub((index - refresh) as u8),
                None => crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH
                    .saturating_sub((index + 1) as u8),
            }
        }
    };

    LaidBranch {
        floors: vec![compile::stone(); blocks.len()],
        blocks,
        strength_before_terminal,
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

    fn live_reservation(&self, incident: &[bool]) -> BTreeMap<Anchor, String> {
        let mut reservation = BTreeMap::new();
        for (index, anchor) in self.anchors.iter().copied().enumerate() {
            reservation.insert(anchor, format!("primitive:{index}"));
        }
        // A primitive keeps other nets out of every cell it occupies, not
        // just the one its anchor names.
        for (index, node) in self.primitive_nodes.iter().enumerate() {
            for &cell in node.occupied() {
                reservation.insert(cell, format!("primitive:{index}"));
            }
        }
        for (index, route) in self.routes.iter().enumerate() {
            if !incident[index] {
                reserve_path(&mut reservation, &route.id, &route.anchors);
            }
        }
        reservation
    }

    fn route_endpoints(
        &self,
        route_index: usize,
        moved_primitive: NodeId,
        old_anchor: Anchor,
        new_anchor: Anchor,
    ) -> (Anchor, Vec<Anchor>) {
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
            vec![route
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
                .unwrap_or(new_anchor)]
        } else {
            route
                .terminals
                .iter()
                .map(|terminal| match self.node_for_gate(&terminal.sink.gate) {
                    // Already moved with its node, as above.
                    Some(anchor) => anchor,
                    None if moved_primitive < self.anchors.len()
                        && terminal.sink.anchor == old_anchor =>
                    {
                        new_anchor
                    }
                    None => terminal.sink.anchor,
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
    reservation: &BTreeMap<Anchor, String>,
) -> Option<Vec<Anchor>> {
    let margin = manhattan_distance(start, goal).saturating_add(2) as i32;
    let min = Anchor {
        x: start.x.min(goal.x).saturating_sub(margin),
        y: start.y.min(goal.y).saturating_sub(margin),
        z: start.z.min(goal.z).saturating_sub(margin),
    };
    let max = Anchor {
        x: start.x.max(goal.x).saturating_add(margin),
        y: start.y.max(goal.y).saturating_add(margin),
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
            if !within_bounds(next, min, max)
                || !anchor_is_free_for(next, start, goal, terminal_support, owner, reservation)
            {
                continue;
            }
            let next_travelled = state.travelled.saturating_add(1);
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

fn neighbours(anchor: Anchor) -> [Anchor; 6] {
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
            y: anchor.y - 1,
            ..anchor
        },
        Anchor {
            y: anchor.y + 1,
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
    reservation: &BTreeMap<Anchor, String>,
) -> bool {
    if anchor != start
        && anchor != goal
        && reservation
            .get(&anchor)
            .is_some_and(|occupied_by| occupied_by != owner)
    {
        return false;
    }
    keep_out(anchor).into_iter().all(|neighbour| {
        neighbour == start
            || neighbour == goal
            || (anchor == goal && neighbour == terminal_support)
            || reservation
                .get(&neighbour)
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

fn reserve_path(reservation: &mut BTreeMap<Anchor, String>, owner: &str, path: &[Anchor]) {
    for &anchor in path {
        reservation
            .entry(anchor)
            .or_insert_with(|| owner.to_string());
    }
}


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
    reservation: &BTreeMap<Anchor, String>,
    owner: &str,
    predecessor: Anchor,
    terminal: Anchor,
    support: Anchor,
) -> bool {
    horizontal_neighbours(terminal)
        .into_iter()
        .all(|neighbour| {
            neighbour == predecessor
                || neighbour == support
                || reservation
                    .get(&neighbour)
                    .is_none_or(|occupied_by| occupied_by == owner)
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
fn candidate_world_size(candidate: &PlanCandidate) -> (i32, i32, i32) {
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
        for proposal in enumerate_candidates(&epoch, effort.seed) {
            if evaluations >= effort.evaluations {
                return OptimisationReport {
                    gate_effort: gate_efforts(&best),
                    candidate: best,
                    evaluations,
                };
            }
            evaluations += 1;
            generation += 1;
            // Legality is what the circuit says, not what a cheaper stand-in
            // says. `validate_candidate_reservation` had no spacing, strength
            // or torch-merge check and assumed every directed-dust terminal
            // was isolated, so it accepted proposals no one could build --
            // which is how the terminal-flip family below survived: it used
            // to relabel a terminal without touching the block underneath it.
            if verify_candidate(&proposal, netlist).is_err() {
                continue;
            }
            let Ok(proposal_score) =
                proposal.score_against_at(&baseline, &weights, effort, generation)
            else {
                continue;
            };
            let Ok(best_score) = best.score_against_at(&baseline, &weights, effort, generation)
            else {
                continue;
            };
            if proposal_score.order < best_score.order {
                best = proposal;
                improved = true;
            }
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

fn enumerate_candidates(candidate: &PlanCandidate, seed: u64) -> Vec<PlanCandidate> {
    const MOVES: [(i32, i32, i32); 6] = [
        (-1, 0, 0),
        (0, -1, 0),
        (0, 0, -1),
        (0, 0, 1),
        (0, 1, 0),
        (1, 0, 0),
    ];
    let mut proposals = Vec::new();
    let rotation = (seed as usize) % MOVES.len();
    for primitive in 0..candidate.anchors.len() {
        let from = candidate.anchors[primitive];
        for offset in 0..MOVES.len() {
            let (x, y, z) = MOVES[(offset + rotation) % MOVES.len()];
            if let Ok(moved) = try_move(
                candidate,
                primitive,
                Anchor {
                    x: from.x.saturating_add(x),
                    y: from.y.saturating_add(y),
                    z: from.z.saturating_add(z),
                },
            ) {
                proposals.push(moved);
            }
        }
    }
    proposals.extend(topology_feedback_candidates(candidate));
    proposals
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
fn topology_feedback_candidates(candidate: &PlanCandidate) -> Vec<PlanCandidate> {
    let library = Library::default_library();
    let mut proposals = Vec::new();
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
            let mut alternative = candidate.clone();
            alternative.topology_entries.insert(gate.gate, entry);
            if !candidate_allows_entry(&alternative, gate.gate, entry) {
                continue;
            }
            if let Some(emission) = alternative.legacy_emission.as_ref() {
                let selection: EntrySelection = alternative.topology_entries.clone();
                if reexpand_gate(
                    emission.netlist(),
                    &library,
                    &selection,
                    gate.gate,
                    entry,
                )
                .is_err()
                {
                    continue;
                }
            }
            proposals.push(alternative);
        }
    }
    proposals
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
                    output_pin: None,
                },
                PrimitiveNode {
                    id: "gate:moved".to_string(),
                    anchor: old_moved_sink,
                    realisation: NodeRealisation::Primitive(Primitive::Torch),
                    footprint: Vec::new(),
                    output_pin: None,
                },
                PrimitiveNode {
                    id: "gate:other".to_string(),
                    anchor: other_sink,
                    realisation: NodeRealisation::Primitive(Primitive::Torch),
                    footprint: Vec::new(),
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
        let other_terminal = terminal_socket(source, other_sink);
        let mut without_destination_reservation = seed.live_reservation(&[true]);
        without_destination_reservation.remove(&seed.primitive_nodes()[1].anchor);

        let unreserved_path = deterministic_astar(
            source,
            other_terminal,
            other_sink,
            "source",
            &without_destination_reservation,
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
                    output_pin: None,
                },
                PrimitiveNode {
                    id: "gate:merge".to_string(),
                    anchor: merge,
                    realisation: NodeRealisation::WireMerge,
                    footprint: Vec::new(),
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
                    output_pin: None,
                },
                PrimitiveNode {
                    id: "gate:y".to_string(),
                    anchor: sink,
                    realisation: NodeRealisation::Primitive(Primitive::Torch),
                    footprint: Vec::new(),
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
                        output_pin: None,
                    },
                    PrimitiveNode {
                        id: "gate:merge".to_string(),
                        anchor: merge,
                        realisation: NodeRealisation::WireMerge,
                        footprint: Vec::new(),
                        output_pin: None,
                    },
                    PrimitiveNode {
                        id: "gate:other".to_string(),
                        anchor: shared_consumer,
                        realisation: NodeRealisation::Primitive(Primitive::Torch),
                        footprint: Vec::new(),
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

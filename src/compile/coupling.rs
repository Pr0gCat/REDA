//! The **complete** electrical graph of a realised world, compared against the
//! graph the netlist and the topology intend.
//!
//! # Why this exists
//!
//! `topology.rs`'s module doc defends a principle this compiler is built on: a
//! gate is a *topology*, physical position is abstracted away, and if the
//! topology is right any physical form realises it. That holds exactly as long
//! as realisation adds **no edges**. Twice on this branch it did, and twice a
//! circuit passed all four physical invariants while computing the wrong
//! function:
//!
//! * a lit lever strongly powers the block above it; a route's floor landed
//!   there and its dust read 15 from an input it was never connected to;
//! * a lit gate torch strongly powers the block above it; `full_adder` routed
//!   over one and eight of its 22 gates came out wrong.
//!
//! Neither had a dust-to-dust edge anywhere, and [`super::verify_connectivity`]
//! walks `connectivity::dust_connections` and nothing else -- dust to dust, one
//! mechanism out of the four `docs/derived/coupling-mechanisms.md` measured.
//! Both bugs lived in an unchecked one.
//!
//! This module widens the edge relation rather than inventing a second checker.
//! It keeps `verify_connectivity`'s granularity (nets, compared through
//! [`super::MergeGroups`] so a declared wire merge is not a violation) and
//! replaces its one relation with all four.
//!
//! # The four mechanisms, and where each one is implemented here
//!
//! Numbered as `docs/derived/coupling-mechanisms.md` numbers them. That
//! artifact is 279 lines of `Simulator` runs, each differenced against a
//! control; nothing below re-argues it.
//!
//! 1. **dust <-> dust** -- [`Mechanism::DustToDust`], via `dust_connections`,
//!    the same call `verify_connectivity` makes. Taken in *both* directions
//!    from every reached cell, which is the one place this deliberately
//!    differs from that walk: a one-way `dust_connections` edge still makes two
//!    nets one electrical node, and the shipping walk's shared `visited` set
//!    plus its lowest-`y`-first seed order can leave such a pair in two
//!    components (Table 4, and `verify_connectivity_misses_a_one_way_dust_edge_
//!    that_runs_against_its_seed_order` in `super`'s own test module).
//! 2. **component -> adjacent dust** -- [`Mechanism::ComponentToDust`].
//! 3. **component -> block -> dust** -- [`Mechanism::StrongBlockToDust`]. Needs
//!    the arriving power to be `Strong` *and* the block to conduct, and then
//!    the block drives dust on **every** face it has left. **Both shipped bugs
//!    are this mechanism.**
//! 4. **component/dust -> block -> torch support or diode rear, on *weak*
//!    power** -- [`Mechanism::BlockToReader`]. No dust probe anywhere can find
//!    this class: weak power never re-drives dust (mechanism 5, which does not
//!    exist), so the coupling is invisible until something *reads* the block.
//!
//! Mechanism 6 (block -> block) and mechanism 7 (torch -> its own support) do
//! not exist, so the walk below never conducts block to block and a torch is
//! never in its own support's reach.
//!
//! # Activation is deliberately ignored
//!
//! A compiled world is not settled: `place_nor_gate` writes torches pre-lit
//! unconditionally, `place_primary_input` writes every lever **off**, and every
//! dust cell has `power == 0` because nothing has run
//! `recompute_dust_strengths` yet. Asking "is this lever currently emitting"
//! would therefore answer nothing about the geometry -- and would in particular
//! answer *no* for the lever bug above. So the extraction is **structural**:
//! which edges a component of this kind, in this orientation, is even capable
//! of. That is exactly the axis [`super::structural_output`] already isolates
//! for `verify_torch_merge`, reused here rather than restated.
//!
//! Rule 6 says derive from the simulator rather than restate its rules, and a
//! structural predicate cannot be read off a settled world. What can be, and
//! is: [`tests::the_extractor_agrees_with_the_simulator`] sweeps every emitter
//! this compiler writes against every mediator material and every direction,
//! drives the emitter, differences the receiver against a control with the
//! emitter written as air, and fails unless the extractor's answer matches the
//! simulator's on every cell of the sweep. The structural predicate is thereby
//! *measured*, not argued.
//!
//! # What counts as an extra edge
//!
//! A **domain** is one electrical source and everything the netlist says it
//! drives: a routed net (its `Reservation` cells, its merge-gate body cells,
//! and the torch or lever that sources it), or a gate whose output reaches only
//! a lamp and therefore has no `Net` at all.
//!
//! An **extra edge** is a cell in domain `A`'s reach that some *other* domain
//! owns, where the netlist does not join the two (`MergeGroups::same_group`).
//! A **foreign reader** is a component -- a gate's output torch, or a diode --
//! that reads a cell carrying a domain the netlist never wired to it. The
//! second is reported separately because it usually has no second net to name:
//! the cell being read is a support block, which belongs to no route.
//!
//! # This module only measures
//!
//! Nothing here is called by `compile`. It changes no placement, no routing and
//! no clearance. If a shipping circuit has an extra edge, that is a result to
//! report, not a thing to fix from inside a measurement.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::redstone::rules::taxonomy::{flags_of, BlockPower};
use crate::redstone::simulator::component::{
    comparator_rear_position, repeater_input_position, torch_support_position,
};
use crate::redstone::simulator::connectivity::{dust_connections, dust_powers_block_toward};
use crate::redstone::simulator::position::{Position, ALL_SIX, HORIZONTAL};
use crate::redstone::world::block::{BlockKind, Facing};
use crate::redstone::world::storage::World;

use super::{
    merge_gate_body_owners, net_name, structural_output, MergeGroups, Net, Netlist, Reservation,
    Source,
};

/// Which measured mechanism carried one edge.
///
/// The numbers are `docs/derived/coupling-mechanisms.md`'s own numbering, so a
/// finding can be looked up in the artifact that measured its mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mechanism {
    /// 1 -- `connectivity::dust_connections`, in either direction.
    DustToDust,
    /// 2 -- a component drives a dust cell touching it, no block between.
    ComponentToDust,
    /// 3 -- a component strongly powers a conductive block, and that block
    /// re-drives dust on every face it has left. **Both shipped bugs.**
    StrongBlockToDust,
    /// 4 -- a block powered at all (weak counts) read by a torch attached to it
    /// or by a diode whose rear it is. Invisible to any dust probe.
    BlockToReader,
    /// A cell of dust read directly by a diode. Not a numbered mechanism in the
    /// artifact -- it is the ordinary way a route terminates -- but it is an
    /// edge, so it is extracted and labelled rather than silently skipped.
    DustToReader,
}

impl Mechanism {
    pub fn number(self) -> &'static str {
        match self {
            Mechanism::DustToDust => "1 (dust <-> dust)",
            Mechanism::ComponentToDust => "2 (component -> dust)",
            Mechanism::StrongBlockToDust => "3 (component -> block -> dust)",
            Mechanism::BlockToReader => "4 (block -> torch support / diode rear)",
            Mechanism::DustToReader => "- (dust -> diode)",
        }
    }
}

/// One edge realisation added: two cells carrying different nets, electrically
/// connected, with nothing in the netlist joining them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExtraEdge {
    /// The domain whose power arrives.
    pub from_domain: String,
    /// The cell the last hop left from.
    pub from_cell: (i32, i32, i32),
    /// The foreign net that owns the cell reached.
    pub to_net: String,
    /// The cell reached.
    pub to_cell: (i32, i32, i32),
    /// The mediating block, when the mechanism has one.
    pub via: Option<(i32, i32, i32)>,
    pub mechanism: Mechanism,
}

impl std::fmt::Display for ExtraEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {:?} -> {} at {:?}",
            self.from_domain, self.from_cell, self.to_net, self.to_cell
        )?;
        if let Some(via) = self.via {
            write!(f, " across {via:?}")?;
        }
        write!(f, ", mechanism {}", self.mechanism.number())
    }
}

/// A component reading a domain the netlist never wired to it.
///
/// Reported apart from [`ExtraEdge`] because the cell being read -- a gate's
/// support block, or a diode's rear -- usually belongs to no route, so there is
/// no second net to name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ForeignReader {
    /// The domain whose power arrives.
    pub from_domain: String,
    /// What reads it: the gate whose output torch this is, or what kind of
    /// diode it is.
    pub reader: String,
    pub reader_cell: (i32, i32, i32),
    /// The cell being read.
    pub read_cell: (i32, i32, i32),
    /// Which net owns the cell being read, when any does.
    ///
    /// This is what separates a *new* coupling from a consequence of one
    /// already listed as an [`ExtraEdge`]: a support block belongs to no route
    /// and reads `None`, while a cell belonging to some other net means the
    /// domain got there through a crossing that is reported separately, and this
    /// line is saying the contamination is then *read* rather than merely
    /// sitting on a wire.
    pub read_cell_net: Option<String>,
    pub mechanism: Mechanism,
}

impl std::fmt::Display for ForeignReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} reaches {:?}",
            self.from_domain, self.read_cell
        )?;
        match &self.read_cell_net {
            Some(owner) => write!(f, " (owned by {owner})")?,
            None => write!(f, " (owned by no route)")?,
        }
        write!(
            f,
            ", read by {} at {:?}, mechanism {}",
            self.reader,
            self.reader_cell,
            self.mechanism.number()
        )
    }
}

/// Everything the extraction found, plus what it walked -- so a clean result
/// can be told apart from a walk that never ran.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The **crossings**: hops that leave a domain's own territory and land on
    /// a foreign net's cell. One entry per coupling, not one per contaminated
    /// cell -- see `contaminated_cells` for the second number.
    pub extra_edges: Vec<ExtraEdge>,
    pub foreign_readers: Vec<ForeignReader>,
    /// How many domains were seeded. Zero means nothing was measured.
    pub domains: usize,
    /// Cells reached, summed over every domain.
    pub reached_cells: usize,
    /// Foreign cells reached, summed over every domain. One crossing usually
    /// contaminates a whole run of them, so this is the size of the damage and
    /// `extra_edges.len()` is the number of causes.
    pub contaminated_cells: usize,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.extra_edges.is_empty() && self.foreign_readers.is_empty()
    }

    /// One line per finding, for a test's panic message.
    pub fn describe(&self) -> String {
        let mut lines = Vec::new();
        for edge in &self.extra_edges {
            lines.push(format!("  EXTRA EDGE   {edge}"));
        }
        for reader in &self.foreign_readers {
            lines.push(format!("  FOREIGN READ {reader}"));
        }
        lines.join("\n")
    }
}

/// One electrical source and everything the netlist says it drives.
struct Domain {
    label: String,
    /// The net index, when this domain is a routed net. `None` for a gate whose
    /// output reaches only a lamp: nothing routes it, so no net exists, but its
    /// torch still emits and can still leak.
    net: Option<usize>,
    seeds: Vec<Position>,
}

/// Extract the realised world's complete electrical graph and report every edge
/// the netlist did not ask for.
///
/// Takes exactly what [`super::verify_connectivity`] takes, plus
/// `input_positions` -- because a lever is a source with no `Reservation` cell
/// of its own, and one of the two shipped bugs was a lever leaking upward.
pub(crate) fn extra_edges(
    world: &World,
    reservation: &Reservation,
    netlist: &Netlist,
    nets: &[Net],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
    input_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> Report {
    let groups = MergeGroups::build(netlist, nets);
    let body_owners = merge_gate_body_owners(netlist, nets, gate_output_positions);
    let owner_of = |pos: &Position| -> Option<usize> {
        reservation
            .get(pos)
            .or_else(|| body_owners.get(pos))
            .copied()
    };

    let readers = readers_of(world, netlist, gate_output_positions);
    let domains = build_domains(
        world,
        reservation,
        &body_owners,
        netlist,
        nets,
        gate_output_positions,
        input_positions,
    );

    let mut report = Report {
        domains: domains.len(),
        ..Report::default()
    };

    for domain in &domains {
        let reach = reach_of(world, &domain.seeds);
        report.reached_cells += reach.arrival.len();

        for (&cell, arrival) in &reach.arrival {
            let Some(found) = owner_of(&cell) else {
                continue;
            };
            if domain_owns(domain, found, &groups) {
                continue;
            }
            report.contaminated_cells += 1;
            // Only the hop that *crosses* is a finding. Once a domain is inside
            // a foreign net's run it walks the rest of that run by ordinary
            // dust-to-dust conduction, and reporting each of those as its own
            // extra edge would report the size of the damage as the number of
            // causes -- seven findings where there is one lever and one
            // mediator to look at.
            let predecessor_is_the_same_foreign_net = owner_of(&arrival.from)
                .is_some_and(|previous| previous == found || groups.same_group(previous, found));
            if predecessor_is_the_same_foreign_net {
                continue;
            }
            report.extra_edges.push(ExtraEdge {
                from_domain: domain.label.clone(),
                from_cell: (arrival.from.x, arrival.from.y, arrival.from.z),
                to_net: net_name(netlist, nets, found),
                to_cell: (cell.x, cell.y, cell.z),
                via: arrival.via.map(|v| (v.x, v.y, v.z)),
                mechanism: arrival.mechanism,
            });
        }

        for (&cell, &mechanism) in &reach.readable {
            for reader in readers.get(&cell).into_iter().flatten() {
                if reader_accepts(reader, domain, nets, &groups, &owner_of) {
                    continue;
                }
                report.foreign_readers.push(ForeignReader {
                    from_domain: domain.label.clone(),
                    reader: reader.label.clone(),
                    reader_cell: (reader.cell.x, reader.cell.y, reader.cell.z),
                    read_cell: (cell.x, cell.y, cell.z),
                    read_cell_net: owner_of(&cell).map(|net| net_name(netlist, nets, net)),
                    mechanism,
                });
            }
        }
    }

    report.extra_edges.sort();
    report.extra_edges.dedup();
    report.foreign_readers.sort();
    report.foreign_readers.dedup();
    report
}

/// Whether `found` is a net this domain is allowed to be electrically one with.
fn domain_owns(domain: &Domain, found: usize, groups: &MergeGroups) -> bool {
    match domain.net {
        Some(net) => net == found || groups.same_group(net, found),
        // A gate with no net of its own routes nothing, so every routed cell it
        // reaches is foreign.
        None => false,
    }
}

// ---------------------------------------------------------------------
// The domains
// ---------------------------------------------------------------------

fn build_domains(
    world: &World,
    reservation: &Reservation,
    body_owners: &HashMap<Position, usize>,
    netlist: &Netlist,
    nets: &[Net],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
    input_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> Vec<Domain> {
    let mut cells_of: Vec<Vec<Position>> = vec![Vec::new(); nets.len()];
    for (&pos, &owner) in reservation {
        cells_of[owner].push(pos);
    }
    for (&pos, &owner) in body_owners {
        cells_of[owner].push(pos);
    }

    let mut domains = Vec::with_capacity(nets.len() + netlist.gates.len());
    let mut sourced_gates: HashSet<usize> = HashSet::new();
    let mut sourced_inputs: HashSet<usize> = HashSet::new();

    for (index, net) in nets.iter().enumerate() {
        let mut seeds = std::mem::take(&mut cells_of[index]);
        // The source component itself. It carries no `Reservation` entry -- a
        // lever is a primitive, not a route cell -- and it is exactly what
        // leaked in both shipped bugs, so a walk seeded only from routed cells
        // would find neither.
        match net.source {
            Source::Gate(gate) => {
                sourced_gates.insert(gate);
                if let Some(&(x, y, z)) = gate_output_positions.get(&netlist.gates[gate].output) {
                    seeds.push(Position::new(x, y, z));
                }
            }
            Source::Lever(input) => {
                sourced_inputs.insert(input);
                if let Some(name) = netlist.inputs.get(input) {
                    if let Some(&(x, y, z)) = input_positions.get(name) {
                        seeds.push(Position::new(x, y, z));
                    }
                }
            }
        }
        seeds.sort_by_key(|p| (p.y, p.z, p.x));
        seeds.dedup();
        domains.push(Domain {
            label: net_name(netlist, nets, index),
            net: Some(index),
            seeds,
        });
    }

    // A gate whose output reaches only a declared circuit-output lamp has no
    // `Net` (`build_nets` drops a signal with no gate-input sink), so the loop
    // above never seeded it -- and its torch emits exactly as hard as any other
    // gate's. A merge in the same position is already covered: its junction is
    // dust and `merge_gate_body_owners` gives it a group root, so it arrived
    // above as part of that group's cells.
    for (gate, definition) in netlist.gates.iter().enumerate() {
        if sourced_gates.contains(&gate) || definition.is_merge() {
            continue;
        }
        let Some(&(x, y, z)) = gate_output_positions.get(&definition.output) else {
            continue;
        };
        domains.push(Domain {
            label: definition.output.clone(),
            net: None,
            seeds: vec![Position::new(x, y, z)],
        });
    }

    // A declared primary input driving nothing still gets a lever placed, and a
    // lever still leaks. Nothing in this project builds such a netlist today,
    // so this arm keeps a future one from being silently unmeasured rather than
    // covering a case with a fixture.
    for (input, name) in netlist.inputs.iter().enumerate() {
        if sourced_inputs.contains(&input) {
            continue;
        }
        let Some(&(x, y, z)) = input_positions.get(name) else {
            continue;
        };
        if world.get(x, y, z).kind == BlockKind::Air {
            continue;
        }
        domains.push(Domain {
            label: name.clone(),
            net: None,
            seeds: vec![Position::new(x, y, z)],
        });
    }

    domains
}

// ---------------------------------------------------------------------
// The reach walk
// ---------------------------------------------------------------------

/// How one cell was first reached: by which mechanism, from where, across what.
#[derive(Debug, Clone, Copy)]
struct Arrival {
    mechanism: Mechanism,
    from: Position,
    via: Option<Position>,
}

struct Reach {
    /// Every cell this domain's power reaches, and the first hop that got
    /// there.
    arrival: HashMap<Position, Arrival>,
    /// Every cell something could *read* this domain from: a block powered at
    /// all (weak counts -- mechanism 4's whole class lives here and nowhere
    /// else), and every dust cell reached (which a diode reads directly).
    readable: HashMap<Position, Mechanism>,
}

/// Every cell `seeds`' power can reach, walking all four measured mechanisms.
///
/// The walk conducts through dust and through **strongly** powered conductive
/// blocks, and stops at every active component: a torch, a repeater or a
/// comparator is a new source, not a wire, so its output is a different
/// domain's business. That is what keeps this from collapsing the whole circuit
/// into one component through its own gates.
///
/// It never conducts block to block -- mechanism 6, which does not exist -- so
/// a powered block's neighbours are expanded as dust, never re-powered.
fn reach_of(world: &World, seeds: &[Position]) -> Reach {
    let mut reach = Reach {
        arrival: HashMap::new(),
        readable: HashMap::new(),
    };
    let mut queue: VecDeque<Position> = VecDeque::new();

    for &seed in seeds {
        visit(
            &mut reach,
            &mut queue,
            seed,
            Mechanism::DustToDust,
            seed,
            None,
        );
    }

    while let Some(pos) = queue.pop_front() {
        let state = world.get(pos.x, pos.y, pos.z);
        let is_dust = state.kind == BlockKind::RedstoneWire;

        if is_dust {
            reach.readable.insert(pos, Mechanism::DustToReader);

            // Mechanism 1, in both directions from every cell reached: an edge
            // that exists only one way still makes the two cells one
            // electrical node, and `verify_connectivity`'s forward-only walk is
            // exactly what Table 4 catches missing one of.
            for direction in HORIZONTAL {
                for next in dust_connections(world, pos, direction).iter() {
                    visit(&mut reach, &mut queue, next, Mechanism::DustToDust, pos, None);
                }
                // The reverse edge: any dust cell whose own `dust_connections`
                // in the opposite direction lands on `pos`. Those are exactly
                // `pos + direction` and its two vertical neighbours.
                let ahead = pos.offset(direction);
                for candidate in [ahead, ahead.up(), ahead.down()] {
                    if world.get(candidate.x, candidate.y, candidate.z).kind
                        != BlockKind::RedstoneWire
                    {
                        continue;
                    }
                    if dust_connections(world, candidate, direction.opposite())
                        .iter()
                        .any(|reached| reached == pos)
                    {
                        visit(
                            &mut reach,
                            &mut queue,
                            candidate,
                            Mechanism::DustToDust,
                            pos,
                            None,
                        );
                    }
                }
            }
        }

        for direction in ALL_SIX {
            let neighbour = pos.offset(direction);
            let (drives_dust, block_power) = emission(world, pos, direction);

            // Mechanism 2. Skipped for a dust emitter: dust reaching dust is
            // mechanism 1's shape-dependent relation above, not a flat
            // six-direction drive, and labelling it twice would put the wrong
            // mechanism number on a real finding.
            if drives_dust
                && !is_dust
                && world.get(neighbour.x, neighbour.y, neighbour.z).kind == BlockKind::RedstoneWire
            {
                visit(
                    &mut reach,
                    &mut queue,
                    neighbour,
                    Mechanism::ComponentToDust,
                    pos,
                    None,
                );
            }

            if block_power == BlockPower::None {
                continue;
            }
            if !flags_of(world.get(neighbour.x, neighbour.y, neighbour.z)).is_conductive() {
                continue;
            }

            // Mechanism 4's precondition: the block is powered at all. What (if
            // anything) reads it is decided by the caller against `readers_of`.
            reach
                .readable
                .entry(neighbour)
                .or_insert(Mechanism::BlockToReader);

            // Mechanism 3, and only on `Strong`: a weakly powered block never
            // re-drives dust (mechanism 5, which does not exist).
            if block_power != BlockPower::Strong {
                continue;
            }
            for outward in ALL_SIX {
                let driven = neighbour.offset(outward);
                if world.get(driven.x, driven.y, driven.z).kind == BlockKind::RedstoneWire {
                    visit(
                        &mut reach,
                        &mut queue,
                        driven,
                        Mechanism::StrongBlockToDust,
                        pos,
                        Some(neighbour),
                    );
                }
            }
        }
    }

    reach
}

/// Record a first arrival and queue the cell. A cell already reached keeps the
/// mechanism that got there first, so a cell genuinely inside this domain's own
/// network is never relabelled by a later, longer path.
fn visit(
    reach: &mut Reach,
    queue: &mut VecDeque<Position>,
    cell: Position,
    mechanism: Mechanism,
    from: Position,
    via: Option<Position>,
) {
    if let std::collections::hash_map::Entry::Vacant(slot) = reach.arrival.entry(cell) {
        slot.insert(Arrival {
            mechanism,
            from,
            via,
        });
        queue.push_back(cell);
    }
}

/// What `pos` emits toward `direction`, with every activation gate dropped.
///
/// [`super::structural_output`] answers this for every kind but dust, whose
/// horizontal output depends on its connection *shape* and therefore on the
/// world rather than on the `BlockState`. `dust_powers_block_toward` is the
/// simulator's own measured predicate for that, asked in its geometry-only form
/// -- the same correction [`super::net_reach`] applies for the same reason.
fn emission(world: &World, pos: Position, direction: Facing) -> (bool, BlockPower) {
    let state = world.get(pos.x, pos.y, pos.z);
    if state.kind == BlockKind::RedstoneWire {
        let power = if dust_powers_block_toward(world, pos, direction) {
            BlockPower::Weak
        } else {
            BlockPower::None
        };
        return (false, power);
    }
    structural_output(state, direction)
}

// ---------------------------------------------------------------------
// Who reads a cell
// ---------------------------------------------------------------------

/// A component that reads a cell it is not itself part of.
struct Reader {
    label: String,
    cell: Position,
    kind: ReaderKind,
}

enum ReaderKind {
    /// A gate's own output torch: the cell read is that gate's input node, so
    /// the domains allowed to reach it are exactly the gate's declared inputs.
    GateSupport(usize),
    /// A diode reading its rear. Legitimate only for the net that owns the
    /// diode.
    DiodeRear,
    /// A torch this scan could not attribute to any gate. Reported rather than
    /// dropped: an unattributed torch is a hole in the map, not a non-finding.
    UnattributedTorch,
}

/// Every cell in the world that something reads, and what reads it.
fn readers_of(
    world: &World,
    netlist: &Netlist,
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> HashMap<Position, Vec<Reader>> {
    let gate_of_torch: HashMap<(i32, i32, i32), usize> = netlist
        .gates
        .iter()
        .enumerate()
        .filter(|(_, gate)| !gate.is_merge())
        .filter_map(|(index, gate)| {
            gate_output_positions
                .get(&gate.output)
                .map(|&pos| (pos, index))
        })
        .collect();

    let mut readers: HashMap<Position, Vec<Reader>> = HashMap::new();
    let mut add = |cell: Position, reader: Reader| {
        readers.entry(cell).or_default().push(reader);
    };

    for kind in [
        BlockKind::Torch,
        BlockKind::WallTorch,
        BlockKind::Repeater,
        BlockKind::Comparator,
    ] {
        for flat in world.positions_of(kind).collect::<Vec<_>>() {
            let (x, y, z) = world.decode(flat);
            let pos = Position::new(x, y, z);
            let state = world.get(x, y, z);
            match kind {
                BlockKind::Torch | BlockKind::WallTorch => {
                    let Some(support) = torch_support_position(state, pos) else {
                        continue;
                    };
                    let (label, reader_kind) = match gate_of_torch.get(&(x, y, z)) {
                        Some(&gate) => (
                            format!("gate {}'s torch", netlist.gates[gate].output),
                            ReaderKind::GateSupport(gate),
                        ),
                        None => ("an unattributed torch".to_string(), ReaderKind::UnattributedTorch),
                    };
                    add(
                        support,
                        Reader {
                            label,
                            cell: pos,
                            kind: reader_kind,
                        },
                    );
                }
                BlockKind::Repeater => {
                    if let Some(rear) = repeater_input_position(state, pos) {
                        add(
                            rear,
                            Reader {
                                label: "a repeater".to_string(),
                                cell: pos,
                                kind: ReaderKind::DiodeRear,
                            },
                        );
                    }
                }
                BlockKind::Comparator => {
                    if let Some(rear) = comparator_rear_position(state, pos) {
                        add(
                            rear,
                            Reader {
                                label: "a comparator".to_string(),
                                cell: pos,
                                kind: ReaderKind::DiodeRear,
                            },
                        );
                    }
                }
                _ => unreachable!("the loop above lists exactly four kinds"),
            }
        }
    }
    readers
}

/// Whether this reader is allowed to see this domain.
fn reader_accepts(
    reader: &Reader,
    domain: &Domain,
    nets: &[Net],
    groups: &MergeGroups,
    owner_of: &dyn Fn(&Position) -> Option<usize>,
) -> bool {
    match reader.kind {
        ReaderKind::GateSupport(gate) => {
            // A gate's support is its input node: the domains that may reach it
            // are exactly the nets driving its declared inputs, widened by the
            // same `MergeGroups` `verify_torch_merge` uses.
            let Some(net) = domain.net else {
                return false;
            };
            nets.iter().enumerate().any(|(index, candidate)| {
                candidate
                    .sinks
                    .iter()
                    .flatten()
                    .any(|&(sink_gate, _)| sink_gate == gate)
                    && (index == net || groups.same_group(index, net))
            })
        }
        ReaderKind::DiodeRear => {
            // A diode is a route's own cell; the only domain entitled to drive
            // its rear is the net that owns it. A diode owned by nobody is
            // reported, which is the safe side.
            let Some(owner) = owner_of(&reader.cell) else {
                return false;
            };
            match domain.net {
                Some(net) => net == owner || groups.same_group(net, owner),
                None => false,
            }
        }
        ReaderKind::UnattributedTorch => false,
    }
}

#[cfg(test)]
mod tests;

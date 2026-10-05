//! Retained-history quality selection: a two-input/three-output player family
//! a balanced four-input matrix, and flat refresh candidates up to six inputs.
//! No function IDs, source names, block counts or volume select a candidate.
use super::fragment_synth::{
    rewrite::realise_physical_rewrite_with_size,
    torch_row::{compile_balanced_torch_matrix, compile_player_torch_row},
};
use super::{CompileError, CompiledCircuit, Netlist};
use crate::redstone::{
    simulator::{position::Position, Simulator},
    world::{block::BlockKind, storage::World},
};
use std::collections::{BTreeSet, HashMap};

pub const METRICS: [&str; 12] = [
    "worst_settle_ticks",
    "total_settle_ticks",
    "driver_glitches",
    "driver_single_input_glitches",
    "driver_last_change_ticks",
    "driver_total_pulse_ticks",
    "driver_worst_pulse_ticks",
    "lamp_glitches",
    "lamp_single_input_glitches",
    "lamp_last_change_ticks",
    "lamp_total_pulse_ticks",
    "lamp_worst_pulse_ticks",
];
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quality {
    pub setup: [u64; 12],
    pub target: [u64; 12],
}
impl Quality {
    pub fn dominates(&self, other: &Self) -> bool {
        self.setup
            .iter()
            .chain(&self.target)
            .zip(other.setup.iter().chain(&other.target))
            .all(|(a, b)| a <= b)
    }
}
fn position((x, y, z): (i32, i32, i32)) -> Position {
    Position::new(x, y, z)
}
fn signal(world: &World, p: Position) -> Option<bool> {
    let block = world.get(p.x, p.y, p.z);
    match block.kind {
        BlockKind::RedstoneWire => Some(block.power > 0),
        BlockKind::Torch
        | BlockKind::WallTorch
        | BlockKind::Repeater
        | BlockKind::Comparator
        | BlockKind::Lever
        | BlockKind::Lamp => Some(block.lit),
        _ => None,
    }
}
fn values<'a>(
    netlist: &'a Netlist,
    order: &[usize],
    inputs: impl Iterator<Item = (&'a str, bool)>,
) -> Option<HashMap<&'a str, bool>> {
    let mut values: HashMap<_, _> = inputs.collect();
    for &index in order {
        let gate = &netlist.gates[index];
        let operands = gate
            .inputs
            .iter()
            .map(|name| values.get(name.as_str()).copied())
            .collect::<Option<Vec<_>>>()?;
        values.insert(&gate.output, gate.kind.evaluate(&operands));
    }
    Some(values)
}

pub struct Phase<'a> {
    pub from: usize,
    pub to: usize,
    pub phase: usize,
    pub address: usize,
    pub ticks: u64,
    pub start: u64,
    pub expected: [bool; 3],
    pub driver: [bool; 3],
    pub lamp: [bool; 3],
    pub events: &'a [crate::redstone::simulator::observer::Observation],
}

/// Complete two-input/three-output continuous-history qualification. Each
/// declared lamp is distinct, and every implemented gate must agree in all32
/// phases. Only observer records are cleared; the simulator history is retained.
pub fn qualify(source: &Netlist, compiled: &CompiledCircuit) -> Option<Quality> {
    qualify_with_phases(source, compiled, |_| {})
}

pub fn qualify_with_phases(
    source: &Netlist,
    compiled: &CompiledCircuit,
    mut record: impl FnMut(Phase<'_>),
) -> Option<Quality> {
    if source.inputs.len() != 2 || source.outputs.len() != 3 {
        return None;
    }
    qualify_boundary_with_phases(source, compiled, |p| {
        record(Phase {
            from: p.from,
            to: p.to,
            phase: p.phase,
            address: p.address,
            ticks: p.ticks,
            start: p.start,
            expected: p.expected.try_into().expect("three source outputs"),
            driver: p.driver.try_into().expect("three source drivers"),
            lamp: p.lamp.try_into().expect("three source lamps"),
            events: p.events,
        })
    })
}

/// Complete retained-history qualification for up to six combinational inputs.
/// Every source output and implemented gate is checked; outputs never truncate.
pub fn qualify_boundary(source: &Netlist, compiled: &CompiledCircuit) -> Option<Quality> {
    qualify_boundary_with_phases(source, compiled, |_| {})
}

pub struct BoundaryPhase<'a> {
    pub from: usize,
    pub to: usize,
    pub phase: usize,
    pub address: usize,
    pub ticks: u64,
    pub start: u64,
    pub expected: Vec<bool>,
    pub driver: Vec<bool>,
    pub lamp: Vec<bool>,
    pub events: &'a [crate::redstone::simulator::observer::Observation],
}

pub fn qualify_boundary_with_phases(
    source: &Netlist,
    compiled: &CompiledCircuit,
    record: impl FnMut(BoundaryPhase<'_>),
) -> Option<Quality> {
    qualify_boundary_impl(source, compiled, true, record)
}

fn qualify_boundary_impl(
    source: &Netlist,
    compiled: &CompiledCircuit,
    require_certificate: bool,
    record: impl FnMut(BoundaryPhase<'_>),
) -> Option<Quality> {
    qualify_boundary_ports(source, compiled, require_certificate, None, record)
}

fn qualify_boundary_ports(
    source: &Netlist,
    compiled: &CompiledCircuit,
    require_certificate: bool,
    caller_ports: Option<&super::planner::PortPlacements>,
    mut record: impl FnMut(BoundaryPhase<'_>),
) -> Option<Quality> {
    qualify_boundary_ports_while(
        source, compiled, require_certificate, caller_ports, None,
        |phase| { record(phase); true },
    )
}

// A failed dominance bound is already a complete rejection proof. Only a
// candidate that reaches the end can return a qualified quality profile.
fn qualify_boundary_ports_while(
    source: &Netlist,
    compiled: &CompiledCircuit,
    require_certificate: bool,
    caller_ports: Option<&super::planner::PortPlacements>,
    upper: Option<&Quality>,
    mut record: impl FnMut(BoundaryPhase<'_>) -> bool,
) -> Option<Quality> {
    if require_certificate {
        compiled.physical_rewrite()?;
    }
    if !(1..=6).contains(&source.inputs.len())
        || source.outputs.is_empty()
        || source.gates.iter().any(|g| g.kind.is_sequential())
    {
        return None;
    }
    compiled.validate_rewrite_view(source).ok()?;
    let inputs = source
        .inputs
        .iter()
        .map(|name| compiled.input_positions.get(name).copied().map(position))
        .collect::<Option<Vec<_>>>()?;
    let mut measured_world = compiled.world.clone();
    for (name, p) in source.inputs.iter().zip(&inputs) {
        let caller = caller_ports.is_some_and(|ports| ports.get(name).is_some());
        if measured_world.get(p.x, p.y, p.z).kind
            != if caller { BlockKind::Air } else { BlockKind::Lever }
        {
            return None;
        }
    }
    let (drivers, lamps) = if let Some(rewrite) = compiled.physical_rewrite() {
        (
            rewrite
                .source_output_driver_positions()
                .iter()
                .copied()
                .map(position)
                .collect::<Vec<_>>(),
            rewrite
                .source_output_positions()
                .iter()
                .copied()
                .map(position)
                .collect::<Vec<_>>(),
        )
    } else {
        (
            source
                .outputs
                .iter()
                .map(|name| {
                    compiled
                        .gate_output_positions
                        .get(name)
                        .copied()
                        .map(position)
                })
                .collect::<Option<Vec<_>>>()?,
            source
                .outputs
                .iter()
                .map(|name| compiled.output_positions.get(name).copied().map(position))
                .collect::<Option<Vec<_>>>()?,
        )
    };
    for (name, p) in source.outputs.iter().zip(&lamps) {
        if caller_ports.is_some_and(|ports| ports.get(name).is_some()) {
            if measured_world.get(p.x, p.y, p.z).kind != BlockKind::Air {
                return None;
            }
            super::probe_caller_cell(&mut measured_world, (p.x, p.y, p.z));
        }
    }
    if lamps
        .iter()
        .map(|p| (p.x, p.y, p.z))
        .collect::<BTreeSet<_>>()
        .len()
        != source.outputs.len()
        || lamps
            .iter()
            .any(|p| measured_world.get(p.x, p.y, p.z).kind != BlockKind::Lamp)
    {
        return None;
    }
    let implementation = compiled.implementation_netlist(source);
    let source_order = source.combinational_order()?;
    let implementation_order = implementation.combinational_order()?;
    let input_bindings = compiled.physical_rewrite().map_or(&source.inputs[..], |r| {
        r.rewrite_certificate().rewritten_inputs_in_source_order()
    });
    // The Boolean oracle depends only on the address. Physical state and
    // observations still replay every phase with their retained history.
    let states = 1usize << source.inputs.len();
    let mut source_values = vec![None; states];
    let mut implementation_values = vec![None; states];
    let mut sim = Simulator::new(measured_world);
    sim.run_until_stable(2000).ok()?;
    sim.attach_observer(
        drivers
            .iter()
            .chain(&lamps)
            .enumerate()
            .map(|(index, p)| (*p, format!("boundary{index}"))),
    );
    let mut quality = Quality::default();
    let initial = values(
        source,
        &source_order,
        source.inputs.iter().map(|name| (name.as_str(), false)),
    )?;
    let mut previous_outputs: Vec<bool> =
        source.outputs.iter().map(|s| initial[s.as_str()]).collect();
    let mut previous = 0usize;
    for from in 0..(1usize << source.inputs.len()) {
        for to in 0..(1usize << source.inputs.len()) {
            for (phase, address) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                let start = sim.current_tick();
                for (bit, p) in inputs.iter().enumerate() {
                    if caller_ports.is_some_and(|ports| ports.get(&source.inputs[bit]).is_some()) {
                        super::drive_caller_cell(
                            sim.world_mut(), (p.x, p.y, p.z), address & (1 << bit) != 0,
                        );
                        continue;
                    }
                    let mut lever = sim.world().get(p.x, p.y, p.z).clone();
                    lever.lit = address & (1 << bit) != 0;
                    sim.world_mut().set(p.x, p.y, p.z, lever);
                }
                let ticks = sim.run_until_stable(2000).ok()?;
                if source_values[address].is_none() {
                    source_values[address] = Some(values(
                        source,
                        &source_order,
                        source
                            .inputs
                            .iter()
                            .enumerate()
                            .map(|(bit, name)| (name.as_str(), address & (1 << bit) != 0)),
                    )?);
                }
                let expected = source_values[address].as_ref()?;
                if implementation_values[address].is_none() {
                    implementation_values[address] = Some(values(
                        implementation,
                        &implementation_order,
                        input_bindings
                            .iter()
                            .enumerate()
                            .map(|(bit, name)| (name.as_str(), address & (1 << bit) != 0)),
                    )?);
                }
                let implemented = implementation_values[address].as_ref()?;
                for gate in &implementation.gates {
                    let p = position(*compiled.gate_output_positions.get(&gate.output)?);
                    if signal(sim.world(), p)? != implemented[gate.output.as_str()] {
                        return None;
                    }
                }
                let metrics = if phase == 0 {
                    &mut quality.setup
                } else {
                    &mut quality.target
                };
                metrics[0] = metrics[0].max(ticks);
                metrics[1] += ticks;
                for (index, p) in drivers.iter().chain(&lamps).enumerate() {
                    let bit = index % source.outputs.len();
                    let target = expected[source.outputs[bit].as_str()];
                    if signal(sim.world(), *p)? != target {
                        return None;
                    }
                    let label = format!("boundary{index}");
                    let events: Vec<_> = sim
                        .observations()
                        .iter()
                        .filter(|e| e.label == label)
                        .collect();
                    if events
                        .iter()
                        .any(|e| e.tick < start || e.tick > start + ticks)
                        || events.last().is_some_and(|e| e.value != target)
                        || events.is_empty() && previous_outputs[bit] != target
                    {
                        return None;
                    }
                    let offset = if index < source.outputs.len() { 2 } else { 7 };
                    metrics[offset] += u64::from(events.len() > 1);
                    metrics[offset + 1] +=
                        u64::from(events.len() > 1 && (previous ^ address).count_ones() == 1);
                    metrics[offset + 2] =
                        metrics[offset + 2].max(events.last().map_or(0, |e| e.tick - start));
                    for pair in events.windows(2) {
                        if pair[0].value != target {
                            let width = pair[1].tick - pair[0].tick;
                            metrics[offset + 3] += width;
                            metrics[offset + 4] = metrics[offset + 4].max(width);
                        }
                    }
                }
                if upper.is_some_and(|limit| !quality.dominates(limit))
                    || !record(BoundaryPhase {
                    from,
                    to,
                    phase,
                    address,
                    ticks,
                    start,
                    expected: source
                        .outputs
                        .iter()
                        .map(|s| expected[s.as_str()])
                        .collect(),
                    driver: drivers
                        .iter()
                        .map(|p| signal(sim.world(), *p).expect("checked driver"))
                        .collect(),
                    lamp: lamps
                        .iter()
                        .map(|p| signal(sim.world(), *p).expect("checked lamp"))
                        .collect(),
                    events: sim.observations(),
                }) {
                    return None;
                }
                previous_outputs = source
                    .outputs
                    .iter()
                    .map(|s| expected[s.as_str()])
                    .collect();
                previous = address;
            }
        }
    }
    Some(quality)
}

/// Generate a strictly certified draft with only flat interior refreshes.
/// This does not select it by quality; callers must compare continuous source
/// waveforms before promoting the candidate to a default policy.
pub fn compile_flat_refresh_candidate(source: &Netlist) -> Result<CompiledCircuit, CompileError> {
    let raw = super::compile_without_rewrite(source)?;
    flat_refresh_from_compiled(source, &raw).map(|(c, _)| c)
}

fn flat_refresh_from_compiled(
    source: &Netlist,
    raw: &CompiledCircuit,
) -> Result<(CompiledCircuit, usize), CompileError> {
    let mut candidate = raw.physical_candidate().cloned().ok_or_else(|| {
        CompileError::CandidateMetadataViolation {
            item: "flat refresh candidate".into(),
            reason: "a complete combinational manifest is required".into(),
        }
    })?;
    let moved = super::fragment_synth::refresh::flatten_refreshes(&mut candidate);
    let rewrite = realise_physical_rewrite_with_size(
        source,
        source,
        &candidate,
        &super::topology::Library::default_library(),
        &source.inputs,
        &source.outputs,
        1_000_000,
        Some(raw.world.size()),
    )
    .map_err(|error| CompileError::CandidateMetadataViolation {
        item: "flat refresh candidate".into(),
        reason: format!("{error:?}"),
    })?;
    Ok((CompiledCircuit::from_certified_rewrite(rewrite), moved))
}

fn certify_backup(source: &Netlist, compiled: &CompiledCircuit) -> Option<CompiledCircuit> {
    let candidate = compiled.physical_candidate()?;
    let rewrite = realise_physical_rewrite_with_size(
        source,
        source,
        candidate,
        &super::topology::Library::default_library(),
        &source.inputs,
        &source.outputs,
        1_000_000,
        Some(compiled.world.size()),
    )
    .ok()?;
    let sealed = CompiledCircuit::from_certified_rewrite(rewrite);
    if sealed.world.size() != compiled.world.size()
        || sealed
            .world
            .cells()
            .iter()
            .enumerate()
            .any(|(flat, &index)| {
                let (x, y, z) = sealed.world.decode(flat);
                sealed.world.palette().get(index) != Some(compiled.world.get(x, y, z))
            })
    {
        return None;
    }
    Some(sealed)
}

#[derive(Clone, PartialEq, Eq)]
struct PhaseQuality {
    settlement: u64,
    arrivals: Vec<u64>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RefreshQuality {
    core: Quality,
    // setup driver/lamp, then target driver/lamp glitch-transition counts.
    groups: [u64; 4],
    phases: Vec<PhaseQuality>,
}
impl RefreshQuality {
    pub(crate) fn dominates(&self, other: &Self) -> bool {
        self.core.dominates(&other.core)
            && self.groups.iter().zip(other.groups).all(|(a, b)| *a <= b)
            && self.phases.len() == other.phases.len()
            && self.phases.iter().zip(&other.phases).all(|(a, b)| {
                a.settlement <= b.settlement
                    && a.arrivals.len() == b.arrivals.len()
                    && a.arrivals.iter().zip(&b.arrivals).all(|(a, b)| a <= b)
            })
    }
}
fn refresh_quality(
    source: &Netlist,
    compiled: &CompiledCircuit,
    require_certificate: bool,
) -> Option<RefreshQuality> {
    quality_with_ports(source, compiled, require_certificate, None, None)
}

/// Measure an already root-certified recursive world with its exact caller ports.
/// Fixtures are installed only in the measurement copy, never in the product.
pub(crate) fn recursive_quality(
    source: &Netlist,
    compiled: &CompiledCircuit,
    caller_ports: Option<&super::planner::PortPlacements>,
) -> Option<RefreshQuality> {
    quality_with_ports(source, compiled, false, caller_ports, None)
}

fn quality_with_ports(
    source: &Netlist,
    compiled: &CompiledCircuit,
    require_certificate: bool,
    caller_ports: Option<&super::planner::PortPlacements>,
    upper: Option<&RefreshQuality>,
) -> Option<RefreshQuality> {
    let mut groups = [0; 4];
    let mut phases = Vec::new();
    let core = qualify_boundary_ports_while(source, compiled, require_certificate, caller_ports, upper.map(|limit| &limit.core), |phase| {
        for channel in 0..2 {
            if (0..source.outputs.len()).any(|bit| {
                let label = format!("boundary{}", channel * source.outputs.len() + bit);
                phase.events.iter().filter(|e| e.label == label).count() > 1
            }) {
                groups[phase.phase * 2 + channel] += 1;
            }
        }
        let measured = PhaseQuality {
            settlement: phase.ticks,
            arrivals: (0..2 * source.outputs.len())
                .map(|index| {
                    let label = format!("boundary{index}");
                    phase
                        .events
                        .iter()
                        .filter(|event| event.label == label)
                        .map(|event| event.tick - phase.start)
                        .max()
                        .unwrap_or(0)
                })
                .collect(),
        };
        if let Some(limit) = upper {
            let Some(previous) = limit.phases.get(phases.len()) else { return false; };
            if measured.settlement > previous.settlement
                || measured.arrivals.len() != previous.arrivals.len()
                || measured.arrivals.iter().zip(&previous.arrivals).any(|(a, b)| a > b)
                || groups.iter().zip(limit.groups).any(|(a, b)| *a > b)
            { return false; }
        }
        phases.push(measured);
        true
    })?;
    let measured = RefreshQuality { core, groups, phases };
    upper.is_none_or(|limit| measured.dominates(limit)).then_some(measured)
}
fn improve_refreshes(
    source: &Netlist,
    mut best: CompiledCircuit,
    mut quality: RefreshQuality,
    budget: usize,
) -> (CompiledCircuit, RefreshQuality) {
    use super::fragment_synth::refresh::{removable_flat_refreshes, remove_flat_refresh};
    if budget == 0 {
        return (best, quality);
    }
    let sites = removable_flat_refreshes(best.physical_rewrite().unwrap().implemented_candidate());
    for (route, at) in sites.into_iter().take(budget) {
        let mut candidate = best
            .physical_rewrite()
            .unwrap()
            .implemented_candidate()
            .clone();
        if !remove_flat_refresh(&mut candidate, route, at) {
            continue;
        }
        let Ok(rewrite) = realise_physical_rewrite_with_size(
            source,
            source,
            &candidate,
            &super::topology::Library::default_library(),
            &source.inputs,
            &source.outputs,
            1_000_000,
            Some(best.world.size()),
        ) else {
            continue;
        };
        let circuit = CompiledCircuit::from_certified_rewrite(rewrite);
        let Some(measured) = quality_with_ports(source, &circuit, true, None, Some(&quality)) else {
            continue;
        };
        if measured.dominates(&quality) && measured != quality {
            best = circuit;
            quality = measured;
        }
    }
    (best, quality)
}

/// Compare the bounded player family and certify structural refresh corrections.
/// Selected rewrites must certify and preserve the complete source boundary.
/// The larger-boundary control is the independent original compilation.
pub fn compile_selected(source: &Netlist) -> Result<CompiledCircuit, CompileError> {
    compile_selected_with_refresh_budget(source, 256)
}

/// Bounded, deterministic refresh proposals outside the fixed player family.
/// Zero still checks the seed; the budget limits attempted structural reductions.
pub fn compile_selected_with_refresh_budget(
    source: &Netlist,
    budget: usize,
) -> Result<CompiledCircuit, CompileError> {
    #[cfg(test)]
    LITERAL_PROPOSALS.with(|count| count.set(0));
    // Six-input matrices use the structurally checked generator directly.
    // Retained-history quality comparison is an independent experiment, not
    // a prerequisite repeated by every API/CLI invocation. This path makes
    // no claim of exhaustive transition-quality dominance over raw routing.
    if source.inputs.len() == 6
        && (1..=16).contains(&source.outputs.len())
        && !source.gates.iter().any(|gate| gate.kind.is_sequential())
    {
        super::checked_rewrite_topological_order(source)?;
        let started = std::time::Instant::now();
        let rewrite = compile_balanced_torch_matrix(source, 1_000_000)
            .map_err(|error| CompileError::CandidateMetadataViolation {
                item: "six-input structural matrix".into(),
                reason: format!("{error:?}"),
            })?;
        if std::env::var_os("REDA_TRACE_MATRIX").is_some() {
            eprintln!("REDA_MATRIX generation_and_physical_checks elapsed_ms={}",
                started.elapsed().as_millis());
        }
        return Ok(CompiledCircuit::from_certified_rewrite(rewrite));
    }
    let occurrence_boundary = (4..=5).contains(&source.inputs.len())
        && (1..=16).contains(&source.outputs.len())
        && !source.gates.iter().any(|gate| gate.kind.is_sequential())
        && super::fragment_synth::rewrite::needs_occurrence_preserving_boundary(source);
    if occurrence_boundary {
        super::checked_rewrite_topological_order(source)?;
    } else {
        super::checked_topological_order(source)?;
    }
    // This is a baseline boundary obligation, not an optional search proposal.
    // Raw name-keyed outputs cannot represent these public port occurrences.
    if occurrence_boundary {
        let rewrite = compile_balanced_torch_matrix(source, 1_000_000)
            .map_err(|error| CompileError::CandidateMetadataViolation {
                item: "source output boundary".into(), reason: format!("{error:?}"),
            })?;
        let compiled = CompiledCircuit::from_certified_rewrite(rewrite);
        qualify_boundary(source, &compiled)
            .ok_or_else(|| CompileError::CandidateMetadataViolation {
                item: "source output boundary".into(),
                reason: "occurrence-preserving boundary failed complete retained-history qualification".into(),
            })?;
        return Ok(select_literal_candidate(source, compiled, budget));
    }
    if source.inputs.len() != 2
        || source.outputs.len() != 3
        || source.gates.iter().any(|g| g.kind.is_sequential())
    {
        let raw = super::compile_without_rewrite(source)?;
        if !(1..=6).contains(&source.inputs.len())
            || source.outputs.is_empty()
            || source.gates.iter().any(|g| g.kind.is_sequential())
        {
            return Ok(raw);
        }
        let Some(mut quality) = refresh_quality(source, &raw, false) else {
            return Ok(if budget > 0 { strict_refresh_selection(source, raw, budget) } else { raw });
        };
        let mut best = raw;
        let flat_seed = flat_refresh_from_compiled(source, &best);
        // Compare complete retained histories, using only structural boundary
        // recognition. A rejected method leaves the already qualified best.
        // The matrix is a structural candidate, not the flat-refresh seed.
        // Establish a qualified global bound before exploring local reductions.
        // Zero retains the existing flat seed behavior.
        if budget > 0 && (4..=5).contains(&source.inputs.len()) && (1..=16).contains(&source.outputs.len()) {
            if let Ok(rewrite) = compile_balanced_torch_matrix(source, 1_000_000) {
                let matrix = CompiledCircuit::from_certified_rewrite(rewrite);
                if let Some(measured) = quality_with_ports(source, &matrix, true, None, Some(&quality)) {
                    if measured.dominates(&quality) && measured != quality {
                        best = matrix;
                        quality = measured;
                    }
                }
            }
        }
        if let Ok((flat, moved)) = flat_seed {
            if let Some(candidate) = quality_with_ports(source, &flat, true, None, Some(&quality)) {
                if candidate.dominates(&quality) {
                    let (improved, measured) =
                        improve_refreshes(source, flat, candidate.clone(), 0);
                    if moved > 0 || measured != candidate {
                        best = improved;
                    }
                }
            }
        }
        return Ok(if budget > 0 { strict_refresh_selection(source, best, budget) } else { best });
    }
    let player = compile_player_torch_row(source, 1_000_000)
        .ok()
        .map(CompiledCircuit::from_certified_rewrite)
        .and_then(|c| qualify(source, &c).map(|q| (c, q)));
    let raw_backup = super::compile_without_rewrite(source);
    let backup = raw_backup
        .as_ref()
        .ok()
        .and_then(|c| certify_backup(source, c))
        .and_then(|c| qualify(source, &c).map(|q| (c, q)));
    match (player, backup) {
        (Some((player, p)), Some((backup, b))) => Ok(if b.dominates(&p) && !p.dominates(&b) {
            backup
        } else {
            player
        }),
        (Some((player, _)), None) => Ok(player),
        (None, Some((backup, _))) => Ok(backup),
        (None, None) => Err(raw_backup
            .err()
            .unwrap_or(CompileError::CandidateMetadataViolation {
                item: "quality selection".into(),
                reason: "no fully certified and truth-correct candidate".into(),
            })),
    }
}


// Local edits preserve the actual implementation graph; compare each channel
// and phase separately so aggregate improvements cannot mask regressions.
#[derive(Clone, PartialEq, Eq)]
struct StrictPhase { ticks: u64, work: u64, cells: u64, channels: Vec<[u64; 5]> }
impl StrictPhase {
    fn dominates(&self, old: &Self) -> bool {
        self.ticks <= old.ticks && self.work <= old.work && self.cells <= old.cells
            && self.channels.len() == old.channels.len()
            && self.channels.iter().zip(&old.channels).all(|(a,b)| a.iter().zip(b).all(|(a,b)| a <= b))
    }
}
fn strict_profile(source: &Netlist, circuit: &CompiledCircuit, upper: Option<&[StrictPhase]>) -> Option<Vec<StrictPhase>> {
    circuit.validate_rewrite_view(source).ok()?;
    let implementation = circuit.implementation_netlist(source);
    let order = implementation.combinational_order()?;
    let source_order = source.combinational_order()?;
    let bindings = circuit.physical_rewrite().map_or(&source.inputs[..], |r| r.rewrite_certificate().rewritten_inputs_in_source_order());
    let drivers = if let Some(r) = circuit.physical_rewrite() { r.source_output_driver_positions().to_vec() } else {
        source.outputs.iter().map(|s| circuit.gate_output_positions.get(s).copied()).collect::<Option<Vec<_>>>()?
    };
    if bindings.len() != source.inputs.len() || drivers.len() != source.outputs.len() { return None; }
    let lamps: Vec<_> = circuit.output_occurrences().iter().map(|(_, p)| *p).collect();
    if lamps.len() != source.outputs.len() { return None; }
    // Repeated public positions are accepted only for the same source signal.
    for i in 0..lamps.len() { for j in 0..i {
        if lamps[i] == lamps[j] && source.outputs[i] != source.outputs[j] { return None; }
    }}
    let mut sites: Vec<_> = drivers.iter().chain(&lamps).copied().collect();
    sites.extend(implementation.gates.iter().map(|g| circuit.gate_output_positions.get(&g.output).copied()).collect::<Option<Vec<_>>>()?);
    let channel_count = sites.len();
    let mut occupied = Vec::new();
    for (flat, &index) in circuit.world.cells().iter().enumerate() {
        if circuit.world.palette().get(index)?.kind != BlockKind::Air { occupied.push(circuit.world.decode(flat)); }
    }
    let mut watches: Vec<_> = sites.iter().enumerate().map(|(i, &p)| (position(p), format!("channel{i}"))).collect();
    watches.extend(occupied.iter().enumerate().map(|(i, &p)| (position(p), format!("cell{i}"))));
    let mut sim = Simulator::new(circuit.world.clone());
    sim.run_until_stable_bounded(2000, 1_000_000).ok()?;
    sim.attach_observer(watches);
    let mut result = Vec::new();
    let mut previous = 0usize;
    let states = 1usize << source.inputs.len();
    let mut targets = Vec::new();
    for address in 0..states {
        let truth = values(source, &source_order, source.inputs.iter().enumerate().map(|(b, s)| (s.as_str(), address & (1 << b) != 0)))?;
        let implemented = values(implementation, &order, bindings.iter().enumerate().map(|(b,s)| (s.as_str(), address & (1 << b) != 0)))?;
        let public: Vec<_> = source.outputs.iter().map(|s| truth[s.as_str()]).collect();
        targets.push(public.iter().chain(&public).copied().chain(implementation.gates.iter().map(|g| implemented[g.output.as_str()])).collect::<Vec<_>>());
    }
    for from in 0..states { for to in 0..states { for address in [from,to] {
        sim.reset_observer();
        let start = sim.current_tick(); let work = sim.work_done();
        for (bit, name) in source.inputs.iter().enumerate() {
            let at = *circuit.input_positions.get(name)?;
            let mut lever = sim.world().get(at.0,at.1,at.2).clone();
            if lever.kind != BlockKind::Lever { return None; }
            lever.lit = address & (1 << bit) != 0;
            sim.world_mut().set(at.0,at.1,at.2,lever);
        }
        let ticks = sim.run_until_stable_bounded(2000, 1_000_000).ok()?;
        let mut channels = vec![[0;5];channel_count];
        let mut events = vec![Vec::new();channel_count]; let mut cells = 0;
        for e in sim.observations() {
            if let Some(i) = e.label.strip_prefix("channel") { events[i.parse::<usize>().ok()?].push(e); }
            else { cells += 1; }
        }
        for (i, es) in events.iter().enumerate() {
            let target = targets[address][i];
            if signal(sim.world(), position(sites[i]))? != target || es.last().is_some_and(|e| e.value != target) || (es.is_empty() && targets[previous][i] != target) { return None; }
            channels[i] = [u64::from(es.len()>1),u64::from(es.len()>1 && (previous ^ address).count_ones()==1),es.last().map_or(0,|e|e.tick-start),0,0];
            for pair in es.windows(2) { if pair[0].value != target { let width=pair[1].tick-pair[0].tick; channels[i][3]+=width; channels[i][4]=channels[i][4].max(width); }}
        }
        let phase = StrictPhase {ticks,work:sim.work_done()-work,cells,channels};
        if let Some(bound) = upper {
            let old = bound.get(result.len())?;
            if !phase.dominates(old) { return None; }
        }
        result.push(phase); previous=address;
    }}}
    Some(result)
}
fn strict_refresh_selection(source: &Netlist, mut best: CompiledCircuit, budget: usize) -> CompiledCircuit {
    use super::fragment_synth::refresh::{removable_flat_refreshes, remove_flat_refresh};
    let Some(mut quality)=strict_profile(source,&best,None) else { return best; };
    let Some(initial)=best.physical_candidate() else { return best; };
    let mut sites=removable_flat_refreshes(initial);
    // Adjacent terminal relays first: their terminal isolation remains intact.
    sites.sort_by_key(|(id,at)| {
        let adjacent=super::fragment_synth::refresh::adjacent_terminal_refresh(initial,*id,*at);
        (!adjacent,*id,*at)
    });
    for (id,at) in sites.into_iter().take(budget) {
        let Some(mut candidate)=best.physical_candidate().cloned() else { break; };
        if !remove_flat_refresh(&mut candidate,id,at) { continue; }
        let Ok(rewrite)=realise_physical_rewrite_with_size(source,source,&candidate,&super::topology::Library::default_library(),&source.inputs,&source.outputs,1_000_000,Some(best.world.size())) else {continue;};
        let proposed=CompiledCircuit::from_certified_rewrite(rewrite);
        if proposed.implementation_netlist(source) != best.implementation_netlist(source) { continue; }
        if let Some(measured)=strict_profile(source,&proposed,Some(&quality)) {
            if measured != quality {best=proposed;quality=measured;}
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_refresh_guard_rejects_missing_channels_and_private_work_regressions() {
        let control = StrictPhase { ticks: 10, work: 20, cells: 30, channels: vec![[0;5];2] };
        let mut candidate = control.clone();
        candidate.channels.pop();
        assert!(!candidate.dominates(&control));
        candidate = control.clone(); candidate.work += 1;
        assert!(!candidate.dominates(&control));
        candidate = control.clone(); candidate.cells += 1;
        assert!(!candidate.dominates(&control));
        candidate = control.clone(); candidate.channels[1][3] = 1;
        assert!(!candidate.dominates(&control));
        candidate = control.clone(); candidate.ticks -= 1;
        assert!(candidate.dominates(&control));
    }
    use crate::compile::{topology::GateKind, Gate};
    fn source() -> Netlist {
        Netlist {
            inputs: vec!["left".into(), "right".into()],
            outputs: vec!["zero".into(), "negative".into(), "mixed".into()],
            gates: vec![
                Gate::nor("zero", &["left", "right"]),
                Gate::nor("negative", &["left"]),
                Gate::nor("mixed", &["negative", "right"]),
            ],
        }
    }
    #[test]
    fn fewer_individual_glitches_cannot_hide_more_glitch_transitions() {
        let mut control = RefreshQuality {
            core: Quality::default(),
            groups: [0, 0, 1, 1],
            phases: vec![PhaseQuality {
                settlement: 10,
                arrivals: vec![4, 8],
            }],
        };
        control.core.target[1] = 10;
        let mut candidate = control.clone();
        candidate.core.target[1] = 9;
        candidate.groups[3] = 2;
        assert!(candidate.core.dominates(&control.core));
        assert!(!candidate.dominates(&control));
        candidate.groups[3] = 1;
        assert!(candidate.dominates(&control));
        candidate.phases[0].arrivals[0] = 5;
        assert!(!candidate.dominates(&control));
        candidate.phases[0].arrivals[0] = 4;
        candidate.phases[0].settlement = 11;
        assert!(!candidate.dominates(&control));
    }
    #[test]
    fn bounded_wave_replay_keeps_complete_success_and_rejects_real_counterexamples() {
        let source = source();
        let compiled = CompiledCircuit::from_certified_rewrite(
            compile_player_torch_row(&source, 1_000_000).unwrap(),
        );
        let complete = refresh_quality(&source, &compiled, true).unwrap();
        assert_eq!(complete.phases.len(), 32);
        let bounded = quality_with_ports(&source, &compiled, true, None, Some(&complete)).unwrap();
        assert!(bounded == complete, "successful bounds retain every phase and metric");

        let mut arrival_limit = complete.clone();
        let phase = arrival_limit.phases.iter_mut()
            .find(|phase| phase.arrivals.iter().any(|&tick| tick > 0)).unwrap();
        *phase.arrivals.iter_mut().find(|tick| **tick > 0).unwrap() = 0;
        assert!(!complete.dominates(&arrival_limit));
        assert!(quality_with_ports(&source, &compiled, true, None, Some(&arrival_limit)).is_none());

        let mut settlement_limit = complete.clone();
        settlement_limit.phases.iter_mut().find(|p| p.settlement > 0).unwrap().settlement = 0;
        assert!(!complete.dominates(&settlement_limit));
        assert!(quality_with_ports(&source, &compiled, true, None, Some(&settlement_limit)).is_none());

        let mut observed = 0;
        assert!(qualify_boundary_ports_while(
            &source, &compiled, true, None, Some(&Quality::default()),
            |_| { observed += 1; true },
        ).is_none());
        assert!(observed < complete.phases.len(), "a proven regression stops rejected replay");
        let mut callbacks = 0;
        assert!(qualify_boundary_ports_while(&source, &compiled, true, None, None,
            |_| { callbacks += 1; false },
        ).is_none());
        assert_eq!(callbacks, 1, "incomplete replay never returns qualification");
    }

    #[test]
    fn bounded_replay_preserves_wide_repeated_public_channels() {
        let source = Netlist {
            inputs: (0..4).map(|bit| format!("a{bit}")).collect(),
            outputs: vec!["out".into(); 16],
            gates: vec![Gate::nor("out", &["a0", "a1", "a2", "a3"])],
        };
        let compiled = CompiledCircuit::from_certified_rewrite(
            compile_balanced_torch_matrix(&source, 1_000_000).unwrap(),
        );
        let complete = refresh_quality(&source, &compiled, true).unwrap();
        assert_eq!(complete.phases.len(), 512);
        assert!(complete.phases.iter().all(|phase| phase.arrivals.len() == 32));
        let bounded = quality_with_ports(&source, &compiled, true, None, Some(&complete)).unwrap();
        assert!(bounded == complete);
        let mut limit = complete.clone();
        let phase = limit.phases.iter_mut().find(|phase| phase.arrivals[31] > 0).unwrap();
        phase.arrivals[31] = 0;
        assert!(!complete.dominates(&limit));
        assert!(quality_with_ports(&source, &compiled, true, None, Some(&limit)).is_none());
    }

    #[test]
    fn unified_backup_retains_an_exact_certifiable_manifest() {
        let source = source();
        let raw = super::super::compile_without_rewrite(&source).unwrap();
        assert_eq!(raw.planner_kind(), super::super::PlannerKind::Unified3d);
        let sealed = certify_backup(&source, &raw)
            .expect("planner manifest must certify with exact original bounds");
        assert!(qualify(&source, &sealed).is_some());
        assert_eq!(sealed.input_positions, raw.input_positions);
        assert_eq!(sealed.output_positions, raw.output_positions);
        assert_eq!(sealed.gate_output_positions, raw.gate_output_positions);
        assert!(realise_physical_rewrite_with_size(
            &source,
            &source,
            raw.physical_candidate().unwrap(),
            &super::super::topology::Library::default_library(),
            &source.inputs,
            &source.outputs,
            1_000_000,
            Some((1, 1, 1))
        )
        .is_err());
    }
    #[test]
    fn modified_sealed_views_never_qualify() {
        let source = source();
        let make = || {
            CompiledCircuit::from_certified_rewrite(
                compile_player_torch_row(&source, 1_000_000).unwrap(),
            )
        };
        let mut bad = make();
        bad.output_positions.clear();
        assert!(qualify(&source, &bad).is_none());
        let mut bad = make();
        let p = position(*bad.gate_output_positions.values().next().unwrap());
        bad.world.set(
            p.x,
            p.y,
            p.z,
            crate::redstone::world::block::BlockState::air(),
        );
        assert!(qualify(&source, &bad).is_none());
        let mut changed = source.clone();
        changed.gates[0].inputs.swap(0, 1);
        assert!(qualify(&changed, &make()).is_none());
    }
    #[test]
    fn selector_preserves_realisability_cycle_and_driver_errors() {
        let mut invalid = source();
        invalid.gates[0].kind = GateKind::And;
        let mut cyclic = source();
        cyclic.gates[0].inputs = vec!["mixed".into()];
        let mut undriven = source();
        undriven.gates[0].inputs = vec!["missing".into()];
        let mut undriven_output = source();
        undriven_output.outputs = vec!["left".into(); 3];
        for source in [invalid, cyclic, undriven, undriven_output] {
            assert_eq!(
                compile_selected(&source).err(),
                super::super::compile_without_rewrite(&source).err()
            );
        }
    }
}

fn literal_site_truths(source: &Netlist, circuit: &CompiledCircuit) -> Option<Vec<Vec<bool>>> {
    let implemented = circuit.implementation_netlist(source);
    let order = implemented.combinational_order()?;
    let names = circuit
        .physical_rewrite()?
        .rewrite_certificate()
        .rewritten_inputs_in_source_order();
    let mut tables = vec![Vec::new(); implemented.gates.len()];
    for address in 0..1usize << source.inputs.len() {
        let truth = values(
            implemented,
            &order,
            names
                .iter()
                .enumerate()
                .map(|(bit, name)| (name.as_str(), address & (1 << bit) != 0)),
        )?;
        for (index, gate) in implemented.gates.iter().enumerate() {
            tables[index].push(truth[gate.output.as_str()]);
        }
    }
    Some(tables)
}

fn literal_histories_dominate(
    outputs: usize,
    states: usize,
    old_truth: &[Vec<bool>],
    new_truth: &[Vec<bool>],
    old: &[StrictPhase],
    new: &[StrictPhase],
) -> bool {
    let Some(offset) = outputs.checked_mul(2) else {
        return false;
    };
    let Some(phases) = states
        .checked_mul(states)
        .and_then(|count| count.checked_mul(2))
    else {
        return false;
    };
    let Some(old_channels) = offset.checked_add(old_truth.len()) else {
        return false;
    };
    let Some(new_channels) = offset.checked_add(new_truth.len()) else {
        return false;
    };
    if states == 0
        || outputs == 0
        || old.len() != phases
        || new.len() != phases
        || old.iter().any(|phase| phase.channels.len() != old_channels)
        || new.iter().any(|phase| phase.channels.len() != new_channels)
        || old_truth
            .iter()
            .chain(new_truth)
            .any(|truth| truth.len() != states)
    {
        return false;
    }
    let matches: Vec<Vec<usize>> = new_truth
        .iter()
        .map(|truth| {
            old_truth
                .iter()
                .enumerate()
                .filter(|(_, old)| *old == truth)
                .map(|(index, _)| index)
                .collect()
        })
        .collect();
    if matches.iter().any(Vec::is_empty) {
        return false;
    }
    let private = |phase: &StrictPhase| {
        let channels = &phase.channels[offset..];
        [
            channels.iter().map(|channel| channel[0]).sum::<u64>(),
            channels.iter().map(|channel| channel[1]).sum::<u64>(),
            channels.iter().map(|channel| channel[2]).max().unwrap_or(0),
            channels.iter().map(|channel| channel[3]).sum::<u64>(),
            channels.iter().map(|channel| channel[4]).max().unwrap_or(0),
        ]
    };
    old.iter().zip(new).all(|(old, new)| {
        new.ticks <= old.ticks
            && new.work <= old.work
            && new.cells <= old.cells
            && old.channels[..offset]
                .iter()
                .zip(&new.channels[..offset])
                .all(|(old, new)| old.iter().zip(new).all(|(old, new)| new <= old))
            && private(old)
                .iter()
                .zip(private(new))
                .all(|(old, new)| new <= *old)
            && matches.iter().enumerate().all(|(gate, old_gates)| {
                old_gates.iter().all(|&prior| {
                    new.channels[offset + gate]
                        .iter()
                        .zip(old.channels[offset + prior])
                        .all(|(new, old)| *new <= old)
                })
            })
    }) && old.iter().zip(new).any(|(old, new)| {
        new.ticks < old.ticks
            || new.work < old.work
            || new.cells < old.cells
            || private(new) != private(old)
    })
}

#[cfg(test)]
thread_local! { static LITERAL_PROPOSALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

fn select_literal_candidate(
    source: &Netlist,
    baseline: CompiledCircuit,
    budget: usize,
) -> CompiledCircuit {
    if budget == 0 {
        return baseline;
    }
    let Ok(rewrite) = super::fragment_synth::torch_row::compile_direct_literals(source, 1_000_000)
    else {
        return baseline;
    };
    #[cfg(test)]
    LITERAL_PROPOSALS.with(|count| count.set(count.get() + 1));
    let candidate = CompiledCircuit::from_certified_rewrite(rewrite);
    let Some(old_truth) = literal_site_truths(source, &baseline) else {
        return baseline;
    };
    let Some(new_truth) = literal_site_truths(source, &candidate) else {
        return baseline;
    };
    let Some(old) = strict_profile(source, &baseline, None) else {
        return baseline;
    };
    let Some(new) = strict_profile(source, &candidate, None) else {
        return baseline;
    };
    if literal_histories_dominate(
        source.outputs.len(),
        1usize << source.inputs.len(),
        &old_truth,
        &new_truth,
        &old,
        &new,
    ) {
        candidate
    } else {
        baseline
    }
}

#[cfg(test)]
mod literal_comparator_tests {
    use super::*;
    fn fixture() -> (Vec<Vec<bool>>, Vec<StrictPhase>, Vec<StrictPhase>) {
        let phase = StrictPhase {
            ticks: 10,
            work: 20,
            cells: 30,
            channels: vec![[0; 5]; 17],
        };
        let old = vec![phase; 512];
        let mut new = old.clone();
        new[0].ticks -= 1;
        (
            vec![(0..16).map(|address| address & 1 != 0).collect()],
            old,
            new,
        )
    }
    #[test]
    fn exact_phase_and_public_private_channel_counts_are_required() {
        let (truth, old, new) = fixture();
        assert!(literal_histories_dominate(
            8, 16, &truth, &truth, &old, &new
        ));
        for private in [false, true] {
            for extra in [false, true] {
                let mut malformed = new.clone();
                let index = if private { 16 } else { 0 };
                if extra {
                    malformed[0].channels.insert(index, [0; 5]);
                } else {
                    malformed[0].channels.remove(index);
                }
                assert!(!literal_histories_dominate(
                    8, 16, &truth, &truth, &old, &malformed
                ));
                let mut malformed = old.clone();
                if extra {
                    malformed[0].channels.insert(index, [0; 5]);
                } else {
                    malformed[0].channels.remove(index);
                }
                assert!(!literal_histories_dominate(
                    8, 16, &truth, &truth, &malformed, &new
                ));
            }
        }
        let mut short = new.clone();
        short.pop();
        assert!(!literal_histories_dominate(
            8, 16, &truth, &truth, &old, &short
        ));
        let mut extra = old.clone();
        extra.push(old[0].clone());
        assert!(!literal_histories_dominate(
            8, 16, &truth, &truth, &extra, &new
        ));
    }
    #[test]
    fn private_metric_regression_and_unmatched_or_incomplete_truth_fail_closed() {
        let (truth, old, new) = fixture();
        for metric in 0..5 {
            let mut bad = new.clone();
            bad[0].channels[16][metric] = 1;
            assert!(!literal_histories_dominate(
                8, 16, &truth, &truth, &old, &bad
            ));
        }
        let mut unmatched = truth.clone();
        unmatched[0][0] = !unmatched[0][0];
        assert!(!literal_histories_dominate(
            8, 16, &truth, &unmatched, &old, &new
        ));
        let mut short = truth.clone();
        short[0].pop();
        assert!(!literal_histories_dominate(
            8, 16, &truth, &short, &old, &new
        ));
    }
}

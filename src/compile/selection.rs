//! Retained-history quality selection: a two-input/three-output player family
//! and structural flat refresh candidates with up to six inputs.
//! No function IDs, source names, block counts or volume select a candidate.
use super::fragment_synth::{
    rewrite::realise_physical_rewrite_with_size, torch_row::compile_player_torch_row,
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
    mut record: impl FnMut(BoundaryPhase<'_>),
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
    if inputs
        .iter()
        .any(|p| compiled.world.get(p.x, p.y, p.z).kind != BlockKind::Lever)
    {
        return None;
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
    if lamps
        .iter()
        .map(|p| (p.x, p.y, p.z))
        .collect::<BTreeSet<_>>()
        .len()
        != source.outputs.len()
        || lamps
            .iter()
            .any(|p| compiled.world.get(p.x, p.y, p.z).kind != BlockKind::Lamp)
    {
        return None;
    }
    let implementation = compiled.implementation_netlist(source);
    let source_order = source.combinational_order()?;
    let implementation_order = implementation.combinational_order()?;
    let input_bindings = compiled.physical_rewrite().map_or(&source.inputs[..], |r| {
        r.rewrite_certificate().rewritten_inputs_in_source_order()
    });
    let mut sim = Simulator::new(compiled.world.clone());
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
                    let mut lever = sim.world().get(p.x, p.y, p.z).clone();
                    lever.lit = address & (1 << bit) != 0;
                    sim.world_mut().set(p.x, p.y, p.z, lever);
                }
                let ticks = sim.run_until_stable(2000).ok()?;
                let expected = values(
                    source,
                    &source_order,
                    source
                        .inputs
                        .iter()
                        .enumerate()
                        .map(|(bit, name)| (name.as_str(), address & (1 << bit) != 0)),
                )?;
                let implemented = values(
                    implementation,
                    &implementation_order,
                    input_bindings
                        .iter()
                        .enumerate()
                        .map(|(bit, name)| (name.as_str(), address & (1 << bit) != 0)),
                )?;
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
                record(BoundaryPhase {
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
                });
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
struct RefreshQuality {
    core: Quality,
    // setup driver/lamp, then target driver/lamp glitch-transition counts.
    groups: [u64; 4],
}
impl RefreshQuality {
    fn dominates(&self, other: &Self) -> bool {
        self.core.dominates(&other.core)
            && self.groups.iter().zip(other.groups).all(|(a, b)| *a <= b)
    }
}
fn refresh_quality(
    source: &Netlist,
    compiled: &CompiledCircuit,
    require_certificate: bool,
) -> Option<RefreshQuality> {
    let mut groups = [0; 4];
    let core = qualify_boundary_impl(source, compiled, require_certificate, |phase| {
        for channel in 0..2 {
            if (0..source.outputs.len()).any(|bit| {
                let label = format!("boundary{}", channel * source.outputs.len() + bit);
                phase.events.iter().filter(|e| e.label == label).count() > 1
            }) {
                groups[phase.phase * 2 + channel] += 1;
            }
        }
    })?;
    Some(RefreshQuality { core, groups })
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
        let Some(measured) = refresh_quality(source, &circuit, true) else {
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
    super::checked_topological_order(source)?;
    if source.inputs.len() != 2
        || source.outputs.len() != 3
        || source.gates.iter().any(|g| g.kind.is_sequential())
    {
        let raw = super::compile_without_rewrite(source)?;
        if (1..=6).contains(&source.inputs.len())
            && !source.outputs.is_empty()
            && !source.gates.iter().any(|g| g.kind.is_sequential())
        {
            if let Ok((flat, moved)) = flat_refresh_from_compiled(source, &raw) {
                if let (Some(control), Some(candidate)) = (
                    refresh_quality(source, &raw, false),
                    refresh_quality(source, &flat, true),
                ) {
                    if candidate.dominates(&control) {
                        let (best, quality) =
                            improve_refreshes(source, flat, candidate.clone(), budget);
                        if moved > 0 || quality != candidate {
                            return Ok(best);
                        }
                    }
                }
            }
        }
        return Ok(raw);
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

#[cfg(test)]
mod tests {
    use super::*;
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
        };
        control.core.target[1] = 10;
        let mut candidate = control.clone();
        candidate.core.target[1] = 9;
        candidate.groups[3] = 2;
        assert!(candidate.core.dominates(&control.core));
        assert!(!candidate.dominates(&control));
        candidate.groups[3] = 1;
        assert!(candidate.dominates(&control));
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

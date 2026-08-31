use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::circuits::{and4, full_adder, seven_segment, verilog};
use crate::compile::fragment_synth::manifest::TransitionManifest;
use crate::compile::geometry::Anchor;
use crate::compile::metrics::{
    canonical_fingerprint, physical_metrics, Fingerprint, PhysicalMetrics,
};
use crate::compile::planner::PortPlacements;
use crate::compile::revisions::{
    cell_library_revision, physical_verifier_revision, simulator_revision,
};
use crate::compile::topology::Library;
use crate::compile::{self, CompiledCircuit, Netlist};
use crate::redstone::simulator::Simulator;
use crate::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use crate::redstone::world::storage::World;

pub const MAX_TRANSITION_GAME_TICKS: u64 = 2_048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkCase {
    pub name: String,
    pub lowered_netlist_hash: Fingerprint,
    pub pin_manifest_hash: Fingerprint,
    pub transition_manifest_hash: Fingerprint,
    pub generated_world_fingerprint: Option<Fingerprint>,
    pub certified: bool,
    pub physical: Option<PhysicalMetrics>,
    pub max_observed_settle_game_ticks_on_manifest: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkBaseline {
    pub baseline_commit: String,
    pub build_profile: String,
    pub cargo_features: Vec<String>,
    pub cell_library_revision: Fingerprint,
    pub simulator_revision: Fingerprint,
    pub verifier_revision: Fingerprint,
    pub cases: Vec<BenchmarkCase>,
}

type ExpectedOutputs = fn(&[bool]) -> Vec<bool>;

pub struct BenchmarkFixture {
    name: String,
    lowered_netlist: Netlist,
    input_ports: Vec<String>,
    output_signals: Vec<String>,
    expected: ExpectedOutputs,
    placements: PortPlacements,
}

impl BenchmarkFixture {
    pub fn new(
        name: impl Into<String>,
        lowered_netlist: Netlist,
        input_ports: Vec<String>,
        output_signals: Vec<String>,
        expected: ExpectedOutputs,
        placements: PortPlacements,
    ) -> Self {
        BenchmarkFixture {
            name: name.into(),
            lowered_netlist,
            input_ports,
            output_signals,
            expected,
            placements,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn lowered_netlist(&self) -> &Netlist {
        &self.lowered_netlist
    }

    pub fn transition_manifest(&self) -> TransitionManifest {
        TransitionManifest::new(self.input_ports.clone())
    }

    fn blank_case(&self) -> BenchmarkCase {
        BenchmarkCase {
            name: self.name.clone(),
            lowered_netlist_hash: canonical_netlist_fingerprint(&self.lowered_netlist),
            pin_manifest_hash: canonical_pin_manifest_fingerprint(&self.placements),
            transition_manifest_hash: self.transition_manifest().fingerprint(),
            generated_world_fingerprint: None,
            certified: false,
            physical: None,
            max_observed_settle_game_ticks_on_manifest: None,
        }
    }
}

pub struct AcceptanceEvaluator {
    fixtures: Vec<BenchmarkFixture>,
}

impl AcceptanceEvaluator {
    pub fn new(fixtures: Vec<BenchmarkFixture>) -> Self {
        AcceptanceEvaluator { fixtures }
    }

    pub fn fixtures(&self) -> &[BenchmarkFixture] {
        &self.fixtures
    }

    pub fn fixture(&self, name: &str) -> Option<&BenchmarkFixture> {
        self.fixtures.iter().find(|fixture| fixture.name == name)
    }

    pub fn evaluate_world(
        &self,
        name: &str,
        compiled: &CompiledCircuit,
    ) -> Result<BenchmarkCase, String> {
        let fixture = self
            .fixture(name)
            .ok_or_else(|| format!("unknown benchmark fixture `{name}`"))?;
        let manifest = fixture.transition_manifest();
        if manifest.transitions().is_empty() {
            return Err(format!("`{name}` has an empty transition manifest"));
        }

        let mut worst = 0;
        for transition in manifest.transitions() {
            let mut world = compiled.world.clone();
            let drivers = install_drivers(&mut world, compiled, fixture)?;
            install_probes(&mut world, fixture)?;
            let sinks = output_sinks(compiled, fixture)?;
            let mut simulator = Simulator::new(world);
            simulator
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| format!("{} did not initially settle: {error:?}", fixture.name))?;

            drive(&mut simulator, &drivers, &transition.from);
            simulator
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| {
                    format!(
                        "{} did not settle at from={:?}: {error:?}",
                        fixture.name, transition.from
                    )
                })?;
            check_outputs(fixture, simulator.world(), &sinks, &transition.from)?;

            drive(&mut simulator, &drivers, &transition.to);
            let ticks = simulator
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| {
                    format!(
                        "{} did not settle at to={:?}: {error:?}",
                        fixture.name, transition.to
                    )
                })?;
            check_outputs(fixture, simulator.world(), &sinks, &transition.to)?;
            worst = worst.max(ticks);
        }

        let mut case = fixture.blank_case();
        case.generated_world_fingerprint = Some(canonical_world_fingerprint(&compiled.world));
        case.certified = true;
        case.physical = Some(physical_metrics(
            &compiled.world,
            fixture.lowered_netlist.gates.len() as u64,
        ));
        case.max_observed_settle_game_ticks_on_manifest = Some(worst);
        Ok(case)
    }

    pub fn capture_legacy(&self, baseline_commit: String) -> BenchmarkBaseline {
        let cases = self
            .fixtures
            .iter()
            .map(|fixture| {
                if !fixture.placements.is_empty() {
                    return fixture.blank_case();
                }
                match compile::compile_legacy(&fixture.lowered_netlist)
                    .map_err(|error| error.to_string())
                    .and_then(|compiled| self.evaluate_world(&fixture.name, &compiled))
                {
                    Ok(case) => case,
                    Err(error) => {
                        eprintln!("{}: new coverage ({error})", fixture.name);
                        fixture.blank_case()
                    }
                }
            })
            .collect();
        self.baseline_with_cases(baseline_commit, cases)
    }

    pub fn baseline(&self, baseline_commit: String) -> BenchmarkBaseline {
        self.baseline_with_cases(baseline_commit, Vec::new())
    }

    fn baseline_with_cases(
        &self,
        baseline_commit: String,
        cases: Vec<BenchmarkCase>,
    ) -> BenchmarkBaseline {
        let library = Library::default_library();
        BenchmarkBaseline {
            baseline_commit,
            build_profile: if cfg!(debug_assertions) {
                "debug".to_string()
            } else {
                "release".to_string()
            },
            cargo_features: Vec::new(),
            cell_library_revision: cell_library_revision(&library),
            simulator_revision: simulator_revision(),
            verifier_revision: physical_verifier_revision(),
            cases,
        }
    }
}

#[derive(Clone, Copy)]
enum Driver {
    Lever((i32, i32, i32)),
    CallerCell((i32, i32, i32)),
}

fn install_drivers(
    world: &mut World,
    compiled: &CompiledCircuit,
    fixture: &BenchmarkFixture,
) -> Result<Vec<Driver>, String> {
    fixture
        .input_ports
        .iter()
        .map(|name| {
            if let Some(pin) = fixture.placements.get(name) {
                require_empty_caller_cell(world, name, pin.at)?;
                Ok(Driver::CallerCell((pin.at.x, pin.at.y, pin.at.z)))
            } else {
                compiled
                    .input_positions
                    .get(name)
                    .copied()
                    .map(Driver::Lever)
                    .ok_or_else(|| format!("compiled circuit has no input `{name}`"))
            }
        })
        .collect()
}

fn install_probes(world: &mut World, fixture: &BenchmarkFixture) -> Result<(), String> {
    for name in &fixture.output_signals {
        if let Some(pin) = fixture.placements.get(name) {
            require_empty_caller_cell(world, name, pin.at)?;
            compile::probe_caller_cell(world, (pin.at.x, pin.at.y, pin.at.z));
        }
    }
    Ok(())
}

fn require_empty_caller_cell(world: &World, name: &str, at: Anchor) -> Result<(), String> {
    let kind = world.get(at.x, at.y, at.z).kind;
    if kind == BlockKind::Air {
        Ok(())
    } else {
        Err(format!(
            "`{name}` caller cell ({}, {}, {}) holds {kind:?}",
            at.x, at.y, at.z
        ))
    }
}

fn output_sinks(
    compiled: &CompiledCircuit,
    fixture: &BenchmarkFixture,
) -> Result<Vec<(i32, i32, i32)>, String> {
    fixture
        .output_signals
        .iter()
        .map(|name| {
            compiled
                .output_positions
                .get(name)
                .copied()
                .ok_or_else(|| format!("compiled circuit has no output `{name}`"))
        })
        .collect()
}

fn drive(simulator: &mut Simulator, drivers: &[Driver], bits: &[bool]) {
    for (driver, &bit) in drivers.iter().zip(bits) {
        match driver {
            Driver::Lever((x, y, z)) => {
                let mut state = simulator.world().get(*x, *y, *z).clone();
                state.lit = bit;
                simulator.world_mut().set(*x, *y, *z, state);
            }
            Driver::CallerCell(at) => compile::drive_caller_cell(simulator.world_mut(), *at, bit),
        }
    }
}

fn check_outputs(
    fixture: &BenchmarkFixture,
    world: &World,
    sinks: &[(i32, i32, i32)],
    bits: &[bool],
) -> Result<(), String> {
    let expected = (fixture.expected)(bits);
    if expected.len() != sinks.len() {
        return Err(format!(
            "{} expected-output count {} does not match {} declared outputs",
            fixture.name,
            expected.len(),
            sinks.len()
        ));
    }
    for ((name, &(x, y, z)), &want) in fixture.output_signals.iter().zip(sinks).zip(&expected) {
        let got = world.get(x, y, z).lit;
        if got != want {
            return Err(format!(
                "{} {:?} -> `{name}` expected {want}, got {got}",
                fixture.name, bits
            ));
        }
    }
    Ok(())
}

fn and4_expected(bits: &[bool]) -> Vec<bool> {
    vec![bits.iter().all(|&bit| bit)]
}

fn full_adder_expected(bits: &[bool]) -> Vec<bool> {
    let ones = bits.iter().filter(|&&bit| bit).count();
    vec![ones % 2 == 1, ones >= 2]
}

fn seven_segment_expected(bits: &[bool]) -> Vec<bool> {
    let digit = bits
        .iter()
        .fold(0usize, |value, &bit| value * 2 + usize::from(bit));
    (0..seven_segment::SEGMENT_NAMES.len())
        .map(|segment| {
            digit < seven_segment::TRUTH_TABLE.len()
                && seven_segment::TRUTH_TABLE[digit][segment] == 1
        })
        .collect()
}

fn segment_a_expected(bits: &[bool]) -> Vec<bool> {
    vec![seven_segment_expected(bits)[0]]
}

fn labels_in_order(labels: &[(String, String)], names: &[&str]) -> Result<Vec<String>, String> {
    names
        .iter()
        .map(|name| {
            labels
                .iter()
                .find(|(label, _)| label == name)
                .map(|(_, signal)| signal.clone())
                .ok_or_else(|| format!("Verilog output `{name}` is missing"))
        })
        .collect()
}

pub fn legacy_benchmark_evaluator() -> Result<AcceptanceEvaluator, String> {
    let (and4_netlist, and4_output) = and4::build_and4_netlist();
    let (adder_netlist, adder_outputs) = full_adder::build_full_adder_netlist();
    let (segment_netlist, segment_output) = seven_segment::build_single_segment_netlist(0);
    let (decoder_netlist, decoder_outputs) = seven_segment::build_seven_segment_netlist();

    let verilog_and4 = verilog::find("verilog:and4").expect("the catalog ships verilog:and4");
    let (verilog_and4_gate_level, verilog_and4_labels) = verilog_and4
        .synthesize()
        .map_err(|error| error.to_string())?;
    let verilog_and4_lowered =
        compile::lowering::lower(&verilog_and4_gate_level).map_err(|error| error.to_string())?;
    let verilog_and4_outputs = labels_in_order(&verilog_and4_labels, &[and4::OUTPUT_NAME])?;

    let verilog_decoder =
        verilog::find("verilog:seven_segment").expect("the catalog ships verilog:seven_segment");
    let (verilog_decoder_gate_level, verilog_decoder_labels) = verilog_decoder
        .synthesize()
        .map_err(|error| error.to_string())?;
    let verilog_decoder_lowered = compile::lowering::lower_optimised(&verilog_decoder_gate_level)
        .map_err(|error| error.to_string())?;
    let verilog_decoder_outputs =
        labels_in_order(&verilog_decoder_labels, &seven_segment::SEGMENT_NAMES)?;

    let mut pinned_decoder_ports = PortPlacements::default();
    for ((_, at, toward), signal) in pinned_glyph().iter().zip(&verilog_decoder_outputs) {
        pinned_decoder_ports.pin(signal.clone(), *at, *toward);
    }
    for (index, name) in seven_segment::INPUT_NAMES.iter().enumerate() {
        pinned_decoder_ports.pin(
            *name,
            Anchor {
                x: 76 + 12 * index as i32,
                y: 1,
                z: 120,
            },
            Facing::North,
        );
    }

    let inputs = |names: &[&str]| names.iter().map(|name| (*name).to_string()).collect();
    let decoder_output_signals = seven_segment::SEGMENT_NAMES
        .iter()
        .map(|name| decoder_outputs[*name].clone())
        .collect();
    let adder_output_signals = full_adder::OUTPUT_NAMES
        .iter()
        .map(|name| adder_outputs[name].clone())
        .collect();

    Ok(AcceptanceEvaluator::new(vec![
        BenchmarkFixture::new(
            "and4",
            and4_netlist,
            inputs(&and4::INPUT_NAMES),
            vec![and4_output],
            and4_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "verilog:and4",
            verilog_and4_lowered,
            inputs(&and4::INPUT_NAMES),
            verilog_and4_outputs,
            and4_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "full_adder",
            adder_netlist,
            inputs(&full_adder::INPUT_NAMES),
            adder_output_signals,
            full_adder_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "segment_a",
            segment_netlist,
            inputs(&seven_segment::INPUT_NAMES),
            vec![segment_output],
            segment_a_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "seven_segment",
            decoder_netlist,
            inputs(&seven_segment::INPUT_NAMES),
            decoder_output_signals,
            seven_segment_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "pinned:verilog:seven_segment",
            verilog_decoder_lowered,
            inputs(&seven_segment::INPUT_NAMES),
            verilog_decoder_outputs,
            seven_segment_expected,
            pinned_decoder_ports,
        ),
    ]))
}

fn pinned_glyph() -> [(&'static str, Anchor, Facing); 7] {
    [
        ("a", Anchor { x: 76, y: 1, z: 24 }, Facing::North),
        ("b", Anchor { x: 84, y: 1, z: 32 }, Facing::East),
        ("c", Anchor { x: 84, y: 1, z: 48 }, Facing::East),
        ("d", Anchor { x: 76, y: 1, z: 56 }, Facing::South),
        ("e", Anchor { x: 68, y: 1, z: 48 }, Facing::West),
        ("f", Anchor { x: 68, y: 1, z: 32 }, Facing::West),
        ("g", Anchor { x: 76, y: 1, z: 40 }, Facing::West),
    ]
}

#[derive(Serialize)]
struct CanonicalNetlist<'a> {
    inputs: &'a [String],
    outputs: &'a [String],
    gates: Vec<CanonicalGate<'a>>,
}

#[derive(Serialize)]
struct CanonicalGate<'a> {
    name: &'a str,
    inputs: &'a [String],
    output: &'a str,
    kind: &'static str,
    arity: usize,
}

fn canonical_netlist_fingerprint(netlist: &Netlist) -> Fingerprint {
    let canonical = CanonicalNetlist {
        inputs: &netlist.inputs,
        outputs: &netlist.outputs,
        gates: netlist
            .gates
            .iter()
            .map(|gate| CanonicalGate {
                name: &gate.name,
                inputs: &gate.inputs,
                output: &gate.output,
                kind: gate.kind.wire_name(),
                arity: gate.kind.arity(),
            })
            .collect(),
    };
    canonical_fingerprint(&serde_json::to_vec(&canonical).expect("a netlist must serialize"))
}

#[derive(Serialize)]
struct CanonicalPin<'a> {
    port: &'a str,
    x: i32,
    y: i32,
    z: i32,
    toward: &'static str,
}

fn canonical_pin_manifest_fingerprint(placements: &PortPlacements) -> Fingerprint {
    let pins: Vec<_> = placements
        .iter()
        .map(|(port, pin)| CanonicalPin {
            port,
            x: pin.at.x,
            y: pin.at.y,
            z: pin.at.z,
            toward: facing_name(pin.toward),
        })
        .collect();
    canonical_fingerprint(&serde_json::to_vec(&pins).expect("a pin manifest must serialize"))
}

#[derive(Serialize)]
struct CanonicalWorld {
    size: (i32, i32, i32),
    cells: Vec<CanonicalCell>,
}

#[derive(Serialize)]
struct CanonicalCell {
    x: i32,
    y: i32,
    z: i32,
    kind: &'static str,
    facing: Option<&'static str>,
    power: u8,
    lit: bool,
    delay: u8,
    face: Option<&'static str>,
}

pub fn canonical_world_fingerprint(world: &World) -> Fingerprint {
    let cells = world
        .cells()
        .iter()
        .enumerate()
        .filter_map(|(flat, &palette_index)| {
            let state = world
                .palette()
                .get(palette_index)
                .expect("a world cell must reference its palette");
            (state.kind != BlockKind::Air).then(|| canonical_cell(world, flat, state))
        })
        .collect();
    let canonical = CanonicalWorld {
        size: world.size(),
        cells,
    };
    canonical_fingerprint(&serde_json::to_vec(&canonical).expect("a world must serialize"))
}

fn canonical_cell(world: &World, flat: usize, state: &BlockState) -> CanonicalCell {
    let (x, y, z) = world.decode(flat);
    CanonicalCell {
        x,
        y,
        z,
        kind: block_kind_name(state.kind),
        facing: state.facing.map(facing_name),
        power: state.power,
        lit: state.lit,
        delay: state.delay,
        face: state.face.map(face_name),
    }
}

fn facing_name(facing: Facing) -> &'static str {
    match facing {
        Facing::North => "north",
        Facing::South => "south",
        Facing::East => "east",
        Facing::West => "west",
        Facing::Up => "up",
        Facing::Down => "down",
    }
}

fn face_name(face: Face) -> &'static str {
    match face {
        Face::Floor => "floor",
        Face::Wall => "wall",
        Face::Ceiling => "ceiling",
    }
}

fn block_kind_name(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Air => "air",
        BlockKind::Solid => "solid",
        BlockKind::Glass => "glass",
        BlockKind::Slab => "slab",
        BlockKind::RedstoneWire => "redstone-wire",
        BlockKind::Repeater => "repeater",
        BlockKind::Comparator => "comparator",
        BlockKind::Torch => "torch",
        BlockKind::WallTorch => "wall-torch",
        BlockKind::Lever => "lever",
        BlockKind::RedstoneBlock => "redstone-block",
        BlockKind::Lamp => "lamp",
        BlockKind::Piston => "piston",
        BlockKind::Button => "button",
        BlockKind::PressurePlate => "pressure-plate",
        BlockKind::WeightedPressurePlate => "weighted-pressure-plate",
        BlockKind::Observer => "observer",
        BlockKind::Target => "target",
        BlockKind::DaylightDetector => "daylight-detector",
        BlockKind::Other => "other",
    }
}

pub fn current_git_commit() -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|error| format!("could not invoke git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let revision = String::from_utf8(output.stdout)
        .map_err(|error| format!("git returned a non-UTF-8 revision: {error}"))?;
    let revision = revision.trim().to_string();
    if revision.is_empty() {
        Err("git returned an empty revision".to_string())
    } else {
        Ok(revision)
    }
}

pub fn refuse_existing_output(path: &Path, replace: bool) -> Result<(), String> {
    if path.exists() && !replace {
        Err(format!(
            "{} already exists; pass --replace to overwrite it",
            path.display()
        ))
    } else {
        Ok(())
    }
}

pub fn write_baseline_json(
    path: &Path,
    baseline: &BenchmarkBaseline,
    replace: bool,
) -> Result<(), String> {
    refuse_existing_output(path, replace)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let mut bytes = serde_json::to_vec_pretty(baseline)
        .map_err(|error| format!("could not serialize baseline: {error}"))?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        canonical_world_fingerprint, write_baseline_json, AcceptanceEvaluator, BenchmarkBaseline,
        BenchmarkFixture,
    };
    use crate::compile::fragment_synth::manifest::TransitionManifest;
    use crate::compile::metrics::canonical_fingerprint;
    use crate::compile::revisions::{
        cell_library_revision, physical_verifier_revision, simulator_revision,
    };
    use crate::compile::topology::Library;
    use crate::compile::{compile_legacy, Gate, Netlist};
    use crate::redstone::world::block::{BlockKind, BlockState, Facing};
    use crate::redstone::world::storage::World;

    #[test]
    fn literal_baseline_schema_preserves_case_order_and_certification_evidence() {
        let baseline: BenchmarkBaseline = serde_json::from_value(serde_json::json!({
            "baseline_commit": "0123456789abcdef",
            "build_profile": "release",
            "cargo_features": [],
            "cell_library_revision": "cell-library",
            "simulator_revision": "simulator",
            "verifier_revision": "verifier",
            "cases": [
                {
                    "name": "and4",
                    "lowered_netlist_hash": "and4-netlist",
                    "pin_manifest_hash": "and4-pins",
                    "transition_manifest_hash": "and4-transitions",
                    "generated_world_fingerprint": "and4-world",
                    "certified": true,
                    "physical": {
                        "non_air_blocks": 1,
                        "occupied_min": { "x": 0, "y": 0, "z": 0 },
                        "occupied_max": { "x": 0, "y": 0, "z": 0 },
                        "occupied_volume": 1,
                        "blocks_per_lowered_gate": { "numerator": 1, "denominator": 1 }
                    },
                    "max_observed_settle_game_ticks_on_manifest": 1
                },
                {
                    "name": "verilog:and4",
                    "lowered_netlist_hash": "verilog-and4-netlist",
                    "pin_manifest_hash": "verilog-and4-pins",
                    "transition_manifest_hash": "verilog-and4-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                },
                {
                    "name": "full_adder",
                    "lowered_netlist_hash": "full-adder-netlist",
                    "pin_manifest_hash": "full-adder-pins",
                    "transition_manifest_hash": "full-adder-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                },
                {
                    "name": "segment_a",
                    "lowered_netlist_hash": "segment-a-netlist",
                    "pin_manifest_hash": "segment-a-pins",
                    "transition_manifest_hash": "segment-a-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                },
                {
                    "name": "seven_segment",
                    "lowered_netlist_hash": "seven-segment-netlist",
                    "pin_manifest_hash": "seven-segment-pins",
                    "transition_manifest_hash": "seven-segment-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                },
                {
                    "name": "pinned:verilog:seven_segment",
                    "lowered_netlist_hash": "pinned-seven-segment-netlist",
                    "pin_manifest_hash": "pinned-seven-segment-pins",
                    "transition_manifest_hash": "pinned-seven-segment-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                }
            ]
        }))
        .expect("the literal baseline schema deserialises");

        assert_eq!(
            baseline
                .cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>(),
            [
                "and4",
                "verilog:and4",
                "full_adder",
                "segment_a",
                "seven_segment",
                "pinned:verilog:seven_segment",
            ]
        );

        for case in baseline.cases.iter().filter(|case| case.certified) {
            assert!(!baseline.baseline_commit.is_empty());
            assert!(!case.lowered_netlist_hash.as_str().is_empty());
            assert!(!case.pin_manifest_hash.as_str().is_empty());
            assert!(!case.transition_manifest_hash.as_str().is_empty());
            assert!(!case
                .generated_world_fingerprint
                .as_ref()
                .expect("a certified case records its world")
                .as_str()
                .is_empty());
            assert!(
                case.physical
                    .as_ref()
                    .expect("a certified case records physical metrics")
                    .non_air_blocks
                    > 0
            );
            assert!(case.max_observed_settle_game_ticks_on_manifest.is_some());
        }
    }

    fn tiny_fixture(expected: fn(&[bool]) -> Vec<bool>) -> BenchmarkFixture {
        BenchmarkFixture::new(
            "not",
            Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![Gate::nor("y", &["a"])],
            },
            vec!["a".into()],
            vec!["y".into()],
            expected,
            Default::default(),
        )
    }

    #[test]
    fn evaluator_certifies_real_truth_outputs_over_a_non_empty_manifest() {
        let evaluator = AcceptanceEvaluator::new(vec![tiny_fixture(|bits| vec![!bits[0]])]);
        let compiled = compile_legacy(evaluator.fixture("not").unwrap().lowered_netlist())
            .expect("the minimal fixture compiles");

        let case = evaluator
            .evaluate_world("not", &compiled)
            .expect("the real simulator certifies the inverter");

        assert!(case.certified);
        assert!(case.generated_world_fingerprint.is_some());
        assert!(case.physical.as_ref().unwrap().non_air_blocks > 0);
        assert!(case.max_observed_settle_game_ticks_on_manifest.is_some());
        assert_eq!(
            TransitionManifest::new(vec!["a".into()])
                .transitions()
                .len(),
            2
        );
    }

    #[test]
    fn evaluator_refuses_a_world_whose_truth_output_is_wrong() {
        let evaluator = AcceptanceEvaluator::new(vec![tiny_fixture(|bits| vec![bits[0]])]);
        let compiled = compile_legacy(evaluator.fixture("not").unwrap().lowered_netlist())
            .expect("the minimal fixture compiles");

        let error = evaluator.evaluate_world("not", &compiled).unwrap_err();
        assert!(error.contains("expected"), "unexpected error: {error}");
    }

    #[test]
    fn evaluator_refuses_an_expectation_that_omits_a_truth_output() {
        let evaluator = AcceptanceEvaluator::new(vec![tiny_fixture(|_| Vec::new())]);
        let compiled = compile_legacy(evaluator.fixture("not").unwrap().lowered_netlist())
            .expect("the minimal fixture compiles");

        let error = evaluator.evaluate_world("not", &compiled).unwrap_err();
        assert!(
            error.contains("expected-output count"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn canonical_world_hash_uses_size_yzx_cells_and_electrical_block_state() {
        let mut first = World::new(3, 2, 4);
        let mut repeater = BlockState::air();
        repeater.kind = BlockKind::Repeater;
        repeater.name = "minecraft:repeater".into();
        repeater.facing = Some(Facing::East);
        repeater.power = 15;
        repeater.lit = true;
        repeater.delay = 2;
        first.set(2, 1, 3, repeater.clone());

        let mut same_cells_different_palette_history = World::new(3, 2, 4);
        let mut temporary = BlockState::air();
        temporary.kind = BlockKind::Solid;
        temporary.name = "minecraft:stone".into();
        same_cells_different_palette_history.set(0, 0, 0, temporary);
        same_cells_different_palette_history.set(0, 0, 0, BlockState::air());
        same_cells_different_palette_history.set(2, 1, 3, repeater.clone());

        assert_eq!(
            canonical_world_fingerprint(&first),
            canonical_world_fingerprint(&same_cells_different_palette_history)
        );

        let mut changed_state = first.clone();
        repeater.delay = 3;
        changed_state.set(2, 1, 3, repeater);
        assert_ne!(
            canonical_world_fingerprint(&first),
            canonical_world_fingerprint(&changed_state)
        );
        assert_ne!(
            canonical_world_fingerprint(&first),
            canonical_world_fingerprint(&World::new(4, 2, 4))
        );
    }

    fn literal_empty_baseline() -> BenchmarkBaseline {
        BenchmarkBaseline {
            baseline_commit: "commit".into(),
            build_profile: "test".into(),
            cargo_features: Vec::new(),
            cell_library_revision: canonical_fingerprint(b"cell"),
            simulator_revision: canonical_fingerprint(b"sim"),
            verifier_revision: canonical_fingerprint(b"verify"),
            cases: Vec::new(),
        }
    }

    fn temporary_output(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "reda-fragment-baseline-{name}-{}.json",
            std::process::id()
        ))
    }

    #[test]
    fn baseline_writer_refuses_overwrite_until_replace_is_explicit() {
        let output = temporary_output("overwrite");
        let _ = std::fs::remove_file(&output);
        std::fs::write(&output, b"keep me").unwrap();

        let error = write_baseline_json(&output, &literal_empty_baseline(), false).unwrap_err();
        assert!(
            error.contains("already exists"),
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"keep me");

        write_baseline_json(&output, &literal_empty_baseline(), true).unwrap();
        let written: BenchmarkBaseline =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(written.baseline_commit, "commit");
        std::fs::remove_file(output).unwrap();
    }

    #[test]
    fn baseline_header_reads_only_task_zero_revision_authorities() {
        let baseline = AcceptanceEvaluator::new(Vec::new()).baseline("commit-from-git".into());
        let library = Library::default_library();

        assert_eq!(baseline.baseline_commit, "commit-from-git");
        assert_eq!(
            baseline.cell_library_revision,
            cell_library_revision(&library)
        );
        assert_eq!(baseline.simulator_revision, simulator_revision());
        assert_eq!(baseline.verifier_revision, physical_verifier_revision());
    }
}

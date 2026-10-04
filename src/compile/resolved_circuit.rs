//! Compiled source logic stitched to dedicated shared-bus terminal hardware.
//! Exports source-bound pinouts and checked shared-bus observation for native consumers.
#[cfg(test)]
use super::resolved_port::SharedBusTerminal;
use super::{
    planner::{Anchor, PortPlacements},
    resolved_port,
    routing::LowerBound,
    CompiledCircuit,
};
use crate::frontend::resolved::SemanticArtifact;
use crate::redstone::world::block::{BlockKind, Facing};
use std::collections::{BTreeMap, BTreeSet};

/// Source-owned port metadata. Physical readback leaves are never user inputs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPinout {
    pub inputs: BTreeMap<String, [i32; 3]>,
    pub outputs: BTreeMap<String, [i32; 3]>,
    pub inouts: Vec<ResolvedBusPort>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedBusPort {
    pub source: crate::frontend::PortBinding,
    pub channel: Vec<[i32; 3]>,
    pub external_entry: [i32; 3],
    pub external_pad: [i32; 3],
    pub readback: [i32; 3],
    pub external_value: [i32; 3],
    pub external_present: [i32; 3],
    pub conflict: [i32; 3],
    pub floating: [i32; 3],
    pub expected_value: [i32; 3],
    pub external_observed: [i32; 3],
    pub external_expected: [i32; 3],
}
impl ResolvedPinout {
    pub fn load_bundle(
        path: &std::path::Path,
        expected: &CompiledResolvedCircuit,
    ) -> Result<(crate::redstone::world::storage::World, Self), String> {
        let world = crate::formats::litematic::load(path).map_err(|e| e.to_string())?;
        let bytes = std::fs::read(path.with_extension("pinout.json")).map_err(|e| e.to_string())?;
        let pinout: Self = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        pinout.validate_loaded(&world, expected)?;
        Ok((world, pinout))
    }
    /// Bind loaded metadata and the initial world before installing caller controls.
    pub fn validate_loaded(
        &self,
        world: &crate::redstone::world::storage::World,
        expected: &CompiledResolvedCircuit,
    ) -> Result<(), String> {
        if self != &expected.pinout {
            return Err("shared-inout metadata differs from source-owned compiled ports".into());
        }
        if super::fragment_synth::benchmark::canonical_world_fingerprint(world)
            != super::fragment_synth::benchmark::canonical_world_fingerprint(&expected.physical.world)
        {
            return Err("shared-inout loaded world differs from compiled world".into());
        }
        Ok(())
    }
}
impl ResolvedBusPort {
    /// Settled checked observation; invalid bus states never become Boolean outputs.
    pub fn observe(
        &self,
        world: &crate::redstone::world::storage::World,
    ) -> Result<bool, crate::frontend::resolved::InvalidBus> {
        use crate::frontend::resolved::InvalidBus;
        let read = |p: [i32; 3], kind: BlockKind| {
            let cell = world.get(p[0], p[1], p[2]);
            if cell.kind != kind { return Err(InvalidBus::ProtocolViolation); }
            Ok(if kind == BlockKind::RedstoneWire { cell.power > 0 } else { cell.lit })
        };
        let value = read(self.readback, BlockKind::RedstoneWire)?;
        if self.channel.is_empty()
            || self.channel.iter().any(|p| read(*p, BlockKind::RedstoneWire) != Ok(value))
            || read(self.expected_value, BlockKind::Lamp)? != value
            || read(self.external_pad, BlockKind::RedstoneWire)?
                != read(self.external_expected, BlockKind::Lamp)?
            || read(self.external_observed, BlockKind::Lamp)?
                != read(self.external_expected, BlockKind::Lamp)?
        {
            return Err(InvalidBus::ProtocolViolation);
        }
        if read(self.conflict, BlockKind::Lamp)? { return Err(InvalidBus::Contention); }
        if read(self.floating, BlockKind::Lamp)? { return Err(InvalidBus::Floating); }
        Ok(value)
    }
}
pub struct CompiledResolvedCircuit {
    pub physical: CompiledCircuit,
    #[cfg(test)]
    pub(crate) terminals: Vec<SharedBusTerminal>,
    #[cfg(test)]
    pub(crate) source_inputs: BTreeMap<String, (i32, i32, i32)>,
    #[cfg(test)]
    pub(crate) protocol_inputs: BTreeMap<String, (i32, i32, i32)>,
    #[cfg(test)]
    pub(crate) source_outputs: BTreeMap<String, (i32, i32, i32)>,
    pub(crate) implementation: super::Netlist,
    pub pinout: ResolvedPinout,
}
impl CompiledResolvedCircuit {
    pub fn implementation_netlist(&self) -> &super::Netlist { &self.implementation }

    pub fn save_bundle(&self, path: &std::path::Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self.pinout).map_err(|e| e.to_string())?;
        crate::formats::litematic::save(path, &self.physical.world, "shared-inout")
            .map_err(|e| e.to_string())?;
        std::fs::write(path.with_extension("pinout.json"), bytes).map_err(|e| e.to_string())
    }
}
pub(crate) fn compile(semantic: &SemanticArtifact) -> Result<CompiledResolvedCircuit, String> {
    let mut placements = PortPlacements::default();
    // The entire terminal is caller-owned x<9; production placement/routing
    // must retain its existing immutable lower-bound exclusion authority.
    placements.bound_below(LowerBound { x: 9, z: 0 });
    let mut bases = Vec::new();
    let mut next_z = 0;
    for b in &semantic.boundaries {
        let count = b.internal_contributions.len() + 1;
        if !(1..=4).contains(&count) {
            return Err("unsupported shared-bus driver count".into());
        }
        let base = Anchor {
            x: 0,
            y: 0,
            z: next_z,
        };
        for (i, signal) in b
            .internal_contributions
            .iter()
            .map(|d| &d.injection_value)
            .enumerate()
        {
            placements.pin(
                signal.clone(),
                Anchor {
                    x: 8,
                    y: 1,
                    z: next_z + 1 + 4 * i as i32,
                },
                Facing::West,
            );
        }
        let read = b
            .read_input
            .as_ref()
            .ok_or("semantic artifact lacks typed physical readback leaf")?;
        placements.pin(
            read.clone(),
            Anchor {
                x: 8,
                y: 1,
                z: next_z + 3 + 4 * (count as i32 - 1),
            },
            Facing::East,
        );
        let external_read = b
            .external_read_input
            .as_ref()
            .ok_or("missing external caller sense leaf")?;
        placements.pin(
            external_read.clone(),
            Anchor {
                x: 8,
                y: 1,
                z: next_z + 1 + 4 * (count as i32 - 1),
            },
            Facing::East,
        );
        bases.push(base);
        next_z += 8 + 4 * count as i32;
    }
    let lowered = super::lowering::lower_optimised(&semantic.netlist).map_err(|e| e.to_string())?;
    let mut physical = super::compile_planned(&lowered, &placements).map_err(|e| e.to_string())?;
    let mut terminals = Vec::new();
    for (b, base) in semantic.boundaries.iter().zip(bases) {
        let terminal = resolved_port::materialize(&mut physical.world, b, base)?;
        let read = b.read_input.as_ref().unwrap();
        if physical.input_positions[read]
            != (
                terminal.read_output.x,
                terminal.read_output.y,
                terminal.read_output.z,
            )
        {
            return Err("physical readback input is detached from shared tap".into());
        }
        for p in terminal.internal.iter() {
            if physical.output_positions[&p.signal] != (p.at.x, p.at.y, p.at.z) {
                return Err("compiled source contribution is detached from injection pad".into());
            }
            let source = physical.world.get(p.at.x + 1, p.at.y, p.at.z);
            if source.kind != BlockKind::Repeater || source.facing != Some(Facing::East) {
                return Err("driver stitch lacks its source-to-pad delivery diode".into());
            }
        }
        let input = physical.world.get(
            terminal.read_output.x + 1,
            terminal.read_output.y,
            terminal.read_output.z,
        );
        if input.kind != BlockKind::Repeater || input.facing != Some(Facing::West) {
            return Err("reader stitch lacks shared-tap-to-reader delivery diode".into());
        }
        let external_read = b.external_read_input.as_ref().unwrap();
        if physical.input_positions[external_read]
            != (
                terminal.external.at.x,
                terminal.external.at.y,
                terminal.external.at.z,
            )
        {
            return Err("external caller sense is detached from actual injection pad".into());
        }
        terminals.push(terminal);
    }
    let protocol = semantic
        .boundaries
        .iter()
        .flat_map(|b| [b.external_value.clone(), b.external_present.clone()])
        .collect::<BTreeSet<_>>();
    let reads = semantic
        .boundaries
        .iter()
        .flat_map(|b| {
            [b.read_input.clone(), b.external_read_input.clone()]
                .into_iter()
                .flatten()
        })
        .collect::<BTreeSet<_>>();
    let source_inputs: BTreeMap<String, (i32, i32, i32)> = physical
        .input_positions
        .iter()
        .filter(|(name, _)| !protocol.contains(*name) && !reads.contains(*name))
        .map(|(n, p)| (n.clone(), *p))
        .collect();
    #[cfg(test)]
    let protocol_inputs = physical
        .input_positions
        .iter()
        .filter(|(name, _)| protocol.contains(*name))
        .map(|(n, p)| (n.clone(), *p))
        .collect();
    let inout_names = semantic
        .boundaries
        .iter()
        .map(|b| b.original.name.as_str())
        .collect::<BTreeSet<_>>();
    let source_outputs: BTreeMap<String, (i32, i32, i32)> = semantic
        .source_outputs
        .iter()
        .filter(|(name, _)| !inout_names.contains(name.as_str()))
        .map(|(name, signal)| (name.clone(), physical.output_positions[signal]))
        .collect();
    let coordinates = |p: &(i32, i32, i32)| [p.0, p.1, p.2];
    let anchor = |p: Anchor| [p.x, p.y, p.z];
    let output = |name: &str| physical.output_positions.get(name)
        .map(coordinates).ok_or_else(|| format!("missing physical status output {name}"));
    let mut inouts = Vec::new();
    for (b, terminal) in semantic.boundaries.iter().zip(&terminals) {
        inouts.push(ResolvedBusPort {
            source: b.original.clone(),
            channel: terminal.channel.iter().copied().map(anchor).collect(),
            external_entry: anchor(terminal.external_entry),
            external_pad: anchor(terminal.external.at),
            readback: anchor(terminal.read_output),
            external_value: coordinates(&physical.input_positions[&b.external_value]),
            external_present: coordinates(&physical.input_positions[&b.external_present]),
            conflict: output(&b.conflict)?,
            floating: output(&b.floating)?,
            expected_value: output(b.expected_value.as_deref().ok_or("missing expected value signal")?)?,
            external_observed: output(b.external_observed.as_deref().ok_or("missing external sense signal")?)?,
            external_expected: output(&b.external_injection)?,
        });
    }
    let pinout = ResolvedPinout {
        inputs: source_inputs.iter().map(|(n, p)| (n.clone(), coordinates(p))).collect(),
        outputs: source_outputs.iter().map(|(n, p)| (n.clone(), coordinates(p))).collect(),
        inouts,
    };
    Ok(CompiledResolvedCircuit {
        pinout,
        implementation: lowered,
        physical,
        #[cfg(test)]
        terminals,
        #[cfg(test)]
        source_inputs,
        #[cfg(test)]
        protocol_inputs,
        #[cfg(test)]
        source_outputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{
        evaluate::Evaluator, resolved::compile_for_materialization, SourceInput,
    };
    use crate::redstone::simulator::Simulator;
    #[test]
    fn pinout_roundtrip_retains_shared_bus_and_hides_physical_leaves() {
        let source = SourceInput {
            name: "fixture.sv",
            text: include_str!("../../tests/fixtures/resolved-inout.sv"),
        };
        let semantic = compile_for_materialization(&[source], "top").unwrap();
        let compiled = compile(&semantic).unwrap();
        let json = serde_json::to_string(&compiled.pinout).unwrap();
        let decoded: ResolvedPinout = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, compiled.pinout);
        decoded.validate_loaded(&compiled.physical.world, &compiled).unwrap();
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/qualification/inout-export-reload");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("shared-inout.litematic");
        compiled.save_bundle(&path).unwrap();
        let (loaded_world, loaded_pinout) = ResolvedPinout::load_bundle(&path, &compiled).unwrap();
        assert_eq!(loaded_pinout, compiled.pinout);
        assert_eq!(loaded_world.size(), compiled.physical.world.size());

        let mut wrong = decoded.clone();
        wrong.inouts[0].source.direction = crate::frontend::PortDirection::Input;
        assert!(wrong.validate_loaded(&compiled.physical.world, &compiled).is_err());
        wrong = decoded.clone();
        wrong.inouts[0].readback = [-1, 0, 0];
        assert!(wrong.validate_loaded(&compiled.physical.world, &compiled).is_err());
        wrong = decoded.clone();
        wrong.inouts[0].channel.pop();
        assert!(wrong.validate_loaded(&compiled.physical.world, &compiled).is_err());
        let mut changed = compiled.physical.world.clone();
        let pad = decoded.inouts[0].external_pad;
        changed.set(pad[0], pad[1], pad[2], crate::redstone::world::block::BlockState::air());
        assert!(decoded.validate_loaded(&changed, &compiled).is_err());

        assert_eq!(decoded.inputs.keys().map(String::as_str).collect::<Vec<_>>(),
                   vec!["d0", "d1", "e0", "e1"]);
        assert_eq!(decoded.outputs.keys().map(String::as_str).collect::<Vec<_>>(), vec!["y"]);
        assert_eq!(decoded.inouts.len(), 1);
        let bus = &decoded.inouts[0];
        assert_eq!(bus.source, semantic.boundaries[0].original);
        assert_eq!(bus.source.direction, crate::frontend::PortDirection::Inout);
        assert_ne!(bus.external_pad, bus.readback);
        for p in &bus.channel {
            assert_eq!(compiled.physical.world.get(p[0], p[1], p[2]).kind, BlockKind::RedstoneWire);
        }
        for p in [bus.external_value, bus.external_present] {
            assert_eq!(compiled.physical.world.get(p[0], p[1], p[2]).kind, BlockKind::Lever);
        }
        let mut malformed: serde_json::Value = serde_json::from_str(&json).unwrap();
        malformed["inouts"][0]["unknown_control"] = serde_json::json!([0,0,0]);
        assert!(serde_json::from_value::<ResolvedPinout>(malformed).is_err());
    }
    #[test]
    fn real_compiled_drivers_and_readers_use_same_shared_channel() {
        let source = SourceInput {
            name: "fixture.sv",
            text: include_str!("../../tests/fixtures/resolved-inout.sv"),
        };
        let semantic = compile_for_materialization(&[source], "top").unwrap();
        let physical = compile(&semantic).unwrap();
        assert_eq!(physical.source_inputs.len(), 4);
        assert_eq!(physical.protocol_inputs.len(), 2);
        assert_eq!(physical.source_outputs.len(), 1);
        eprintln!(
            "stitched source world={:?},gates={},source_inputs={},protocol_inputs={}",
            physical.physical.world.size(),
            physical.implementation.gates.len(),
            physical.source_inputs.len(),
            physical.protocol_inputs.len()
        );
        let b = &semantic.boundaries[0];
        let terminal = &physical.terminals[0];
        let mut evaluator = Evaluator::new(&semantic.netlist).unwrap();
        let mut implementation_evaluator = Evaluator::new(&physical.implementation).unwrap();
        let mut world = physical.physical.world.clone();
        let p = terminal.external_entry;
        let mut floor = crate::redstone::world::block::BlockState::air();
        floor.kind = BlockKind::Solid;
        floor.name = "minecraft:stone".into();
        world.set(p.x, p.y - 1, p.z, floor);
        let mut lever = crate::redstone::world::block::BlockState::air();
        lever.kind = BlockKind::Lever;
        lever.name = "minecraft:lever".into();
        lever.face = Some(crate::redstone::world::block::Face::Floor);
        lever.facing = Some(Facing::North);
        world.set(p.x, p.y, p.z, lever);
        let mut simulator = Simulator::new(world);
        simulator.run_until_stable(2000).unwrap();
        let raw = include_str!("../../tests/fixtures/resolved-inout-standard-oracle.csv");
        let mut checked = 0;
        for line in raw.lines() {
            let fields = line.split(',').collect::<Vec<_>>();
            if fields.len() != 4 {
                continue;
            }
            let before = fields[0].parse::<usize>().unwrap();
            let after = fields[1].parse::<usize>().unwrap();
            let phase = fields[2].parse::<usize>().unwrap();
            let state = if phase == 0 { before } else { after };
            let mut inputs = [
                ("e0", 0),
                ("d0", 1),
                ("e1", 2),
                ("d1", 3),
                (b.external_present.as_str(), 4),
                (b.external_value.as_str(), 5),
            ]
            .into_iter()
            .map(|(name, bit)| (name.to_string(), state & (1 << bit) != 0))
            .collect::<BTreeMap<_, _>>();
            for (name, at) in physical
                .source_inputs
                .iter()
                .chain(physical.protocol_inputs.iter())
            {
                let mut lever = simulator.world().get(at.0, at.1, at.2).clone();
                assert_eq!(lever.kind, BlockKind::Lever);
                lever.lit = inputs[name];
                simulator.world_mut().set(at.0, at.1, at.2, lever);
            }
            let p = terminal.external_entry;
            let mut drive = simulator.world().get(p.x, p.y, p.z).clone();
            drive.lit = inputs[&b.external_present] && inputs[&b.external_value];
            simulator.world_mut().set(p.x, p.y, p.z, drive);
            // External caller fixture follows the independent HDL testbench
            // driver. Internal source contributions are never evaluator-fed.
            // No caller injects source-evaluator values here. Only real source
            // input/protocol levers change; source logic drives every bus pad.
            simulator.run_until_stable(2000).unwrap();
            let value = simulator
                .world()
                .get(
                    terminal.read_output.x,
                    terminal.read_output.y,
                    terminal.read_output.z,
                )
                .power
                > 0;
            inputs.insert(b.read_input.clone().unwrap(), value);
            inputs.insert(
                b.external_read_input.clone().unwrap(),
                simulator
                    .world()
                    .get(
                        terminal.external.at.x,
                        terminal.external.at.y,
                        terminal.external.at.z,
                    )
                    .power
                    > 0,
            );
            let expected = evaluator.step(&inputs).unwrap();
            let gates = implementation_evaluator.settle(&inputs).unwrap();
            for gate in &physical.implementation.gates {
                let p = physical.physical.gate_output_positions[&gate.output];
                let s = simulator.world().get(p.0, p.1, p.2);
                let actual = if s.kind == BlockKind::RedstoneWire {
                    s.power > 0
                } else {
                    s.lit
                };
                assert_eq!(
                    actual, gates[&gate.output],
                    "internal gate={} state={state},from={before},to={after},phase={phase}",
                    gate.output
                );
            }
            for p in terminal
                .internal
                .iter()
                .chain(std::iter::once(&terminal.external))
            {
                assert_eq!(
                    simulator.world().get(p.at.x, p.at.y, p.at.z).power > 0,
                    expected[&p.signal],
                    "driver={} state={state}",
                    p.signal
                );
            }
            for p in &terminal.channel {
                assert_eq!(simulator.world().get(p.x, p.y, p.z).power > 0, value);
            }
            let mut observed = expected.clone();
            for (name, p) in &physical.physical.output_positions {
                let state = simulator.world().get(p.0, p.1, p.2);
                let v = if state.kind == BlockKind::RedstoneWire {
                    state.power > 0
                } else {
                    state.lit
                };
                observed.insert(name.clone(), v);
            }
            let actual = b.observe(&observed);
            assert_eq!(physical.pinout.inouts[0].observe(simulator.world()), actual,
                       "consumer observation state={state},from={before},to={after},phase={phase}");
            let class = match actual {
                Ok(false) => "0",
                Ok(true) => "1",
                Err(crate::frontend::resolved::InvalidBus::Floating) => "Z",
                Err(crate::frontend::resolved::InvalidBus::Contention) => "X",
                Err(crate::frontend::resolved::InvalidBus::ProtocolViolation) => {
                    "protocol mismatch"
                }
            };
            assert_eq!(
                class, fields[3],
                "state={state},from={before},to={after},phase={phase}"
            );
            if let Ok(v) = actual {
                let y = physical.source_outputs["y"];
                assert_eq!(
                    simulator.world().get(y.0, y.1, y.2).lit,
                    v,
                    "physical source reader y state={state}"
                );
            }
            checked += 1;
            if checked % 1024 == 0 {
                eprintln!("stitched source qualified oracle rows={checked}/8192");
            }
        }
        assert_eq!(checked, 8192);

        // Independent caller violates both declared data polarities in turn.
        for name in ["e0", "e1"] {
            let at = physical.source_inputs[name];
            let mut cell = simulator.world().get(at.0,at.1,at.2).clone();
            cell.lit = false; simulator.world_mut().set(at.0,at.1,at.2,cell);
        }
        for declared in [true,false] {
            for (name,on) in [(&b.external_present,true),(&b.external_value,declared)] {
                let at = physical.protocol_inputs[name];
                let mut cell = simulator.world().get(at.0,at.1,at.2).clone();
                cell.lit = on; simulator.world_mut().set(at.0,at.1,at.2,cell);
            }
            let at = terminal.external_entry;
            let mut caller = simulator.world().get(at.x,at.y,at.z).clone();
            caller.lit = !declared; simulator.world_mut().set(at.x,at.y,at.z,caller);
            simulator.run_until_stable(2000).unwrap();
            assert_eq!(physical.pinout.inouts[0].observe(simulator.world()),
                       Err(crate::frontend::resolved::InvalidBus::ProtocolViolation));
            let mut caller = simulator.world().get(at.x,at.y,at.z).clone();
            caller.lit = declared; simulator.world_mut().set(at.x,at.y,at.z,caller);
            simulator.run_until_stable(2000).unwrap();
            assert_eq!(physical.pinout.inouts[0].observe(simulator.world()),Ok(declared));
            let y = physical.source_outputs["y"];
            assert_eq!(simulator.world().get(y.0,y.1,y.2).lit,declared);
        }
    }
}

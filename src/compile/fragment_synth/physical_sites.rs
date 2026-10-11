//! Source-rooted functions and exact physical sites retained from typed owners.
//! World scans only check inventory coverage; no connection is inferred from it.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use super::candidate::ExpandedPhysicalCandidate;
use super::identity::{ObservationId, PhysicalEndpointId};
use super::topology::{primitive_meanings, BooleanMeaning};
use crate::compile::{Netlist, geometry::Anchor, routing::RealisedRouteTree};
use crate::redstone::world::{block::BlockKind, storage::World};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Serialize))]
pub(crate) enum GateFunction {
    Signal(String),
    Primitive { meaning: BooleanMeaning, inputs: Vec<String> },
}
impl GateFunction {
    pub(crate) fn evaluate(&self, values: &HashMap<&str, bool>) -> Option<bool> {
        match self {
            Self::Signal(name) => values.get(name.as_str()).copied(),
            Self::Primitive { meaning, inputs } => meaning.evaluate(&inputs.iter().map(|name| values.get(name.as_str()).copied()).collect::<Option<Vec<_>>>()?),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Serialize))]
pub(crate) struct GateSite { pub kind: BlockKind, pub function: GateFunction }
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateInventory { pub complete: bool, pub sites: BTreeMap<Anchor, GateSite> }
impl Default for GateInventory {
    fn default() -> Self { Self { complete: true, sites: BTreeMap::new() } }
}
fn component(kind: BlockKind) -> bool {
    matches!(kind, BlockKind::Torch | BlockKind::WallTorch | BlockKind::Repeater | BlockKind::Comparator)
}
impl GateInventory {
    fn insert(&mut self, at: Anchor, site: GateSite) -> Option<()> {
        if self.sites.insert(at, site).is_some() { self.complete = false; return None; }
        Some(())
    }
    pub(crate) fn from_candidate(candidate: &ExpandedPhysicalCandidate, source: &Netlist) -> Option<Self> {
        candidate.validate_shape().ok()?;
        candidate.validate_physical_ownership().ok()?;
        let mut functions = BTreeMap::<PhysicalEndpointId, GateFunction>::new();
        for (port, signal) in candidate.instances.primary_inputs.iter().zip(&source.inputs) {
            functions.insert(PhysicalEndpointId::PrimaryInput(*port), GateFunction::Signal(signal.clone()));
        }
        for instance in &candidate.instances.instances {
            let gate = source.gates.get(instance.logical_gate.0 as usize)?;
            for (primitive, meaning) in primitive_meanings(&instance.expanded.topology, gate.kind).ok()? {
                functions.insert(PhysicalEndpointId::PrimitiveOutput(primitive), GateFunction::Primitive { meaning, inputs: gate.inputs.clone() });
            }
            functions.insert(PhysicalEndpointId::Junction(instance.id), GateFunction::Signal(gate.output.clone()));
        }
        let mut inventory = Self::default();
        for (endpoint, function) in &functions {
            let observation = match endpoint {
                PhysicalEndpointId::PrimitiveOutput(id) => ObservationId::PrimitiveOutput(*id),
                PhysicalEndpointId::Junction(id) => ObservationId::JunctionOutput(*id),
                _ => continue,
            };
            let Some(site) = candidate.observations.get(&observation) else { continue; };
            inventory.insert(site.site.at, GateSite { kind: site.state.kind, function: function.clone() })?;
        }
        for route in candidate.routes.values() {
            let function = functions.get(&route.source)?.clone();
            for block in &route.cells {
                if component(block.state.kind) {
                    if block.state.kind != BlockKind::Repeater { return None; }
                    inventory.insert(block.at, GateSite { kind: block.state.kind, function: function.clone() })?;
                }
            }
        }
        for (endpoint, boundary) in &candidate.boundaries {
            let signal = match endpoint {
                PhysicalEndpointId::PrimaryInput(port) => source.inputs.get(port.0 as usize)?,
                PhysicalEndpointId::DeclaredOutput(port) => source.outputs.get(port.0 as usize)?,
                _ => return None,
            };
            for block in &boundary.blocks {
                if component(block.state.kind) {
                    if block.state.kind != BlockKind::Repeater { return None; }
                    inventory.insert(block.at, GateSite { kind: block.state.kind, function: GateFunction::Signal(signal.clone()) })?;
                }
            }
        }
        Some(inventory)
    }
    pub(crate) fn absorb(&mut self, child: &Self, by: Anchor) -> Option<()> {
        self.complete &= child.complete;
        for (at, site) in &child.sites {
            let at = Anchor { x: at.x.checked_add(by.x)?, y: at.y.checked_add(by.y)?, z: at.z.checked_add(by.z)? };
            self.insert(at, site.clone())?;
        }
        Some(())
    }
    pub(crate) fn add_route(&mut self, route: &RealisedRouteTree, signal: &str) -> Option<()> {
        for block in &route.cells {
            if component(block.state.kind) {
                if block.state.kind != BlockKind::Repeater { return None; }
                self.insert(block.at, GateSite { kind: block.state.kind, function: GateFunction::Signal(signal.into()) })?;
            }
        }
        Some(())
    }
    pub(crate) fn add_buffer(&mut self, at: Anchor, signal: &str) -> Option<()> {
        self.insert(at, GateSite { kind: BlockKind::Repeater, function: GateFunction::Signal(signal.into()) })
    }
    pub(crate) fn covers(&self, world: &World) -> bool {
        if !self.complete { return false; }
        let expected = self.sites.iter().filter(|(_, site)| component(site.kind)).map(|(at, _)| *at).collect::<BTreeSet<_>>();
        let actual = world.cells().iter().enumerate().filter_map(|(flat, &index)| {
            component(world.palette().get(index)?.kind).then(|| { let (x,y,z)=world.decode(flat); Anchor{x,y,z} })
        }).collect::<BTreeSet<_>>();
        expected == actual && self.sites.iter().all(|(at, site)| world.get(at.x, at.y, at.z).kind == site.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{Gate, fragment_synth::{recursive, config::SearchConfig}};
    use crate::redstone::simulator::Simulator;

    #[test]
    fn certified_leaf_and_composition_inventory_match_every_actual_gate_truth() {
        let source = Netlist { inputs: vec!["a".into(), "b".into()], outputs: vec!["y".into()],
            gates: vec![Gate::nor("mid", &["a", "b"]), Gate::nor("y", &["mid"])] };
        let product = recursive::compile_proposal(&source, None, &SearchConfig::checked_defaults(),
            recursive::StructuralChoice::MinimumCrossing).unwrap();
        let inventory = product.gate_inventory.as_ref().unwrap();
        assert!(inventory.covers(&product.world), "complete typed inventory {:?}", inventory);
        assert!(inventory.sites.len() >= source.gates.len());
        let mut simulator = Simulator::new(product.world.clone());
        simulator.run_until_stable_bounded(2000, 1_000_000).unwrap();
        for from in 0..4 { for to in 0..4 { for address in [from, to] {
            let mut values = HashMap::new();
            for (bit, name) in source.inputs.iter().enumerate() {
                let value = address & (1 << bit) != 0;
                values.insert(name.as_str(), value);
                let at = product.input_positions[name];
                let mut lever = simulator.world().get(at.0, at.1, at.2).clone(); lever.lit = value;
                simulator.world_mut().set(at.0, at.1, at.2, lever);
            }
            values.insert("mid", !(values["a"] || values["b"])); values.insert("y", !values["mid"]);
            simulator.run_until_stable_bounded(2000, 1_000_000).unwrap();
            for (at, site) in &inventory.sites {
                let state = simulator.world().get(at.x, at.y, at.z);
                let actual = match state.kind { BlockKind::RedstoneWire => state.power > 0, _ => state.lit };
                assert_eq!(Some(actual), site.function.evaluate(&values), "site {at:?} sourceaddress {address}");
            }
        } } }
        let mut missing = inventory.clone(); missing.sites.pop_first();
        assert!(!missing.covers(&product.world));
        let mut hidden = product.world.clone();
        let at = Anchor { x: 0, y: 0, z: 0 };
        assert_eq!(hidden.get(at.x, at.y, at.z).kind, BlockKind::Air);
        hidden.set(at.x, at.y, at.z, crate::compile::repeater(crate::redstone::world::block::Facing::South));
        assert!(!inventory.covers(&hidden));
        let mut duplicate = inventory.clone();
        let (&at, site) = inventory.sites.first_key_value().unwrap();
        assert!(duplicate.insert(at, site.clone()).is_none());
        assert!(!duplicate.covers(&product.world));
        let by = Anchor { x: 3, y: 4, z: 5 };
        let mut translated = GateInventory::default(); translated.absorb(inventory, by).unwrap();
        assert_eq!(translated.sites.len(), inventory.sites.len());
        for (at, site) in &inventory.sites {
            assert_eq!(translated.sites.get(&Anchor{x:at.x+by.x,y:at.y+by.y,z:at.z+by.z}),Some(site));
        }
        let mut bad = GateInventory::default();
        assert!(bad.absorb(inventory, Anchor {x:i32::MAX,y:0,z:0}).is_none());
    }
}

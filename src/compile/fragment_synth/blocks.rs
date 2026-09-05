//! Compiling a Verilog module once as a reusable, unrotated "block".
//!
//! A block is compiled by the existing unpinned seed
//! ([`compile_sparse_seed_with_services`] with `pins: None`) -- nothing
//! about the seed changes here. The unpinned seed already places every
//! automatic primary input as a lever one input-channel west of the first
//! level (on a stone floor), and every declared output as a lamp one
//! channel east of the last level; those cells ARE the block's port table.
//! This module only reads them back off the compiled candidate.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::compile::fragment_synth::candidate::ExpandedPhysicalCandidate;
use crate::compile::fragment_synth::certification::{CandidateMetrics, CertifiedCandidate};
use crate::compile::fragment_synth::identity::{PhysicalEndpointId, PortId};
use crate::compile::fragment_synth::relocate;
use crate::compile::fragment_synth::seed::{compile_sparse_seed_with_services, SeedError, SeedInput, SeedServices};
use crate::compile::fragment_synth::timing_graph::ExactDelay;
use crate::compile::geometry::Anchor;
use crate::compile::Netlist;
use crate::redstone::world::block::{BlockKind, Facing};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockBounds {
    pub min: Anchor,
    pub max: Anchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockPort {
    pub cell: Anchor,
    pub toward: Facing,
}

#[derive(Debug, Clone)]
pub(crate) struct CompiledBlock {
    pub module: String,
    /// The netlist `candidate` was certified against, and the block's
    /// declared interface: `lowered.inputs`/`lowered.outputs` are the
    /// module's own ports, in declared order, and `lowered.gates` is
    /// everything the candidate realises.
    ///
    /// For a leaf that is the module's own gates. For a module that itself
    /// instantiates something it is that module's whole FLATTENING --
    /// grandchildren included -- because that is what its candidate holds.
    /// The two must not diverge: the union one level up reads these gates
    /// (`union::block_reads_input`) to tell "this port feeds nothing" from
    /// "the splice lost a route", and a netlist narrower than the candidate
    /// makes every port consumed only by a grandchild look unused, which
    /// retires the parent's delivery to a route that is really there.
    /// `HierarchicalNetlist::flatten` copies the top module's `inputs` and
    /// `outputs` verbatim, so a flattening carries exactly the same port
    /// name list, in the same order, as `LoweredHierarchy::block_netlist`
    /// would -- which is what keeps `union::planning_netlist`'s port
    /// bindings and `from_certified`'s `PortId` indexing lined up.
    pub lowered: Netlist,
    /// Block-local coordinates, exactly as compiled.
    pub candidate: ExpandedPhysicalCandidate,
    pub bounds: BlockBounds,
    /// By port name; `lowered.inputs`/`lowered.outputs` carry the declared order.
    pub inputs: BTreeMap<String, BlockPort>,
    pub outputs: BTreeMap<String, BlockPort>,
    /// `metrics.quality.static_routed_delay`.
    pub delay: ExactDelay,
    pub metrics: CandidateMetrics,
}

#[derive(Debug, Error)]
pub(crate) enum BlockError {
    #[error("compiling block `{module}` failed: {source}")]
    Seed { module: String, source: SeedError },
    #[error("block `{module}` has no compiled boundary for port `{port}`")]
    MissingBoundary { module: String, port: String },
    #[error("block `{module}` compiled with a pinned frame; blocks must stay unpinned")]
    PinnedFrame { module: String },
    #[error("block `{module}` declares more ports than a typed port identity can index")]
    IdentityOverflow { module: String },
}

pub(crate) fn compile_block(
    module: &str,
    lowered: &Netlist,
    services: SeedServices<'_>,
) -> Result<CompiledBlock, BlockError> {
    let input = SeedInput {
        lowered,
        source_provenance: None,
        pins: None,
    };
    let certified =
        compile_sparse_seed_with_services(input, services).map_err(|source| BlockError::Seed {
            module: module.to_string(),
            source,
        })?;
    CompiledBlock::from_certified(module, lowered, &certified)
}

impl CompiledBlock {
    /// Read a block's port table off a candidate that has already been
    /// certified for `lowered`.
    ///
    /// Split out of [`compile_block`] because a module that itself contains
    /// module instances is not compiled by the flat seed at all: it is
    /// planned around its own blocks and unioned into a flat candidate
    /// (`hierarchy_api::compile_module_with_blocks`), and that candidate
    /// then becomes a [`CompiledBlock`] for its own parent. Both routes
    /// must read the port table by exactly the same rule, so there is only
    /// one place that reads it.
    ///
    /// `lowered` must be the netlist `certified` was certified against --
    /// for that second route, the module's flattening, NOT its own gates.
    /// See [`CompiledBlock::lowered`].
    pub(crate) fn from_certified(
        module: &str,
        lowered: &Netlist,
        certified: &CertifiedCandidate,
    ) -> Result<CompiledBlock, BlockError> {
        let candidate = certified.candidate().clone();
        // `pins: None` on every route into here means `bind_pin_contracts`
        // (seed.rs) never resolves a pin, so `automatic_boundary_direction`
        // (seed.rs) always takes its `pin_contracts.is_empty()` branch and
        // returns `Facing::East` for every boundary it places. Check that
        // invariant explicitly here rather than just hardcoding
        // `Facing::East` on the `BlockPort`s below on faith -- a block that
        // somehow compiled with a pinned frame would place its boundaries
        // by a different rule, and every later task that stamps this block
        // by its `toward` would silently be wrong.
        if !candidate.pin_contracts.is_empty() {
            return Err(BlockError::PinnedFrame {
                module: module.to_string(),
            });
        }
        let metrics = certified.metrics().clone();

        let mut inputs = BTreeMap::new();
        for (index, name) in lowered.inputs.iter().enumerate() {
            let port = PortId(u32::try_from(index).map_err(|_| BlockError::IdentityOverflow {
                module: module.to_string(),
            })?);
            let endpoint = PhysicalEndpointId::PrimaryInput(port);
            let lever = candidate
                .boundaries
                .get(&endpoint)
                .and_then(|boundary| {
                    boundary
                        .blocks
                        .iter()
                        .find(|block| block.state.kind == BlockKind::Lever)
                })
                .ok_or_else(|| BlockError::MissingBoundary {
                    module: module.to_string(),
                    port: name.clone(),
                })?;
            inputs.insert(
                name.clone(),
                BlockPort {
                    cell: lever.at,
                    toward: Facing::East,
                },
            );
        }

        let mut outputs = BTreeMap::new();
        for (index, name) in lowered.outputs.iter().enumerate() {
            let port = PortId(u32::try_from(index).map_err(|_| BlockError::IdentityOverflow {
                module: module.to_string(),
            })?);
            let endpoint = PhysicalEndpointId::DeclaredOutput(port);
            let lamp = candidate
                .boundaries
                .get(&endpoint)
                .and_then(|boundary| {
                    boundary
                        .blocks
                        .iter()
                        .find(|block| block.state.kind == BlockKind::Lamp)
                })
                .ok_or_else(|| BlockError::MissingBoundary {
                    module: module.to_string(),
                    port: name.clone(),
                })?;
            outputs.insert(
                name.clone(),
                BlockPort {
                    cell: lamp.at,
                    toward: Facing::East,
                },
            );
        }

        let bounds = bounds_of(&candidate);
        let delay = metrics.quality.static_routed_delay;
        Ok(CompiledBlock {
            module: module.to_string(),
            lowered: lowered.clone(),
            candidate,
            bounds,
            inputs,
            outputs,
            delay,
            metrics,
        })
    }
}

/// The axis-aligned box containing every anchor `candidate` owns, per
/// [`relocate::anchors_of`] -- the same exhaustive walk `translate` uses, so
/// a bounding box computed here can never miss a field either function
/// knows about.
fn bounds_of(candidate: &ExpandedPhysicalCandidate) -> BlockBounds {
    let mut anchors = relocate::anchors_of(candidate).into_iter();
    let first = anchors
        .next()
        .expect("a compiled candidate places at least one block");
    let (mut min, mut max) = (first, first);
    for anchor in anchors {
        min = Anchor {
            x: min.x.min(anchor.x),
            y: min.y.min(anchor.y),
            z: min.z.min(anchor.z),
        };
        max = Anchor {
            x: max.x.max(anchor.x),
            y: max.y.max(anchor.y),
            z: max.z.max(anchor.z),
        };
    }
    BlockBounds { min, max }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::compile::fragment_synth::relocate::Offset;

    #[test]
    fn a_full_adder_block_exposes_lever_inputs_west_and_lamp_outputs_east() {
        let netlist = crate::compile::lowering::lower_optimised(&crate::circuits::full_adder::build_full_adder_netlist().0).unwrap();
        let (library, config) = crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        let block = compile_block("full_adder", &netlist, services).expect("compiles");
        assert_eq!(block.inputs.len(), 3);
        assert_eq!(block.outputs.len(), 2);
        for port in block.inputs.values() {
            assert_eq!(port.toward, Facing::East);
            assert!(port.cell.x < block.outputs.values().map(|p| p.cell.x).min().unwrap());
            assert_eq!(block.candidate.boundaries.values().flat_map(|b| &b.blocks).find(|b| b.at == port.cell).unwrap().state.kind, BlockKind::Lever);
        }
        for port in block.outputs.values() {
            assert_eq!(port.toward, Facing::East);
            assert_eq!(block.candidate.boundaries.values().flat_map(|b| &b.blocks).find(|b| b.at == port.cell).unwrap().state.kind, BlockKind::Lamp);
        }
        assert!(block.bounds.min.x >= 16 && block.bounds.min.z >= 16, "unpinned seed shifts to the origin margin");
        assert_eq!(block.bounds.min.y, 0, "floors under the ground row");
        assert!(block.bounds.max.y <= 4);
        assert_eq!(block.delay, block.metrics.quality.static_routed_delay);
    }

    fn full_adder_lowered() -> Netlist {
        crate::compile::lowering::lower_optimised(
            &crate::circuits::full_adder::build_full_adder_netlist().0,
        )
        .expect("full adder netlist lowers")
    }

    fn full_adder_block() -> CompiledBlock {
        let netlist = full_adder_lowered();
        let (library, config) =
            crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        compile_block("full_adder", &netlist, services).expect("full adder block compiles")
    }

    #[test]
    fn every_input_cell_is_strictly_west_of_every_output_cell() {
        let block = full_adder_block();
        let furthest_west_output = block
            .outputs
            .values()
            .map(|port| port.cell.x)
            .min()
            .expect("full adder declares outputs");
        for port in block.inputs.values() {
            assert!(
                port.cell.x < furthest_west_output,
                "input at {:?} is not strictly west of the nearest output (x = {furthest_west_output})",
                port.cell
            );
        }
    }

    #[test]
    fn the_port_table_covers_every_declared_port_and_nothing_else() {
        let netlist = full_adder_lowered();
        let block = full_adder_block();
        let expected_inputs: BTreeSet<String> = netlist.inputs.iter().cloned().collect();
        let expected_outputs: BTreeSet<String> = netlist.outputs.iter().cloned().collect();
        let actual_inputs: BTreeSet<String> = block.inputs.keys().cloned().collect();
        let actual_outputs: BTreeSet<String> = block.outputs.keys().cloned().collect();
        assert_eq!(actual_inputs, expected_inputs, "missing or extra input ports");
        assert_eq!(actual_outputs, expected_outputs, "missing or extra output ports");
    }

    #[test]
    fn bounds_contain_every_anchor_the_candidate_owns() {
        let block = full_adder_block();
        for anchor in relocate::anchors_of(&block.candidate) {
            assert!(
                anchor.x >= block.bounds.min.x && anchor.x <= block.bounds.max.x,
                "anchor {anchor:?} escapes bounds on x"
            );
            assert!(
                anchor.y >= block.bounds.min.y && anchor.y <= block.bounds.max.y,
                "anchor {anchor:?} escapes bounds on y"
            );
            assert!(
                anchor.z >= block.bounds.min.z && anchor.z <= block.bounds.max.z,
                "anchor {anchor:?} escapes bounds on z"
            );
        }
    }

    #[test]
    fn translating_the_compiled_candidate_moves_every_port_cell_by_the_same_offset() {
        let netlist = full_adder_lowered();
        let block = full_adder_block();
        let offset = Offset {
            dx: 37,
            dy: 0,
            dz: -5,
        };
        let mut moved = block.candidate.clone();
        relocate::translate(&mut moved, offset);

        for (index, name) in netlist.inputs.iter().enumerate() {
            let endpoint = PhysicalEndpointId::PrimaryInput(PortId(index as u32));
            let moved_cell = moved.boundaries[&endpoint]
                .blocks
                .iter()
                .find(|block| block.state.kind == BlockKind::Lever)
                .expect("translated candidate keeps its lever")
                .at;
            let original = block.inputs[name].cell;
            assert_eq!(
                moved_cell,
                Anchor {
                    x: original.x + offset.dx,
                    y: original.y + offset.dy,
                    z: original.z + offset.dz,
                },
                "input `{name}` did not move by the offset"
            );
        }
        for (index, name) in netlist.outputs.iter().enumerate() {
            let endpoint = PhysicalEndpointId::DeclaredOutput(PortId(index as u32));
            let moved_cell = moved.boundaries[&endpoint]
                .blocks
                .iter()
                .find(|block| block.state.kind == BlockKind::Lamp)
                .expect("translated candidate keeps its lamp")
                .at;
            let original = block.outputs[name].cell;
            assert_eq!(
                moved_cell,
                Anchor {
                    x: original.x + offset.dx,
                    y: original.y + offset.dy,
                    z: original.z + offset.dz,
                },
                "output `{name}` did not move by the offset"
            );
        }
    }
}

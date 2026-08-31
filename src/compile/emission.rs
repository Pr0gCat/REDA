//! Lossless block emission shared by legacy and fragment-synthesis candidates.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::compile::fragment_synth::identity::{
    InstanceId, PhysicalEndpointId, PrimitiveId, RouteId, RoutedSinkId,
};
use crate::compile::geometry::Anchor;
use crate::redstone::world::block::{BlockKind, BlockState};
use crate::redstone::world::storage::World;

/// The electrically meaningful role of one emitted block.
///
/// This is intentionally name-free. Both the legacy planner and the expanded
/// fragment candidate must resolve their local bookkeeping to stable typed
/// identities before crossing this boundary. In particular, a route terminal
/// keeps its sink, target, terminal kind and complete path repeater count; its
/// [`BlockState`] separately keeps the exact facing and per-component delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PhysicalBlockRole {
    Primitive(PrimitiveId),
    Boundary(PhysicalEndpointId),
    Junction(InstanceId),
    RouteConductor(RouteId),
    RouteFloor(RouteId),
    RouteTerminal {
        sink: RoutedSinkId,
        target: PhysicalEndpointId,
        kind: TerminalKind,
        repeaters: u64,
    },
    DeclaredOutputLamp(PhysicalEndpointId),
}

/// Durable terminal classification used by the emission/verifier boundary.
///
/// This mirrors the physical distinctions the verifier needs without making
/// the durable emitter depend on planner-owned policy types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalKind {
    RepeaterIntoSupport,
    DirectedDustIntoSupport,
    BareMergeDust,
    BareMergeRepeater,
    OutputTerminalRepeater,
}

/// A borrowed, exact block claim supplied by a candidate adapter.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PhysicalBlockRef<'a> {
    pub at: Anchor,
    pub state: &'a BlockState,
    pub role: PhysicalBlockRole,
}

/// Minimal lossless source boundary for durable block emission.
///
/// Implementations belong in the legacy and expanded adapters. They may walk
/// any internal representation, but every final physical claim must be
/// visited exactly once. Route terminals replace their corresponding generic
/// route-conductor claim, while route floors remain separate claims. Adapters
/// validate representation-specific shape before exposing this view.
pub(crate) trait PhysicalCandidateView {
    fn visit_blocks(&self, visitor: &mut dyn FnMut(PhysicalBlockRef<'_>));
}

/// One exact write suppressed by a documented emission precedence rule.
///
/// Legacy routes record a floor under every route anchor. A declared output's
/// lamp may already occupy that same cell and is itself a valid conductive
/// floor, so the stone write is deliberately not applied. Keeping this record
/// makes the adapter lossless even though the finished world contains only
/// the lamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SuppressedWrite {
    pub at: Anchor,
    pub state: BlockState,
    pub role: PhysicalBlockRole,
    pub kept_role: PhysicalBlockRole,
}

#[derive(Debug, Clone)]
struct OwnedClaim {
    state: BlockState,
    role: PhysicalBlockRole,
}

/// A world plus the typed ownership evidence used to create it.
#[derive(Debug, Clone)]
pub(crate) struct EmittedWorld {
    pub world: World,
    owners: BTreeMap<Anchor, PhysicalBlockRole>,
    #[cfg_attr(not(test), allow(dead_code))]
    suppressed_writes: Vec<SuppressedWrite>,
}

impl EmittedWorld {
    pub fn owner_at(&self, at: Anchor) -> Option<PhysicalBlockRole> {
        self.owners.get(&at).copied()
    }

    pub fn owners(&self) -> impl Iterator<Item = (Anchor, PhysicalBlockRole)> + '_ {
        self.owners.iter().map(|(&at, &role)| (at, role))
    }

    #[cfg(test)]
    pub fn suppressed_writes(&self) -> &[SuppressedWrite] {
        &self.suppressed_writes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum EmissionError {
    #[error("world size {size:?} must be positive")]
    InvalidWorldSize { size: (i32, i32, i32) },
    #[error("{role:?} block at {at:?} is outside world size {size:?}")]
    BlockOutsideWorld {
        at: Anchor,
        size: (i32, i32, i32),
        role: PhysicalBlockRole,
    },
    #[error("cell {at:?} is claimed by both {first:?} and {second:?}")]
    DuplicatePhysicalClaim {
        at: Anchor,
        first: PhysicalBlockRole,
        second: PhysicalBlockRole,
    },
}

/// Emit exact candidate-owned states into a fresh world.
///
/// No gate shape, route strength, facing or delay is inferred here. Those
/// decisions have already happened and are carried by the view. This function
/// only validates the world boundary, resolves the one intentional
/// lamp-as-floor overlap, rejects all other duplicate ownership, and writes
/// the resulting ledger deterministically by coordinate.
pub(crate) fn emit_candidate(
    view: &dyn PhysicalCandidateView,
    size: (i32, i32, i32),
) -> Result<EmittedWorld, EmissionError> {
    if size.0 <= 0 || size.1 <= 0 || size.2 <= 0 {
        return Err(EmissionError::InvalidWorldSize { size });
    }

    let mut claims = BTreeMap::<Anchor, OwnedClaim>::new();
    let mut suppressed_writes = Vec::new();
    let mut emission_error = None;

    view.visit_blocks(&mut |block| {
        if emission_error.is_some() {
            return;
        }
        if outside(block.at, size) {
            emission_error = Some(EmissionError::BlockOutsideWorld {
                at: block.at,
                size,
                role: block.role,
            });
            return;
        }

        let incoming = OwnedClaim {
            state: block.state.clone(),
            role: block.role,
        };
        match claims.entry(block.at) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(incoming);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let existing = entry.get();
                if is_lamp_floor_overlap(existing, &incoming) {
                    suppressed_writes.push(SuppressedWrite {
                        at: block.at,
                        state: incoming.state,
                        role: incoming.role,
                        kept_role: existing.role,
                    });
                } else if is_lamp_floor_overlap(&incoming, existing) {
                    let displaced = entry.insert(incoming);
                    suppressed_writes.push(SuppressedWrite {
                        at: block.at,
                        state: displaced.state,
                        role: displaced.role,
                        kept_role: entry.get().role,
                    });
                } else {
                    emission_error = Some(EmissionError::DuplicatePhysicalClaim {
                        at: block.at,
                        first: existing.role,
                        second: incoming.role,
                    });
                }
            }
        }
    });

    if let Some(error) = emission_error {
        return Err(error);
    }

    let mut world = World::new(size.0, size.1, size.2);
    let mut owners = BTreeMap::new();
    for (at, claim) in claims {
        world.set(at.x, at.y, at.z, claim.state);
        owners.insert(at, claim.role);
    }

    Ok(EmittedWorld {
        world,
        owners,
        suppressed_writes,
    })
}

fn outside(at: Anchor, size: (i32, i32, i32)) -> bool {
    at.x < 0 || at.y < 0 || at.z < 0 || at.x >= size.0 || at.y >= size.1 || at.z >= size.2
}

fn is_lamp_floor_overlap(lamp: &OwnedClaim, floor: &OwnedClaim) -> bool {
    matches!(lamp.role, PhysicalBlockRole::DeclaredOutputLamp(_))
        && lamp.state.kind == BlockKind::Lamp
        && matches!(floor.role, PhysicalBlockRole::RouteFloor(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::identity::{
        ConnectionId, InstanceId, PhysicalEndpointId, PortId, PrimitiveId, RouteId, RoutedSinkId,
        TopologyNodeId,
    };
    use crate::compile::geometry::Anchor;
    use crate::redstone::world::block::{BlockKind, BlockState, Facing};

    #[derive(Default)]
    struct FixtureView {
        blocks: Vec<(Anchor, BlockState, PhysicalBlockRole)>,
    }

    impl PhysicalCandidateView for FixtureView {
        fn visit_blocks(&self, visitor: &mut dyn FnMut(PhysicalBlockRef<'_>)) {
            for (at, state, role) in &self.blocks {
                visitor(PhysicalBlockRef {
                    at: *at,
                    state,
                    role: *role,
                });
            }
        }
    }

    fn block(kind: BlockKind, name: &str) -> BlockState {
        let mut state = BlockState::air();
        state.kind = kind;
        state.name = name.to_string();
        state
    }

    fn primitive(instance: u32, node: u16) -> PrimitiveId {
        PrimitiveId {
            instance: InstanceId(instance),
            node: TopologyNodeId(node),
        }
    }

    #[test]
    fn emits_exact_terminal_and_floor_states_with_typed_ownership() {
        let floor_at = Anchor { x: 3, y: 0, z: 4 };
        let terminal_at = Anchor { x: 3, y: 1, z: 4 };
        let route = RouteId(7);
        let sink = RoutedSinkId { route, ordinal: 2 };
        let target = PhysicalEndpointId::Landing(ConnectionId::Internal {
            instance: InstanceId(9),
            edge_index: 1,
        });
        let floor = block(BlockKind::Solid, "minecraft:smooth_stone");
        let mut terminal = block(BlockKind::Repeater, "minecraft:repeater");
        terminal.facing = Some(Facing::East);
        terminal.delay = 4;
        terminal.power = 11;
        terminal
            .extra_properties
            .insert("locked".into(), "true".into());
        let terminal_role = PhysicalBlockRole::RouteTerminal {
            sink,
            target,
            kind: TerminalKind::RepeaterIntoSupport,
            repeaters: 3,
        };
        let view = FixtureView {
            blocks: vec![
                (
                    floor_at,
                    floor.clone(),
                    PhysicalBlockRole::RouteFloor(route),
                ),
                (terminal_at, terminal.clone(), terminal_role),
            ],
        };

        let emitted = emit_candidate(&view, (8, 4, 8)).expect("valid typed view must emit");

        assert_eq!(
            emitted.world.get(floor_at.x, floor_at.y, floor_at.z),
            &floor
        );
        assert_eq!(
            emitted
                .world
                .get(terminal_at.x, terminal_at.y, terminal_at.z),
            &terminal,
            "facing, delay, power, name and extra properties must survive emission"
        );
        assert_eq!(
            emitted.owner_at(floor_at),
            Some(PhysicalBlockRole::RouteFloor(route))
        );
        assert_eq!(emitted.owner_at(terminal_at), Some(terminal_role));
        assert!(emitted.suppressed_writes().is_empty());
    }

    #[test]
    fn route_floor_never_overwrites_a_declared_output_lamp() {
        let at = Anchor { x: 2, y: 1, z: 2 };
        let output = PhysicalEndpointId::DeclaredOutput(PortId(1));
        let lamp = block(BlockKind::Lamp, "minecraft:redstone_lamp");
        let floor = block(BlockKind::Solid, "minecraft:smooth_stone");
        let floor_role = PhysicalBlockRole::RouteFloor(RouteId(4));
        let view = FixtureView {
            blocks: vec![
                (
                    at,
                    lamp.clone(),
                    PhysicalBlockRole::DeclaredOutputLamp(output),
                ),
                (at, floor.clone(), floor_role),
            ],
        };

        let emitted = emit_candidate(&view, (6, 4, 6)).expect("lamp/floor overlap is intentional");

        assert_eq!(emitted.world.get(at.x, at.y, at.z), &lamp);
        assert_eq!(
            emitted.owner_at(at),
            Some(PhysicalBlockRole::DeclaredOutputLamp(output))
        );
        assert_eq!(
            emitted.suppressed_writes(),
            &[SuppressedWrite {
                at,
                state: floor,
                role: floor_role,
                kept_role: PhysicalBlockRole::DeclaredOutputLamp(output),
            }]
        );
    }

    #[test]
    fn duplicate_physical_claims_are_named_instead_of_last_write_winning() {
        let at = Anchor { x: 1, y: 1, z: 1 };
        let first = PhysicalBlockRole::Primitive(primitive(1, 0));
        let second = PhysicalBlockRole::Primitive(primitive(2, 0));
        let stone = block(BlockKind::Solid, "minecraft:stone");
        let view = FixtureView {
            blocks: vec![(at, stone.clone(), first), (at, stone, second)],
        };

        assert_eq!(
            emit_candidate(&view, (4, 4, 4)).unwrap_err(),
            EmissionError::DuplicatePhysicalClaim { at, first, second }
        );
    }

    #[test]
    fn out_of_bounds_blocks_are_refused_before_world_can_drop_them() {
        let at = Anchor { x: -1, y: 0, z: 0 };
        let role = PhysicalBlockRole::Boundary(PhysicalEndpointId::PrimaryInput(PortId(3)));
        let view = FixtureView {
            blocks: vec![(
                at,
                block(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
                role,
            )],
        };

        assert_eq!(
            emit_candidate(&view, (4, 4, 4)).unwrap_err(),
            EmissionError::BlockOutsideWorld {
                at,
                size: (4, 4, 4),
                role,
            }
        );
    }

    #[test]
    fn non_positive_world_dimensions_are_rejected() {
        assert_eq!(
            emit_candidate(&FixtureView::default(), (4, 0, 4)).unwrap_err(),
            EmissionError::InvalidWorldSize { size: (4, 0, 4) }
        );
    }
}

//! Shared physical input-terminal geometry for placement and seed materialisation.

use thiserror::Error;

use crate::compile::geometry::{self, Anchor, CellFacing};
use crate::compile::physical::{self, PortKind};
use crate::compile::topology::Primitive;
use crate::redstone::world::block::Facing;

/// The straight runway a terminal exposes past its anchor.
///
/// [`terminal_access_cells`] projects the anchor and this many further cells
/// along the terminal's own direction, and nothing else a parent route may
/// use; `terminal_access_is_exactly_the_guard_core_columns` holds the two
/// together. It is the number a
/// [`ForcedTerminalRunways`](crate::compile::routing::ForcedTerminalRunways)
/// contract for a packed terminal is built from, so the router's forced prefix
/// and the released access column are one fact rather than two.
pub(crate) const TERMINAL_RUNWAY_CELLS: u32 = 2;

/// Half-width of a terminal's own runway guard. Keep narrower than portal
/// pitch so adjacent terminals retain distinct anchors.
pub(crate) const TERMINAL_GUARD_HALF_WIDTH: i32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrimitiveInputTerminal {
    pub support: Anchor,
    pub terminal: Anchor,
    pub allowed_entry: Facing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrimitiveOutputTerminal {
    pub support: Anchor,
    pub route_anchor: Anchor,
    pub allowed_exit: Facing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum PrimitiveTerminalError {
    #[error("primitive {primitive:?} does not accept a routed input")]
    UnsupportedInput { primitive: Primitive },
    #[error("primitive {primitive:?} does not provide a routed output")]
    UnsupportedOutput { primitive: Primitive },
    #[error("primitive {primitive:?} has no physical input socket at ordinal {ordinal}")]
    InvalidInputOrdinal {
        primitive: Primitive,
        ordinal: usize,
    },
}

pub(crate) fn primitive_output_terminal(
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
) -> Result<PrimitiveOutputTerminal, PrimitiveTerminalError> {
    let variant = &physical::variants(primitive)[usize::from(facing.index())];
    let kind = match primitive {
        Primitive::Torch => PortKind::TorchOutput,
        Primitive::Repeater => PortKind::RepeaterFront,
        Primitive::Comparator | Primitive::Lamp | Primitive::Lever => {
            return Err(PrimitiveTerminalError::UnsupportedOutput { primitive });
        }
    };
    let port = variant.port(kind);
    let support = Anchor {
        x: anchor.x + port.position.x,
        y: anchor.y + port.position.y,
        z: anchor.z + port.position.z,
    };
    Ok(PrimitiveOutputTerminal {
        support,
        route_anchor: step(support, port.direction),
        allowed_exit: port.direction,
    })
}

pub(crate) fn source_escape_corridor(route_anchor: Anchor, allowed_exit: Facing) -> [Anchor; 3] {
    let exit = step(route_anchor, allowed_exit);
    let runway = step(exit, allowed_exit);
    let mouth = step(runway, allowed_exit);
    [exit, runway, mouth]
}

/// The terminal core and two-cell runway, projected from ground through `top`.
///
/// These are the only endpoint guard cells a selected parent route may release.
pub(crate) fn terminal_access_cells(anchor: Anchor, facing: Facing, top: i32) -> Vec<Anchor> {
    terminal_access_cells_from(anchor, facing, 0, top)
}

/// [`terminal_access_cells`] with an explicit local floor. A packed child uses
/// this before translation so its halo minimum lands on parent `y = 0`.
pub(crate) fn terminal_access_cells_from(
    anchor: Anchor,
    facing: Facing,
    bottom: i32,
    top: i32,
) -> Vec<Anchor> {
    if top < bottom {
        return Vec::new();
    }
    runway_core(anchor, facing)
        .into_iter()
        .flat_map(|core| (bottom..=top).map(move |y| Anchor { y, ..core }))
        .collect()
}

/// The anchor and its [`TERMINAL_RUNWAY_CELLS`] runway, on the anchor's own
/// layer. The single definition both the access columns and the router's
/// forced-runway contract are derived from.
pub(crate) fn runway_core(anchor: Anchor, facing: Facing) -> Vec<Anchor> {
    let mut core = vec![anchor];
    for _ in 0..TERMINAL_RUNWAY_CELLS {
        core.push(step(
            *core.last().expect("the core starts at its anchor"),
            facing,
        ));
    }
    core
}

/// A packed terminal's egress path: the cells its route stands on from the
/// end of the forced runway up to height `top`, in order.
///
/// `path[0]` is the mouth -- the cell past the runway's last core cell, which
/// is also the third cell of [`source_escape_corridor`]: that corridor is
/// `[core[1], core[2], path[0]]`, one cell further along than [`runway_core`]
/// because it starts at the exit rather than the anchor. Every later cell is
/// one step further along `facing` and one higher: the straight staircase
/// the router climbs, since it moves one cell sideways with every cell of
/// height. At `top <= anchor.y` the path is the mouth alone.
///
/// A canonical path, not the only legal one: the route may turn wherever the
/// runway lets it. It is the one path the parent holds for the terminal so
/// that a way to its lane exists whatever earlier trunks laid.
pub(crate) fn terminal_egress_path(anchor: Anchor, facing: Facing, top: i32) -> Vec<Anchor> {
    let last = *runway_core(anchor, facing)
        .last()
        .expect("the core starts at its anchor");
    let mut path = vec![step(last, facing)];
    while path.last().expect("the path starts at the mouth").y < top {
        let previous = *path.last().expect("the path starts at the mouth");
        let next = step(previous, facing);
        path.push(Anchor {
            y: previous.y + 1,
            ..next
        });
    }
    path
}

/// The cells the router needs clear of *every* foreign claim to climb the
/// egress path: for each riser, the cell under the next step and the cell
/// over the current one, which `staircase_clearance_typed` checks and a
/// keep-out of any owner blocks. Not path cells, and not coupling cells.
pub(crate) fn terminal_egress_clearance(anchor: Anchor, facing: Facing, top: i32) -> Vec<Anchor> {
    let path = terminal_egress_path(anchor, facing, top);
    let mut cells = Vec::new();
    for pair in path.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        cells.push(Anchor { y: from.y, ..to });
        cells.push(Anchor {
            y: from.y + 1,
            ..from
        });
    }
    cells.retain(|at| !path.contains(at));
    cells.sort();
    cells.dedup();
    cells
}

/// The three mouth cells: the first egress cell and the cells above and
/// below it, the forward slice of the coupling ball around the runway's last
/// core cell. The route must enter one of them, so a foreign conductor in
/// any of them seals the terminal.
pub(crate) fn terminal_mouth_ring(anchor: Anchor, facing: Facing) -> [Anchor; 3] {
    let mouth = terminal_egress_path(anchor, facing, anchor.y)[0];
    [
        Anchor {
            y: mouth.y - 1,
            ..mouth
        },
        mouth,
        Anchor {
            y: mouth.y + 1,
            ..mouth
        },
    ]
}

/// The coupling closure of a packed terminal's declared egress: every cell
/// the router's coupling ball (`keep_out_typed`) claims around any cell of
/// the runway core, the mouth ring or the egress path to `top`, minus those
/// cells themselves.
///
/// A foreign *conductor* anywhere in this set makes some cell of the path
/// unenterable; a keep-out does not. So the closure needs only to be free of
/// conductors, which is what an endpoint-owned keep-out on each free cell of
/// it secures: an earlier trunk cannot lay dust there, and the owning trunk
/// may still cross it. The router's own arithmetic is reused rather than
/// restated, so the closure cannot drift from what the search checks.
pub(crate) fn terminal_egress_closure(
    anchor: Anchor,
    facing: Facing,
    top: i32,
) -> std::collections::BTreeSet<Anchor> {
    use crate::compile::routing::keep_out_typed;

    let mut own = runway_core(anchor, facing);
    own.extend(terminal_mouth_ring(anchor, facing));
    own.extend(terminal_egress_path(anchor, facing, top));
    let own = own.into_iter().collect::<std::collections::BTreeSet<_>>();
    own.iter()
        .flat_map(|at| keep_out_typed(*at))
        .filter(|at| !own.contains(at))
        .collect()
}

/// Parent-owned keep-out for a terminal and its two-cell runway.
///
/// Superset of [`terminal_access_cells`]. Lateral columns and the `+/-Y`
/// coupling ring stay keep-out-only.
pub(crate) fn terminal_guard_cells(anchor: Anchor, facing: Facing, top: i32) -> Vec<Anchor> {
    terminal_guard_cells_from(anchor, facing, 0, top)
}

/// [`terminal_guard_cells`] with an explicit local floor. The leaf artifact
/// uses this before packing; the parent uses the zero-floor wrapper.
pub(crate) fn terminal_guard_cells_from(
    anchor: Anchor,
    facing: Facing,
    bottom: i32,
    top: i32,
) -> Vec<Anchor> {
    if top < bottom {
        return Vec::new();
    }
    let exit = step(anchor, facing);
    let across = match facing {
        Facing::North | Facing::South => Facing::East,
        Facing::East | Facing::West => Facing::South,
        Facing::Up | Facing::Down => Facing::East,
    };
    let slide = |at: Anchor, offset: i32| match across {
        Facing::East => Anchor {
            x: at.x + offset,
            ..at
        },
        Facing::West => Anchor {
            x: at.x - offset,
            ..at
        },
        Facing::South => Anchor {
            z: at.z + offset,
            ..at
        },
        Facing::North => Anchor {
            z: at.z - offset,
            ..at
        },
        Facing::Up | Facing::Down => unreachable!("across is horizontal"),
    };
    let mut cells = terminal_access_cells_from(anchor, facing, bottom, top);
    for core in runway_core(anchor, facing) {
        for offset in -TERMINAL_GUARD_HALF_WIDTH..=TERMINAL_GUARD_HALF_WIDTH {
            if offset == 0 {
                continue;
            }
            let side = slide(core, offset);
            for y in bottom..=top {
                cells.push(Anchor { y, ..side });
            }
        }
    }
    for core in [anchor, exit] {
        for side_facing in [Facing::North, Facing::South, Facing::East, Facing::West] {
            let side = step(core, side_facing);
            for dy in [-1, 0, 1] {
                cells.push(Anchor {
                    y: side.y + dy,
                    ..side
                });
            }
        }
    }
    cells.retain(|at| at.y >= bottom);
    cells
}

pub(crate) fn primitive_input_terminal(
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
    ordinal: usize,
) -> Result<PrimitiveInputTerminal, PrimitiveTerminalError> {
    let variant = &physical::variants(primitive)[usize::from(facing.index())];
    let (port, direction) = match primitive {
        Primitive::Torch => {
            let direction = geometry::input_directions(facing)
                .get(ordinal)
                .copied()
                .ok_or(PrimitiveTerminalError::InvalidInputOrdinal { primitive, ordinal })?;
            (variant.port(PortKind::TorchInput), direction)
        }
        Primitive::Repeater if ordinal == 0 => {
            let port = variant.port(PortKind::RepeaterRear);
            (port, port.direction)
        }
        Primitive::Repeater => {
            return Err(PrimitiveTerminalError::InvalidInputOrdinal { primitive, ordinal });
        }
        Primitive::Comparator | Primitive::Lamp | Primitive::Lever => {
            return Err(PrimitiveTerminalError::UnsupportedInput { primitive });
        }
    };
    let support = Anchor {
        x: anchor.x + port.position.x,
        y: anchor.y + port.position.y,
        z: anchor.z + port.position.z,
    };
    Ok(PrimitiveInputTerminal {
        support,
        terminal: step(support, direction),
        allowed_entry: direction,
    })
}

fn step(at: Anchor, direction: Facing) -> Anchor {
    match direction {
        Facing::North => Anchor { z: at.z - 1, ..at },
        Facing::South => Anchor { z: at.z + 1, ..at },
        Facing::East => Anchor { x: at.x + 1, ..at },
        Facing::West => Anchor { x: at.x - 1, ..at },
        Facing::Up => Anchor { y: at.y + 1, ..at },
        Facing::Down => Anchor { y: at.y - 1, ..at },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torch_ordinals_match_materialised_input_direction_order() {
        let anchor = Anchor { x: 4, y: 2, z: 7 };
        let terminals = (0..3)
            .map(|ordinal| {
                primitive_input_terminal(Primitive::Torch, CellFacing::NORTH, anchor, ordinal)
                    .unwrap()
                    .terminal
            })
            .collect::<Vec<_>>();

        assert_eq!(
            terminals,
            [
                Anchor { x: 3, y: 2, z: 7 },
                Anchor { x: 5, y: 2, z: 7 },
                Anchor { x: 4, y: 2, z: 8 },
            ]
        );
    }

    #[test]
    fn impossible_input_ordinals_are_rejected_instead_of_falling_back() {
        let anchor = Anchor { x: 0, y: 1, z: 0 };
        assert_eq!(
            primitive_input_terminal(Primitive::Torch, CellFacing::NORTH, anchor, 3),
            Err(PrimitiveTerminalError::InvalidInputOrdinal {
                primitive: Primitive::Torch,
                ordinal: 3,
            })
        );
        assert_eq!(
            primitive_input_terminal(Primitive::Repeater, CellFacing::EAST, anchor, 1),
            Err(PrimitiveTerminalError::InvalidInputOrdinal {
                primitive: Primitive::Repeater,
                ordinal: 1,
            })
        );
    }

    #[test]
    fn output_escape_corridor_follows_the_physical_port_direction() {
        let anchor = Anchor { x: 4, y: 2, z: 7 };
        let output =
            primitive_output_terminal(Primitive::Torch, CellFacing::NORTH, anchor).unwrap();

        // The torch at z=7 drives the block at z=6; the route's first dust
        // sits past that block at z=5 and the corridor runs on from there.
        assert_eq!(output.support, Anchor { x: 4, y: 2, z: 6 });
        assert_eq!(output.route_anchor, Anchor { x: 4, y: 2, z: 5 });
        assert_eq!(output.allowed_exit, Facing::North);
        assert_eq!(
            source_escape_corridor(output.route_anchor, output.allowed_exit),
            [
                Anchor { x: 4, y: 2, z: 4 },
                Anchor { x: 4, y: 2, z: 3 },
                Anchor { x: 4, y: 2, z: 2 },
            ]
        );
    }

    #[test]
    fn the_runway_constant_is_the_access_column_count() {
        let anchor = Anchor { x: 5, y: 2, z: 7 };
        let core = terminal_access_cells_from(anchor, Facing::East, anchor.y, anchor.y);

        // The router's forced runway is built from this constant, so a change
        // to the exposed column count that did not change it would hand the
        // router a contract the access cells do not back.
        assert_eq!(core.len() as u32, TERMINAL_RUNWAY_CELLS + 1);
        assert_eq!(core, runway_core(anchor, Facing::East));
        assert_eq!(
            core,
            vec![
                Anchor { x: 5, y: 2, z: 7 },
                Anchor { x: 6, y: 2, z: 7 },
                Anchor { x: 7, y: 2, z: 7 },
            ]
        );
    }

    /// The egress path starts one past the runway -- the corridor's third
    /// cell, since `source_escape_corridor` starts at the exit and
    /// `runway_core` at the anchor -- and climbs one cell per step along the
    /// facing until it stands at `top`.
    #[test]
    fn the_egress_path_starts_past_the_runway_and_climbs_to_its_lane() {
        for facing in [Facing::East, Facing::West, Facing::North, Facing::South] {
            let anchor = Anchor { x: 9, y: 3, z: 9 };
            let core = runway_core(anchor, facing);
            let corridor = source_escape_corridor(anchor, facing);
            let path = terminal_egress_path(anchor, facing, 7);

            // The off-by-one, stated: corridor = [core[1], core[2], path[0]].
            assert_eq!(corridor[0], core[1], "{facing:?}");
            assert_eq!(corridor[1], core[2], "{facing:?}");
            assert_eq!(corridor[2], path[0], "{facing:?}");
            assert_eq!(path[0], step(core[2], facing));
            assert_eq!(path[0].y, anchor.y);

            // One step along and one up, every cell, ending exactly at `top`.
            assert_eq!(path.len(), 5, "{facing:?}");
            for pair in path.windows(2) {
                assert_eq!(
                    pair[1],
                    Anchor {
                        y: pair[0].y + 1,
                        ..step(pair[0], facing)
                    }
                );
            }
            assert_eq!(path.last().unwrap().y, 7);
            // No climb wanted: the mouth alone.
            assert_eq!(terminal_egress_path(anchor, facing, 3), vec![path[0]]);
            assert_eq!(terminal_egress_path(anchor, facing, 0), vec![path[0]]);
            // The runway is not lengthened by any of it.
            assert_eq!(core.len() as u32, TERMINAL_RUNWAY_CELLS + 1);
            for at in &path {
                assert!(!core.contains(at));
            }
        }
    }

    /// The staircase clearance is exactly what `staircase_clearance_typed`
    /// asks for at each climb: the cell under the next step and the cell over
    /// the current one, and never a path cell.
    #[test]
    fn the_egress_clearance_is_the_riser_and_the_headroom_of_each_climb() {
        let anchor = Anchor { x: 9, y: 3, z: 9 };
        let path = terminal_egress_path(anchor, Facing::East, 5);
        assert_eq!(
            path,
            vec![
                Anchor { x: 12, y: 3, z: 9 },
                Anchor { x: 13, y: 4, z: 9 },
                Anchor { x: 14, y: 5, z: 9 },
            ]
        );
        let clearance = terminal_egress_clearance(anchor, Facing::East, 5);
        assert_eq!(
            clearance,
            vec![
                Anchor { x: 12, y: 4, z: 9 },
                Anchor { x: 13, y: 3, z: 9 },
                Anchor { x: 13, y: 5, z: 9 },
                Anchor { x: 14, y: 4, z: 9 },
            ]
        );
        assert!(terminal_egress_clearance(anchor, Facing::East, 3).is_empty());
    }

    /// The closure is exactly the coupling closure of the declared egress:
    /// the union of the router's own `keep_out_typed` over every core, mouth
    /// and path cell, minus those cells. The mouth ring is the slice of it
    /// that the runway alone needs, and lies on the path column.
    #[test]
    fn the_egress_closure_is_the_coupling_closure_of_the_declared_egress() {
        use crate::compile::routing::keep_out_typed;
        use std::collections::BTreeSet;

        for facing in [Facing::East, Facing::West, Facing::North, Facing::South] {
            let anchor = Anchor { x: 9, y: 3, z: 9 };
            let top = 6;
            let core = runway_core(anchor, facing);
            let ring = terminal_mouth_ring(anchor, facing);
            let path = terminal_egress_path(anchor, facing, top);
            let own = core
                .iter()
                .chain(ring.iter())
                .chain(path.iter())
                .copied()
                .collect::<BTreeSet<_>>();
            let expected = own
                .iter()
                .flat_map(|at| keep_out_typed(*at))
                .filter(|at| !own.contains(at))
                .collect::<BTreeSet<_>>();
            let closure = terminal_egress_closure(anchor, facing, top);
            assert_eq!(closure, expected, "{facing:?}");
            // It excludes cells the endpoint path itself occupies. It may
            // overlap another height of vertical access, protected separately.
            assert!(closure.is_disjoint(&own), "{facing:?}");
            assert!(!closure.is_empty());

            // The ring: the forward slice of the ball around the last core
            // cell, on the mouth's own column, three cells, and the middle
            // one is the path's first cell.
            let last = *core.last().unwrap();
            let forward = keep_out_typed(last)
                .into_iter()
                .filter(|at| at.x == path[0].x && at.z == path[0].z)
                .collect::<BTreeSet<_>>();
            assert_eq!(ring.iter().copied().collect::<BTreeSet<_>>(), forward);
            assert_eq!(ring[1], path[0]);
        }
    }

    #[test]
    fn terminal_access_is_exactly_the_guard_core_columns() {
        let anchor = Anchor { x: 5, y: 2, z: 7 };
        let access = terminal_access_cells(anchor, Facing::East, 4);
        let guard = terminal_guard_cells(anchor, Facing::East, 4);

        assert_eq!(access.len(), 15);
        assert!(access.iter().all(|at| guard.contains(at)));
        assert!(!access.contains(&Anchor { x: 5, y: 4, z: 8 }));
        assert!(guard.contains(&Anchor { x: 5, y: 4, z: 8 }));
    }
}

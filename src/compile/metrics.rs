use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::compile::geometry::Anchor;
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Fingerprint(String);

impl Fingerprint {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ratio {
    pub numerator: u64,
    pub denominator: u64,
}

impl Ratio {
    pub const fn new(numerator: u64, denominator: u64) -> Self {
        assert!(denominator > 0, "a ratio denominator must be non-zero");
        Ratio {
            numerator,
            denominator,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalMetrics {
    pub non_air_blocks: u64,
    pub occupied_min: Option<Anchor>,
    pub occupied_max: Option<Anchor>,
    pub occupied_volume: u64,
    pub blocks_per_lowered_gate: Ratio,
}

/// SHA-256 over exactly `bytes`, rendered as lowercase hexadecimal.
pub fn canonical_fingerprint(bytes: &[u8]) -> Fingerprint {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Fingerprint(hex)
}

/// Canonical physical size metrics over non-air cells only.
pub fn physical_metrics(world: &World, lowered_gate_count: u64) -> PhysicalMetrics {
    let mut non_air_blocks = 0u64;
    let mut occupied_min: Option<Anchor> = None;
    let mut occupied_max: Option<Anchor> = None;

    for (flat, &palette_index) in world.cells().iter().enumerate() {
        let state = world
            .palette()
            .get(palette_index)
            .expect("a world cell must reference an existing palette entry");
        if state.kind == BlockKind::Air {
            continue;
        }

        non_air_blocks += 1;
        let (x, y, z) = world.decode(flat);
        let anchor = Anchor { x, y, z };
        occupied_min = Some(match occupied_min {
            Some(min) => Anchor {
                x: min.x.min(x),
                y: min.y.min(y),
                z: min.z.min(z),
            },
            None => anchor,
        });
        occupied_max = Some(match occupied_max {
            Some(max) => Anchor {
                x: max.x.max(x),
                y: max.y.max(y),
                z: max.z.max(z),
            },
            None => anchor,
        });
    }

    let occupied_volume = occupied_min
        .zip(occupied_max)
        .map(|(min, max)| {
            let x = (i64::from(max.x) - i64::from(min.x) + 1) as u64;
            let y = (i64::from(max.y) - i64::from(min.y) + 1) as u64;
            let z = (i64::from(max.z) - i64::from(min.z) + 1) as u64;
            x * y * z
        })
        .unwrap_or(0);

    PhysicalMetrics {
        non_air_blocks,
        occupied_min,
        occupied_max,
        occupied_volume,
        blocks_per_lowered_gate: Ratio::new(non_air_blocks, lowered_gate_count.max(1)),
    }
}

#[cfg(test)]
mod tests {
    use super::{canonical_fingerprint, physical_metrics, Ratio};
    use crate::compile::{geometry, planner};
    use crate::redstone::world::block::{BlockKind, BlockState};
    use crate::redstone::world::storage::World;

    fn solid() -> BlockState {
        let mut block = BlockState::air();
        block.kind = BlockKind::Solid;
        block.name = "minecraft:stone".to_string();
        block
    }

    fn place_fixture(world: &mut World) {
        for (x, y, z) in [(2, 1, 3), (4, 1, 3), (4, 2, 5)] {
            world.set(x, y, z, solid());
        }
    }

    #[test]
    fn physical_metrics_measure_occupied_cells_not_allocated_world_size() {
        let mut padded = World::new(20, 8, 20);
        place_fixture(&mut padded);

        let metrics = physical_metrics(&padded, 2);
        assert_eq!(metrics.non_air_blocks, 3);
        assert_eq!(
            metrics.occupied_min,
            Some(geometry::Anchor { x: 2, y: 1, z: 3 })
        );
        assert_eq!(
            metrics.occupied_max,
            Some(geometry::Anchor { x: 4, y: 2, z: 5 })
        );
        assert_eq!(metrics.occupied_volume, 18);
        assert_eq!(metrics.blocks_per_lowered_gate, Ratio::new(3, 2));

        let mut tightly_sized = World::new(5, 3, 6);
        place_fixture(&mut tightly_sized);
        assert_eq!(
            physical_metrics(&padded, 2),
            physical_metrics(&tightly_sized, 2)
        );
    }

    #[test]
    fn physical_metrics_leave_an_empty_world_unbounded() {
        let metrics = physical_metrics(&World::new(20, 8, 20), 0);

        assert_eq!(metrics.non_air_blocks, 0);
        assert_eq!(metrics.occupied_min, None);
        assert_eq!(metrics.occupied_max, None);
        assert_eq!(metrics.occupied_volume, 0);
        assert_eq!(metrics.blocks_per_lowered_gate, Ratio::new(0, 1));
    }

    #[test]
    fn canonical_fingerprints_are_repeatable_and_byte_sensitive() {
        assert_eq!(
            canonical_fingerprint(b"reda"),
            canonical_fingerprint(b"reda")
        );
        assert_ne!(
            canonical_fingerprint(b"reda"),
            canonical_fingerprint(b"REDA")
        );
    }

    #[test]
    fn planner_anchor_is_the_geometry_anchor_type() {
        let durable = geometry::Anchor { x: 2, y: 1, z: 3 };
        let compatible: planner::Anchor = durable;

        assert_eq!(compatible, durable);
    }
}

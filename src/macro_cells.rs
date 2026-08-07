//! Macro cell library: loading pre-built `.litematic` geometry plus its pin
//! manifest, the way an ASIC flow consumes a LEF file.
//!
//! See `docs/superpowers/specs/2026-08-07-macro-cells.md` for the full
//! rationale. In short: a netlist (`compile::Netlist`) never carries
//! coordinates, but a macro like a seven-segment display *is* its layout --
//! the geometry has to come from somewhere. That somewhere is a directory:
//!
//! ```text
//! cells/<cell_name>/
//!   cell.litematic    the geometry
//!   cell.toml         the pin manifest
//! ```
//!
//! This module only *reads* that directory and validates it. Placement
//! (`Floorplan`) and routing a pin into the rest of a circuit are separate,
//! later pieces of work.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::formats::litematic;
use crate::formats::nbt::FormatError;
use crate::redstone::rules::taxonomy::flags_of;
use crate::redstone::world::block::Facing;
use crate::redstone::world::storage::World;

// ---------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------

/// Which way a pin's wire is meant to flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PinDirection {
    Input,
    Output,
}

/// One named connection point on a cell's outline.
///
/// `at` is cell-local, origin at the `.litematic`'s corner (same convention
/// `World` itself uses). `side` names which face of the cell's bounding box
/// the wire approaches from; a loaded `Pin` is guaranteed to have `at` lying
/// on that face's boundary plane (see [`load_cell`]'s validation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub at: (i32, i32, i32),
    pub side: Facing,
    pub direction: PinDirection,
}

/// A loaded macro cell: geometry plus the pins that name its connection
/// points. The compiler treats this as an opaque box -- it never inspects
/// `world` for meaning, only for the blocks sitting at each pin.
#[derive(Debug, Clone)]
pub struct Cell {
    pub name: String,
    pub world: World,
    pub pins: Vec<Pin>,
}

/// A named collection of cells, keyed by [`Cell::name`] (the manifest's
/// `name` field, not the directory name).
pub type CellLibrary = HashMap<String, Cell>;

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// Everything that can go wrong loading a cell or a library of cells.
///
/// Every validation failure names the offending pin so a caller can tell
/// e.g. "pin `a` is not on the south face" apart from "pin `a` is out of
/// bounds" without parsing prose.
#[derive(Debug, thiserror::Error)]
pub enum CellError {
    #[error("failed to read cell manifest {path}: {source}")]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse cell manifest {path}: {source}")]
    ParseManifest {
        path: PathBuf,
        // Boxed: `toml::de::Error` alone is >100 bytes, which would make
        // every `Result<_, CellError>` pay that cost even on the `Ok` path.
        #[source]
        source: Box<toml::de::Error>,
    },

    #[error("failed to load cell geometry {path}: {source}")]
    LoadGeometry {
        path: PathBuf,
        #[source]
        source: FormatError,
    },

    #[error(
        "cell '{cell}' manifest declares size {manifest_size:?} but its \
         litematic geometry is {litematic_size:?}"
    )]
    SizeMismatch {
        cell: String,
        manifest_size: (i32, i32, i32),
        litematic_size: (i32, i32, i32),
    },

    #[error("pin '{pin}' at {at:?} is out of bounds for cell size {size:?}")]
    PinOutOfBounds {
        pin: String,
        at: (i32, i32, i32),
        size: (i32, i32, i32),
    },

    #[error("pin '{pin}' at {at:?} is not on the {side:?} face")]
    PinNotOnFace {
        pin: String,
        at: (i32, i32, i32),
        side: Facing,
    },

    #[error(
        "pin '{pin}' at {at:?} sits on block '{block_name}', which the \
         router cannot legally connect to (it does not conduct redstone)"
    )]
    PinBlockNotConnectable {
        pin: String,
        at: (i32, i32, i32),
        block_name: String,
    },

    #[error("pin name '{name}' is used by more than one pin in this cell")]
    DuplicatePinName { name: String },

    #[error("pins '{first}' and '{second}' both sit at {at:?}")]
    DuplicatePinCoordinate {
        first: String,
        second: String,
        at: (i32, i32, i32),
    },

    #[error("failed to read cell library directory {path}: {source}")]
    ReadLibraryDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------
// Manifest schema (private: callers only ever see `Cell` and `Pin`)
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CellManifest {
    name: String,
    size: (i32, i32, i32),
    #[serde(default)]
    pins: Vec<PinManifest>,
}

#[derive(Debug, Deserialize)]
struct PinManifest {
    name: String,
    at: (i32, i32, i32),
    #[serde(deserialize_with = "deserialize_side")]
    side: Facing,
    direction: PinDirection,
}

/// Deserialise a pin's `side` from the same lower-case strings the manifest
/// spells `direction` with, rather than `Facing`'s derive-free `Debug`
/// spelling -- `Facing` has no `Deserialize` impl of its own since it lives
/// in `redstone::world::block`, which this module does not touch.
fn deserialize_side<'de, D>(deserializer: D) -> Result<Facing, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    match raw.as_str() {
        "north" => Ok(Facing::North),
        "south" => Ok(Facing::South),
        "east" => Ok(Facing::East),
        "west" => Ok(Facing::West),
        "up" => Ok(Facing::Up),
        "down" => Ok(Facing::Down),
        other => Err(serde::de::Error::custom(format!(
            "unknown side '{other}' (expected one of: north, south, east, west, up, down)"
        ))),
    }
}

// ---------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------

const MANIFEST_FILE_NAME: &str = "cell.toml";
const GEOMETRY_FILE_NAME: &str = "cell.litematic";

/// Load one cell from `dir` (expected to contain `cell.toml` and
/// `cell.litematic`), running every validation the cell format's spec
/// requires before returning it.
///
/// A cell that fails validation is rejected here, at load time, rather than
/// surfacing as a confusing failure deep inside a later placement or routing
/// pass.
pub fn load_cell(dir: &Path) -> Result<Cell, CellError> {
    let manifest_path = dir.join(MANIFEST_FILE_NAME);
    let geometry_path = dir.join(GEOMETRY_FILE_NAME);

    let manifest_text =
        fs::read_to_string(&manifest_path).map_err(|source| CellError::ReadManifest {
            path: manifest_path.clone(),
            source,
        })?;
    let manifest: CellManifest =
        toml::from_str(&manifest_text).map_err(|source| CellError::ParseManifest {
            path: manifest_path.clone(),
            source: Box::new(source),
        })?;

    let world = litematic::load(&geometry_path).map_err(|source| CellError::LoadGeometry {
        path: geometry_path.clone(),
        source,
    })?;

    let litematic_size = world.size();
    if manifest.size != litematic_size {
        return Err(CellError::SizeMismatch {
            cell: manifest.name,
            manifest_size: manifest.size,
            litematic_size,
        });
    }
    let size = litematic_size;

    let mut pins = Vec::with_capacity(manifest.pins.len());
    for raw in manifest.pins {
        validate_in_bounds(&raw.name, raw.at, size)?;
        validate_on_face(&raw.name, raw.at, raw.side, size)?;
        validate_connectable(&raw.name, raw.at, &world)?;
        pins.push(Pin {
            name: raw.name,
            at: raw.at,
            side: raw.side,
            direction: raw.direction,
        });
    }

    let mut names_seen: HashSet<&str> = HashSet::new();
    for pin in &pins {
        if !names_seen.insert(pin.name.as_str()) {
            return Err(CellError::DuplicatePinName {
                name: pin.name.clone(),
            });
        }
    }

    let mut coords_seen: HashMap<(i32, i32, i32), &str> = HashMap::new();
    for pin in &pins {
        if let Some(&first) = coords_seen.get(&pin.at) {
            return Err(CellError::DuplicatePinCoordinate {
                first: first.to_string(),
                second: pin.name.clone(),
                at: pin.at,
            });
        }
        coords_seen.insert(pin.at, pin.name.as_str());
    }

    Ok(Cell {
        name: manifest.name,
        world,
        pins,
    })
}

/// Load every cell found directly under `dir` (one subdirectory per cell),
/// keyed by each cell's manifest `name`. This is the one call a caller needs
/// to bring in a whole library rather than one cell at a time.
pub fn load_library(dir: &Path) -> Result<CellLibrary, CellError> {
    let entries = fs::read_dir(dir).map_err(|source| CellError::ReadLibraryDir {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut library = CellLibrary::new();
    for entry in entries {
        let entry = entry.map_err(|source| CellError::ReadLibraryDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            let cell = load_cell(&path)?;
            library.insert(cell.name.clone(), cell);
        }
    }
    Ok(library)
}

// ---------------------------------------------------------------------
// Per-pin validation
// ---------------------------------------------------------------------

fn validate_in_bounds(
    pin: &str,
    at: (i32, i32, i32),
    size: (i32, i32, i32),
) -> Result<(), CellError> {
    let (x, y, z) = at;
    let (sx, sy, sz) = size;
    if x < 0 || y < 0 || z < 0 || x >= sx || y >= sy || z >= sz {
        return Err(CellError::PinOutOfBounds {
            pin: pin.to_string(),
            at,
            size,
        });
    }
    Ok(())
}

/// `side` must name the face whose boundary plane `at` actually lies on.
/// The mapping mirrors `Position::offset` (`redstone::simulator::position`):
/// north/south move along -Z/+Z, east/west along +X/-X, up/down along +Y/-Y.
fn validate_on_face(
    pin: &str,
    at: (i32, i32, i32),
    side: Facing,
    size: (i32, i32, i32),
) -> Result<(), CellError> {
    let (x, y, z) = at;
    let (sx, sy, sz) = size;
    let on_face = match side {
        Facing::North => z == 0,
        Facing::South => z == sz - 1,
        Facing::West => x == 0,
        Facing::East => x == sx - 1,
        Facing::Down => y == 0,
        Facing::Up => y == sy - 1,
    };
    if !on_face {
        return Err(CellError::PinNotOnFace {
            pin: pin.to_string(),
            at,
            side,
        });
    }
    Ok(())
}

/// The block a pin sits on must be something the router could plausibly
/// terminate a wire in: the router closes every net in a repeater facing the
/// pin's block, which only forwards a signal if the block conducts redstone
/// (`BlockFlags::CONDUCTIVE`). Air, glass, and similar non-conductive blocks
/// would silently swallow the connection during routing rather than failing
/// here, at load time, where the diagnostic can actually name the pin.
fn validate_connectable(pin: &str, at: (i32, i32, i32), world: &World) -> Result<(), CellError> {
    let (x, y, z) = at;
    let block = world.get(x, y, z);
    if !flags_of(block).is_conductive() {
        return Err(CellError::PinBlockNotConnectable {
            pin: pin.to_string(),
            at,
            block_name: block.name.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState};

    fn stone() -> BlockState {
        let mut state = BlockState::air();
        state.kind = BlockKind::Solid;
        state.name = "minecraft:stone".to_string();
        state
    }

    /// A unique-per-test scratch directory under the system temp dir, wiped
    /// clean before use so a previous failed run can't leak state in.
    fn scratch_dir(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reda_macro_cell_test_{test_name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn pin_toml(name: &str, at: (i32, i32, i32), side: &str, direction: &str) -> String {
        format!(
            "\n[[pins]]\nname = \"{name}\"\nat = [{}, {}, {}]\nside = \"{side}\"\ndirection = \"{direction}\"\n",
            at.0, at.1, at.2
        )
    }

    fn manifest_toml(cell_name: &str, size: (i32, i32, i32), pins: &str) -> String {
        format!(
            "name = \"{cell_name}\"\nsize = [{}, {}, {}]\n{pins}",
            size.0, size.1, size.2
        )
    }

    /// Write `world` and `manifest_text` as a cell fixture at `dir`.
    fn write_fixture(dir: &Path, cell_name: &str, world: &World, manifest_text: &str) {
        fs::create_dir_all(dir).expect("create cell dir");
        litematic::save(&dir.join(GEOMETRY_FILE_NAME), world, cell_name)
            .expect("save litematic fixture");
        fs::write(dir.join(MANIFEST_FILE_NAME), manifest_text).expect("write manifest fixture");
    }

    #[test]
    fn loads_a_valid_cell_end_to_end() {
        let dir = scratch_dir("valid_cell");

        let mut world = World::new(3, 3, 3);
        world.set(2, 1, 1, stone()); // east face, side "east"
        world.set(0, 1, 1, stone()); // west face, side "west"

        let mut pins_toml = String::new();
        pins_toml.push_str(&pin_toml("a", (2, 1, 1), "east", "output"));
        pins_toml.push_str(&pin_toml("b", (0, 1, 1), "west", "input"));
        let manifest = manifest_toml("valid_cell", (3, 3, 3), &pins_toml);

        write_fixture(&dir, "valid_cell", &world, &manifest);

        let cell = load_cell(&dir).expect("a well-formed cell must load");

        assert_eq!(cell.name, "valid_cell");
        assert_eq!(cell.world.size(), (3, 3, 3));
        assert_eq!(cell.pins.len(), 2);

        let a = cell.pins.iter().find(|p| p.name == "a").expect("pin a");
        assert_eq!(a.at, (2, 1, 1));
        assert_eq!(a.side, Facing::East);
        assert_eq!(a.direction, PinDirection::Output);

        let b = cell.pins.iter().find(|p| p.name == "b").expect("pin b");
        assert_eq!(b.at, (0, 1, 1));
        assert_eq!(b.side, Facing::West);
        assert_eq!(b.direction, PinDirection::Input);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_pin_out_of_bounds() {
        let dir = scratch_dir("pin_out_of_bounds");

        let world = World::new(3, 3, 3);
        let pins_toml = pin_toml("a", (5, 0, 0), "east", "input");
        let manifest = manifest_toml("bad_cell", (3, 3, 3), &pins_toml);
        write_fixture(&dir, "bad_cell", &world, &manifest);

        let err = load_cell(&dir).expect_err("an out-of-bounds pin must be rejected");
        match err {
            CellError::PinOutOfBounds { pin, at, size } => {
                assert_eq!(pin, "a");
                assert_eq!(at, (5, 0, 0));
                assert_eq!(size, (3, 3, 3));
            }
            other => panic!("expected PinOutOfBounds, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_pin_not_on_its_declared_face() {
        let dir = scratch_dir("pin_not_on_face");

        // In-bounds, but z = 1 is the middle of a 3-deep cell -- not the
        // south face (z = size.z - 1 = 2).
        let mut world = World::new(3, 3, 3);
        world.set(1, 1, 1, stone());
        let pins_toml = pin_toml("a", (1, 1, 1), "south", "input");
        let manifest = manifest_toml("bad_cell", (3, 3, 3), &pins_toml);
        write_fixture(&dir, "bad_cell", &world, &manifest);

        let err = load_cell(&dir).expect_err("a pin off its declared face must be rejected");
        match err {
            CellError::PinNotOnFace { pin, at, side } => {
                assert_eq!(pin, "a");
                assert_eq!(at, (1, 1, 1));
                assert_eq!(side, Facing::South);
            }
            other => panic!("expected PinNotOnFace, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_pin_on_a_block_the_router_cannot_connect_to() {
        let dir = scratch_dir("pin_not_connectable");

        // (2, 1, 1) is correctly on the east face but left as air -- a
        // repeater facing into air cannot terminate a route there.
        let world = World::new(3, 3, 3);
        let pins_toml = pin_toml("a", (2, 1, 1), "east", "output");
        let manifest = manifest_toml("bad_cell", (3, 3, 3), &pins_toml);
        write_fixture(&dir, "bad_cell", &world, &manifest);

        let err = load_cell(&dir).expect_err("a pin on a non-conductive block must be rejected");
        match err {
            CellError::PinBlockNotConnectable {
                pin,
                at,
                block_name,
            } => {
                assert_eq!(pin, "a");
                assert_eq!(at, (2, 1, 1));
                assert_eq!(block_name, "minecraft:air");
            }
            other => panic!("expected PinBlockNotConnectable, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_duplicate_pin_names() {
        let dir = scratch_dir("duplicate_pin_names");

        let mut world = World::new(3, 3, 3);
        world.set(2, 1, 1, stone());
        world.set(0, 1, 1, stone());

        let mut pins_toml = String::new();
        pins_toml.push_str(&pin_toml("a", (2, 1, 1), "east", "output"));
        pins_toml.push_str(&pin_toml("a", (0, 1, 1), "west", "input"));
        let manifest = manifest_toml("bad_cell", (3, 3, 3), &pins_toml);
        write_fixture(&dir, "bad_cell", &world, &manifest);

        let err = load_cell(&dir).expect_err("a repeated pin name must be rejected");
        match err {
            CellError::DuplicatePinName { name } => assert_eq!(name, "a"),
            other => panic!("expected DuplicatePinName, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_two_pins_sharing_a_coordinate() {
        let dir = scratch_dir("duplicate_pin_coordinate");

        let mut world = World::new(3, 3, 3);
        world.set(2, 1, 1, stone());

        let mut pins_toml = String::new();
        pins_toml.push_str(&pin_toml("a", (2, 1, 1), "east", "output"));
        pins_toml.push_str(&pin_toml("b", (2, 1, 1), "east", "input"));
        let manifest = manifest_toml("bad_cell", (3, 3, 3), &pins_toml);
        write_fixture(&dir, "bad_cell", &world, &manifest);

        let err = load_cell(&dir).expect_err("two pins sharing a coordinate must be rejected");
        match err {
            CellError::DuplicatePinCoordinate { first, second, at } => {
                assert_eq!(first, "a");
                assert_eq!(second, "b");
                assert_eq!(at, (2, 1, 1));
            }
            other => panic!("expected DuplicatePinCoordinate, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_manifest_size_that_disagrees_with_the_litematic() {
        let dir = scratch_dir("size_mismatch");

        let world = World::new(3, 3, 3);
        // The manifest claims a taller cell than the litematic actually is.
        let manifest = manifest_toml("bad_cell", (3, 3, 4), "");
        write_fixture(&dir, "bad_cell", &world, &manifest);

        let err = load_cell(&dir).expect_err("a manifest/geometry size mismatch must be rejected");
        match err {
            CellError::SizeMismatch {
                cell,
                manifest_size,
                litematic_size,
            } => {
                assert_eq!(cell, "bad_cell");
                assert_eq!(manifest_size, (3, 3, 4));
                assert_eq!(litematic_size, (3, 3, 3));
            }
            other => panic!("expected SizeMismatch, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_every_cell_in_a_directory() {
        let dir = scratch_dir("library");

        let mut world_one = World::new(3, 3, 3);
        world_one.set(2, 1, 1, stone());
        let pins_one = pin_toml("out", (2, 1, 1), "east", "output");
        let manifest_one = manifest_toml("cell_one", (3, 3, 3), &pins_one);
        write_fixture(&dir.join("cell_one"), "cell_one", &world_one, &manifest_one);

        let mut world_two = World::new(2, 2, 2);
        world_two.set(0, 0, 0, stone());
        let pins_two = pin_toml("in", (0, 0, 0), "west", "input");
        let manifest_two = manifest_toml("cell_two", (2, 2, 2), &pins_two);
        write_fixture(&dir.join("cell_two"), "cell_two", &world_two, &manifest_two);

        let library = load_library(&dir).expect("a directory of valid cells must load");

        assert_eq!(library.len(), 2);
        assert!(library.contains_key("cell_one"));
        assert!(library.contains_key("cell_two"));
        assert_eq!(library["cell_one"].pins[0].name, "out");
        assert_eq!(library["cell_two"].pins[0].name, "in");

        let _ = fs::remove_dir_all(&dir);
    }
}

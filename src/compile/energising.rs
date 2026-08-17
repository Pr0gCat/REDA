//! The **derived energising range** of every block this compiler writes, read
//! out of the derived artifacts rather than written down again.
//!
//! # Why this exists
//!
//! `planner::keep_out` takes a coordinate and nothing else. It cannot know what
//! stands there, so it answers the same twelve cells -- the four horizontal
//! neighbours at `y-1`, `y` and `y+1` -- for a lever, a torch, a repeater and a
//! piece of dust alike. Two artifacts already measured what those things
//! actually reach:
//!
//! * `docs/derived/coupling-mechanisms.md` -- 279 lines of `Simulator` runs.
//!   Table 1 is the direct drive relation (**hop 1**, component -> adjacent
//!   dust); Table 2 is the same sweep with one stone block in the way, which is
//!   the gate on **hop 2** (component -> strongly powered conductive block ->
//!   dust); Tables 3 and 3b sweep the mediator's *other* faces and say how far
//!   that second hop then fans out.
//! * `docs/derived/dust-join-relation.md` -- the dust-against-dust relation,
//!   whose own summary line is the thing to read before believing any saving is
//!   available: **"12 of `keep_out`'s 12 cells really join"** for a dust cell on
//!   a stone floor. For dust, `keep_out` is not conservative; it is exact.
//!
//! So this module is *not* "keep_out is too big". It is the arithmetic that
//! says, per block kind and per facing, how much of the twelve is real.
//!
//! # What is derived here and what is not
//!
//! [`energises`] reads the tables. It does **not** restate them: change a mark
//! in the artifact and this function's answer changes with it, and
//! [`tests::the_parse_is_the_artifact`] fails if the shape of the file it is
//! parsing ever moves.
//!
//! Two things it deliberately does not fold in:
//!
//! * **The dust-join relation.** Dust-against-dust is a different relation with
//!   a different artifact and a conditional closed form (`planner::join_lid`).
//!   [`Range`] is about what a component *energises*, and a caller wanting the
//!   whole keep-out question has to ask both. [`dust_join_offsets`] is here so
//!   it can, and it is the twelve.
//! * **What reads a cell.** A repeater's rear is `x` in every table -- the rig
//!   cannot ask it, because that is where the rig's own feed has to go -- and a
//!   cell a diode *reads* is every bit as much a short as a cell it drives.
//!   Those offsets land in [`Range::unmeasured`] and a caller that wants a safe
//!   answer takes [`Range::conservative`], which keeps them.
//!
//! # This module only measures
//!
//! Nothing here is called by `compile`, and it is `#[cfg(test)]` so that is a
//! property of the build rather than a promise in a comment. Wiring any of it
//! into `anchor_is_free_for` is a separate change with its own measurements;
//! see `planner::keep_out_against`'s doc comment for the two that stopped the
//! last attempt.

use std::collections::BTreeSet;

use crate::redstone::world::block::{BlockKind, Facing};

#[cfg(test)]
mod tests;

/// A cell offset from the emitter, in `(dx, dy, dz)`.
pub type Offset = (i32, i32, i32);

/// The artifact the two hop tables are read out of.
const COUPLING: &str = include_str!("../../docs/derived/coupling-mechanisms.md");

/// The artifact the dust-against-dust relation is read out of.
const DUST_JOIN: &str = include_str!("../../docs/derived/dust-join-relation.md");

/// The six directions, in the order every table's columns are printed in.
///
/// `position::ALL_SIX`'s order, restated as offsets because this module reads
/// text and never touches a `World`. `tests::the_column_order_is_all_six`
/// checks the two against each other.
pub const SIX: [(&str, Offset); 6] = [
    ("N", (0, 0, -1)),
    ("S", (0, 0, 1)),
    ("E", (1, 0, 0)),
    ("W", (-1, 0, 0)),
    ("U", (0, 1, 0)),
    ("D", (0, -1, 0)),
];

/// One mark in one column of one artifact table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// `J` or `j` -- the reading changed against its control.
    Coupled,
    /// `.` -- it did not.
    Clear,
    /// `x`, `~`, `0`, `!` -- the rig could not answer. Never treated as clear.
    Unanswered,
}

impl Mark {
    fn of(code: &str) -> Mark {
        match code {
            "J" | "j" => Mark::Coupled,
            "." => Mark::Clear,
            _ => Mark::Unanswered,
        }
    }
}

/// What one emitter, at one facing, is measured to reach.
///
/// Split by hop because the two are gated by different things and the whole
/// point of the exercise is that today's rule has one of them and not the
/// other: `keep_out`'s twelve cells are a one-hop halo, and every one of the 41
/// extra edges `docs/derived/realised-graph-extras.md` records is hop 2.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Range {
    /// Component -> adjacent dust, with no block in between. Table 1.
    pub hop1: BTreeSet<Offset>,
    /// Component -> strongly powered conductive block -> dust on every face
    /// that block has left. Table 2 gates it, Tables 3 and 3b fan it out.
    pub hop2: BTreeSet<Offset>,
    /// Offsets no rig in the artifact could ask about -- a diode's rear, where
    /// the rig's own feed has to stand. Not clear, not coupled: unknown.
    pub unmeasured: BTreeSet<Offset>,
}

impl Range {
    /// Everything measured to couple, both hops. The optimistic answer.
    pub fn measured(&self) -> BTreeSet<Offset> {
        self.hop1.union(&self.hop2).copied().collect()
    }

    /// Everything measured to couple **plus** everything unmeasured. The answer
    /// a reservation would actually have to use, because an offset the artifact
    /// could not ask about is not an offset it said was safe.
    pub fn conservative(&self) -> BTreeSet<Offset> {
        self.measured().union(&self.unmeasured).copied().collect()
    }
}

/// The name a block kind goes by in the artifacts, or `None` for a kind no
/// table has a row for.
pub fn artifact_name(kind: BlockKind) -> Option<&'static str> {
    Some(match kind {
        BlockKind::Solid => "stone",
        BlockKind::Glass => "glass",
        BlockKind::Lamp => "lamp",
        BlockKind::RedstoneBlock => "redstone_block",
        BlockKind::RedstoneWire => "dust",
        BlockKind::Repeater => "repeater",
        BlockKind::Comparator => "comparator",
        BlockKind::Torch => "torch",
        BlockKind::WallTorch => "wall_torch",
        BlockKind::Lever => "lever",
        BlockKind::Button => "button",
        BlockKind::PressurePlate => "pressure_plate",
        _ => return None,
    })
}

/// Whether a kind's row is selected by its `facing`, which is only true of the
/// three kinds the artifact prints four rows for.
fn facing_matters(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Repeater | BlockKind::Comparator | BlockKind::WallTorch
    )
}

fn facing_label(facing: Facing) -> &'static str {
    match facing {
        Facing::North => "N",
        Facing::South => "S",
        Facing::East => "E",
        Facing::West => "W",
        Facing::Up => "U",
        Facing::Down => "D",
    }
}

/// **The derived energising range.**
///
/// `kind` and `facing` are exactly what a `BlockState` carries, so a caller
/// holding a realised world can ask this of any cell in it. `Air`, and every
/// kind the artifact has no row for, answers an empty range -- which is the
/// artifact's answer for `stone`, `glass` and `lamp` too, measured rather than
/// assumed.
///
/// `state` in the brief's `energises(kind, facing, state)` is deliberately
/// absent: the artifact's rows are **structural**, taken with the emitter
/// driven, and `compile` writes an unsettled world in which every lever is off
/// and every dust reads zero. Asking about activation would answer `no` for the
/// lever bug that shipped. `compile::coupling`'s module doc makes the same call
/// for the same reason; `tests::activation_is_deliberately_not_a_parameter`
/// records it.
pub fn energises(kind: BlockKind, facing: Option<Facing>) -> Range {
    let Some(name) = artifact_name(kind) else {
        return Range::default();
    };
    let want_facing = match (facing_matters(kind), facing) {
        (true, Some(facing)) => facing_label(facing),
        // A facing-sensitive kind with no facing recorded is not answerable
        // from a table that has four rows for it, so it gets none of them.
        (true, None) => return Range::default(),
        (false, _) => "-",
    };

    let direct = six_row(&table("## Table 1"), name, want_facing);
    let across = six_row(&table("## Table 2"), name, want_facing);

    let mut range = Range::default();
    for (index, (_, offset)) in SIX.iter().enumerate() {
        match direct[index] {
            Mark::Coupled => {
                range.hop1.insert(*offset);
            }
            Mark::Unanswered => {
                range.unmeasured.insert(*offset);
            }
            Mark::Clear => {}
        }
        // The mediator sits one step out in this column's direction; a mark
        // says it is strongly powered and conductive, and a strongly powered
        // conductive block drives dust on **every face it has left** --
        // measured in Tables 3 and 3b, and by `the_fan_out_holds_for_a_
        // horizontal_mediator_too` for the four columns those tables cannot
        // reach.
        let fan = |into: &mut BTreeSet<Offset>| {
            for (_, step) in SIX {
                let cell = (offset.0 + step.0, offset.1 + step.1, offset.2 + step.2);
                if cell != (0, 0, 0) {
                    into.insert(cell);
                }
            }
        };
        match across[index] {
            Mark::Coupled => fan(&mut range.hop2),
            Mark::Unanswered => fan(&mut range.unmeasured),
            Mark::Clear => {}
        }
    }
    // A cell measured to couple is not unmeasured, whichever hop found it.
    let known = range.measured();
    range.unmeasured.retain(|cell| !known.contains(cell));
    range
}

/// **Whether a powered dust cell one cardinal step from a block is read by a
/// torch attached to that block** -- mechanism 4 in the geometry neither
/// artifact measures.
///
/// Table 5 puts its weak driver *standing on* the mediator and never beside it,
/// so this direction is not in the derived range at all. It matters because it
/// is the reverse direction: the offender (a gate's support block) is inert,
/// the newcomer is the emitter, and [`energises`] asked of the offender
/// therefore answers *nothing*. `compile::gate_footprint` keeps foreign wire
/// off a NOR's support for exactly this reason.
///
/// Measured by `tests::a_powered_dust_against_a_gate_support_puts_its_torch_out`,
/// which fails if the simulator and this constant ever disagree.
pub const BESIDE_A_SUPPORT_IS_READ: bool = true;

/// The twelve offsets the dust-join artifact reports as joining, read out of
/// its own Table 1 rather than out of `keep_out`.
///
/// This is the half of the keep-out question [`energises`] is not about, and it
/// is the half that decides whether component-awareness buys anything at a
/// given pair: the artifact's own summary is that **all twelve really join** in
/// the shape a compiled world presents, and that the four same-layer ones are
/// unconditional -- no content of any cell anywhere separates them.
pub fn dust_join_offsets() -> BTreeSet<Offset> {
    let block = fenced_after(DUST_JOIN, "## Table 1");
    let mut joins = BTreeSet::new();
    for line in block.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 || fields[0].starts_with("offset") {
            continue;
        }
        let offset = parse_offset(fields[0]);
        // `A->B` or `B->A`: either direction merges two nets, which is the
        // artifact's own reading of its own table.
        if fields[1] == "join" || fields.get(2) == Some(&"join") {
            joins.insert(offset);
        }
    }
    joins
}

/// Whether the dust-join artifact reports this offset as **unconditional** --
/// joined with no cell anywhere able to separate the pair.
///
/// The four same-layer offsets, per the artifact's summary line. Taken from the
/// same summary rather than from a hand-written list, so a re-measurement that
/// moved one would move this.
pub fn unconditional_dust_joins() -> BTreeSet<Offset> {
    // The summary names the *conditional* ones and says they are "every offset
    // with a level change, and none without". So the unconditional set is the
    // joins less the listed conditionals, and the assertion that the listed
    // conditionals are exactly the level-changing joins is
    // `tests::the_unconditional_joins_are_the_four_same_layer_cells`.
    let conditional = summary_offsets(DUST_JOIN, "8 of the 12 are nonetheless conditional");
    dust_join_offsets()
        .into_iter()
        .filter(|offset| !conditional.contains(offset))
        .collect()
}

// ---------------------------------------------------------------------
// Reading the artifacts
// ---------------------------------------------------------------------

/// The fenced code block that follows a heading.
fn fenced_after<'a>(artifact: &'a str, heading: &str) -> &'a str {
    let start = artifact
        .find(heading)
        .unwrap_or_else(|| panic!("`{heading}` is not in the artifact any more"));
    let rest = &artifact[start..];
    let open = rest
        .find("```")
        .unwrap_or_else(|| panic!("`{heading}` has no fenced block after it"));
    let body = &rest[open + 3..];
    let close = body
        .find("```")
        .unwrap_or_else(|| panic!("`{heading}`'s fence is not closed"));
    &body[..close]
}

fn table(heading: &str) -> String {
    fenced_after(COUPLING, heading).to_string()
}

/// One row of a six-column table, as marks in `SIX` order.
///
/// Panics rather than defaulting: a row that is not there is a table whose
/// shape moved, and answering an empty range for it would silently turn every
/// count downstream into a smaller, wronger number.
fn six_row(block: &str, name: &str, facing: &str) -> [Mark; 6] {
    for line in block.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 9 || fields[0] == "emitter" {
            continue;
        }
        if fields[0] != name || fields[1] != facing {
            continue;
        }
        let mut marks = [Mark::Clear; 6];
        for (index, mark) in marks.iter_mut().enumerate() {
            *mark = Mark::of(fields[2 + index]);
        }
        return marks;
    }
    panic!("no row `{name}` / `{facing}` in this table any more");
}

fn parse_offset(field: &str) -> Offset {
    let inner = field.trim_start_matches('(').trim_end_matches(')');
    let parts: Vec<i32> = inner
        .split(',')
        .map(|piece| piece.trim().parse().expect("an integer coordinate"))
        .collect();
    assert_eq!(parts.len(), 3, "an offset has three coordinates");
    (parts[0], parts[1], parts[2])
}

/// Every `(x, y, z)` triple on the artifact summary line that starts with
/// `needle`. The summaries are computed from the runs, so this reads a measured
/// list rather than a written one.
fn summary_offsets(artifact: &str, needle: &str) -> BTreeSet<Offset> {
    let line = artifact
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("the summary line `{needle}` is gone"));
    let mut found = BTreeSet::new();
    for (start, _) in line.match_indices('(') {
        let Some(end) = line[start..].find(')') else {
            continue;
        };
        let piece = &line[start..start + end + 1];
        let inner = &piece[1..piece.len() - 1];
        let numeric = piece.matches(',').count() == 2
            && inner
                .chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c == '-' || c == ' ');
        if numeric {
            found.insert(parse_offset(piece));
        }
    }
    found
}

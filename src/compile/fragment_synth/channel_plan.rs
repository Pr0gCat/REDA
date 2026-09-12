//! Channel routing plan for the topology-aware seed.
//!
//! A channel is the free forward range between two consecutive levels.  Its
//! ground layer carries one east-west line per endpoint row, its third layer
//! carries one north-south trunk per lane, and lanes sit three cells apart so
//! a climb beside one lane never hugs the next.  This module decides which
//! lane every net uses (constrained left-edge over lateral intervals), how
//! wide a channel must be for that many lanes, and which free gap rows a
//! multi-level net crosses an intermediate column on.  Everything here is a
//! pure function of typed identities and integer coordinates in the
//! placement frame; see `.superpowers/sdd/2026-09-01-topology-aware-seed-v2/
//! channel-routing-design.md` for the reasoning.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use thiserror::Error;

/// Forward cells between the column edge and the first lane: a three-cell
/// entry line, the cell beyond it, and the climb cell.
pub(crate) const LANE_MARGIN: i32 = 3;
/// Forward cells between neighbouring lanes.
pub(crate) const LANE_PITCH: i32 = 3;
/// Closed forward cells a turnaround needs beyond its free channel, before
/// the first column and after the last one.
pub(crate) const FORWARD_MARGIN: i32 = 8;
/// Free forward cells the pre-bounded turnaround channel always reserved
/// beyond the last column, whatever the nets crossing it needed.
pub(crate) const LEGACY_TURNAROUND_CHANNEL: i32 = 40;

/// Forward cells the legacy turnaround takes: its fixed channel and the
/// closed margin behind it.
pub(crate) const fn legacy_turnaround_allowance() -> i32 {
    LEGACY_TURNAROUND_CHANNEL + FORWARD_MARGIN
}

/// Free forward cells a turnaround channel needs for this many lanes.
pub(crate) fn bounded_turnaround_channel(lanes: usize) -> i32 {
    channel_width(lanes)
}

/// Forward cells a bounded turnaround takes: its channel and the closed
/// margin behind it.
pub(crate) fn bounded_turnaround_allowance(lanes: usize) -> i32 {
    bounded_turnaround_channel(lanes) + FORWARD_MARGIN
}

/// One net's presence in one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChannelNet<N> {
    pub id: N,
    /// Inclusive lateral extent the trunk must cover.
    pub interval: (i32, i32),
    /// Rows whose ground line runs from the source-side edge to `lane - 2`.
    pub source_rows: Vec<i32>,
    /// Rows whose ground line runs from `lane + 2` to the sink-side edge.
    pub sink_rows: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ChannelPlanError<N: Debug> {
    #[error("nets {nets:?} keep each other below their own lane")]
    ConstraintCycle { nets: Vec<N> },
    #[error("nets {first:?} and {second:?} both need row {row} on the same channel side")]
    RowClash { first: N, second: N, row: i32 },
    #[error("net {net:?} cannot cross the column: no free row near {preferred}")]
    NoCrossingRow { net: N, preferred: i32 },
}

/// One straight run of a net's trunk on one lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment {
    /// Lane index within its pool: counted from the start edge for a net
    /// with a line on the start edge, from the end edge for a net whose lines
    /// all lie on the end edge.
    pub lane: usize,
    /// Whether `lane` counts from the end edge.
    pub from_end: bool,
    /// Inclusive lateral extent of the run.
    pub interval: (i32, i32),
    /// Row at which the trunk leaves this lane for the next segment's lane.
    pub jog: Option<i32>,
    /// The rows this segment serves.  A dogleg's two segments overlap at
    /// the jog row and the rows between, so a row must climb onto or leave
    /// the lane of the segment that owns it, not the first lane it lies in.
    pub source_rows: Vec<i32>,
    pub sink_rows: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChannelPlan<N> {
    /// Every net's trunk, in lateral order.  Consecutive segments meet at a
    /// jog row: the trunk leaves the first lane there, runs along the ground
    /// between the two lanes, and climbs onto the second.
    pub segments: BTreeMap<N, Vec<Segment>>,
    /// Lanes counted from the start edge.
    pub start_lanes: usize,
    /// Lanes counted from the end edge.
    pub end_lanes: usize,
    /// All lanes together.
    pub lane_count: usize,
}

impl<N: Ord + Copy> ChannelPlan<N> {
    /// The lane of a net that needed no jog.
    #[cfg(test)]
    fn single_lane(&self, id: N) -> Option<usize> {
        match self.segments.get(&id).map(Vec::as_slice) {
            Some([segment]) => Some(segment.lane),
            _ => None,
        }
    }
}

/// Forward coordinate of a lane, counted from the first free channel cell.
pub(crate) fn lane_forward(channel_start: i32, lane: usize) -> i32 {
    channel_start + LANE_MARGIN + LANE_PITCH * i32::try_from(lane).unwrap_or(i32::MAX / LANE_PITCH)
}

/// Free forward cells a channel needs for this many lanes: the lane margin
/// on both sides and three cells per lane.
pub(crate) fn channel_width(lane_count: usize) -> i32 {
    2 * LANE_MARGIN + LANE_PITCH * i32::try_from(lane_count.max(1)).unwrap_or(i32::MAX / LANE_PITCH)
}

/// Forward coordinate of a lane counted from the end edge.
pub(crate) fn lane_forward_from_end(channel_end: i32, lane: usize) -> i32 {
    channel_end - LANE_MARGIN - LANE_PITCH * i32::try_from(lane).unwrap_or(i32::MAX / LANE_PITCH)
}

/// Two trunks on one lane need at least one empty cell between them, so
/// intervals that merely touch count as overlapping.
fn overlaps(first: (i32, i32), second: (i32, i32)) -> bool {
    first.0 <= second.1 + 1 && second.0 <= first.1 + 1
}

/// Smallest number of lanes that can carry these lateral intervals: the
/// greedy left-edge colouring, which is optimal for interval graphs.
pub(crate) fn lane_count(intervals: &[(i32, i32)]) -> usize {
    let mut ordered = intervals.to_vec();
    ordered.sort();
    let mut lanes = Vec::<Vec<(i32, i32)>>::new();
    for interval in ordered {
        match lanes
            .iter_mut()
            .find(|lane| lane.iter().all(|used| !overlaps(*used, interval)))
        {
            Some(lane) => lane.push(interval),
            None => lanes.push(vec![interval]),
        }
    }
    lanes.len()
}

/// One straight piece of a net between two of its rows; a net that jogs is
/// several pieces.
#[derive(Debug, Clone)]
struct Piece<N> {
    net: N,
    index: usize,
    interval: (i32, i32),
    source_rows: Vec<i32>,
    sink_rows: Vec<i32>,
    /// Row where this piece hands over to the sink-side piece.
    jog_out: Option<i32>,
}

impl<N: Copy> Piece<N> {
    fn rows(&self) -> impl Iterator<Item = i32> + '_ {
        self.source_rows
            .iter()
            .chain(self.sink_rows.iter())
            .copied()
    }
}

/// Assigns every net its lanes.
///
/// Two ground lines in the same or adjacent rows may not overlap in forward
/// extent.  A source line reaches from the source-side edge to `lane - 2`, a
/// sink line from `lane + 2` to the sink-side edge, so a source row of net A
/// next to a sink row of net B forces `lane(A) < lane(B)`.  Two source rows
/// (or two sink rows) of different nets next to each other always overlap and
/// are reported as a placement clash.
///
/// When these constraints form a cycle, the classic dogleg resolves it: one
/// net of the cycle is split at a jog row (a free lateral at least two cells
/// from every row in the channel), so its source-side piece and sink-side
/// piece can sit on different lanes.  Only a cycle no split can break is an
/// error.
pub(crate) fn plan_channel<N: Ord + Copy + Debug>(
    nets: &[ChannelNet<N>],
    free_jog_rows: &BTreeSet<i32>,
) -> Result<ChannelPlan<N>, ChannelPlanError<N>> {
    for first in nets {
        for second in nets {
            if first.id >= second.id {
                continue;
            }
            for &row in &first.source_rows {
                if let Some(&clash) = second
                    .source_rows
                    .iter()
                    .find(|other| (*other - row).abs() <= 1)
                {
                    return Err(ChannelPlanError::RowClash {
                        first: first.id,
                        second: second.id,
                        row: clash,
                    });
                }
            }
            for &row in &first.sink_rows {
                if let Some(&clash) = second
                    .sink_rows
                    .iter()
                    .find(|other| (*other - row).abs() <= 1)
                {
                    return Err(ChannelPlanError::RowClash {
                        first: first.id,
                        second: second.id,
                        row: clash,
                    });
                }
            }
        }
    }

    let mut pieces = nets
        .iter()
        .map(|net| Piece {
            net: net.id,
            index: 0,
            interval: net.interval,
            source_rows: net.source_rows.clone(),
            sink_rows: net.sink_rows.clone(),
            jog_out: None,
        })
        .collect::<Vec<_>>();
    let all_rows = nets
        .iter()
        .flat_map(|net| net.source_rows.iter().chain(net.sink_rows.iter()).copied())
        .collect::<BTreeSet<_>>();
    let jog_rows = free_jog_rows
        .iter()
        .copied()
        .filter(|row| all_rows.iter().all(|used| (used - row).abs() > 1))
        .collect::<Vec<_>>();
    let mut jogs_used = BTreeSet::<i32>::new();

    // Nets whose lines all lie on the end edge take lanes counted from that
    // edge; they never constrain anything and keep their ground lines short.
    let end_only = nets
        .iter()
        .filter(|net| net.source_rows.is_empty())
        .map(|net| net.id)
        .collect::<BTreeSet<_>>();
    let end_pieces = pieces
        .iter()
        .filter(|piece| end_only.contains(&piece.net))
        .cloned()
        .collect::<Vec<_>>();
    pieces.retain(|piece| !end_only.contains(&piece.net));
    let (end_lanes, end_lane_count) = assign_pieces(&end_pieces).unwrap_or_default();

    let split_budget = nets.len().saturating_mul(2);
    for _ in 0..=split_budget {
        match assign_pieces(&pieces) {
            Ok((lanes, lane_count)) => {
                let mut segments = BTreeMap::<N, Vec<Segment>>::new();
                let mut ordered = pieces.iter().chain(end_pieces.iter()).collect::<Vec<_>>();
                ordered.sort_by_key(|piece| (piece.net, piece.index));
                for piece in ordered {
                    let from_end = end_only.contains(&piece.net);
                    let key = (piece.net, piece.index);
                    segments.entry(piece.net).or_default().push(Segment {
                        lane: if from_end {
                            end_lanes[&key]
                        } else {
                            lanes[&key]
                        },
                        from_end,
                        interval: piece.interval,
                        jog: piece.jog_out,
                        source_rows: piece.source_rows.clone(),
                        sink_rows: piece.sink_rows.clone(),
                    });
                }
                return Ok(ChannelPlan {
                    segments,
                    start_lanes: lane_count,
                    end_lanes: end_lane_count,
                    lane_count: lane_count + end_lane_count,
                });
            }
            Err(stalled) => {
                let Some((piece_position, jog)) =
                    choose_jog(&pieces, &stalled, &jog_rows, &jogs_used)
                else {
                    return Err(ChannelPlanError::ConstraintCycle {
                        nets: stalled.iter().map(|(net, _)| *net).collect(),
                    });
                };
                jogs_used.insert(jog);
                let piece = pieces.remove(piece_position);
                let (low, high) = split_piece(piece, jog);
                pieces.push(low);
                pieces.push(high);
                for piece in &mut pieces {
                    if piece.index > 0 {
                        // keep indices dense per net in lateral order
                    }
                }
                renumber(&mut pieces);
            }
        }
    }
    Err(ChannelPlanError::ConstraintCycle {
        nets: nets.iter().map(|net| net.id).collect(),
    })
}

/// Constrained left-edge over the pieces.  Returns the stalled pieces when
/// no lane can take any of the remaining ones.
#[allow(clippy::type_complexity)]
fn assign_pieces<N: Ord + Copy + Debug>(
    pieces: &[Piece<N>],
) -> Result<(BTreeMap<(N, usize), usize>, usize), Vec<(N, usize)>> {
    let key = |piece: &Piece<N>| (piece.net, piece.index);
    let mut predecessors = BTreeMap::<(N, usize), BTreeSet<(N, usize)>>::new();
    for piece in pieces {
        predecessors.entry(key(piece)).or_default();
    }
    for first in pieces {
        for second in pieces {
            if first.net == second.net {
                continue;
            }
            for &row in &first.source_rows {
                if second
                    .sink_rows
                    .iter()
                    .any(|other| (other - row).abs() <= 1)
                {
                    predecessors
                        .entry(key(second))
                        .or_default()
                        .insert(key(first));
                }
            }
        }
    }
    // Pieces that touch the start edge take the low lanes; pieces that only
    // touch the end edge follow, so their trunks stay near the edge their
    // lines come from and the ground lines stay short.
    let mut remaining = pieces.iter().collect::<Vec<_>>();
    remaining.sort_by_key(|piece| {
        (
            piece.source_rows.is_empty(),
            piece.interval,
            piece.net,
            piece.index,
        )
    });
    let mut lanes = BTreeMap::<(N, usize), usize>::new();
    let mut lane_count = 0usize;
    while !remaining.is_empty() {
        let lane = lane_count;
        let mut used = Vec::<(i32, i32)>::new();
        let mut placed_now = Vec::<(N, usize)>::new();
        remaining.retain(|piece| {
            let ready = predecessors[&key(piece)]
                .iter()
                .all(|earlier| lanes.get(earlier).is_some_and(|&other| other < lane));
            let free = used
                .iter()
                .all(|interval| !overlaps(*interval, piece.interval));
            if ready && free {
                used.push(piece.interval);
                placed_now.push(key(piece));
                false
            } else {
                true
            }
        });
        if placed_now.is_empty() {
            return Err(remaining.iter().map(|piece| key(piece)).collect());
        }
        for id in placed_now {
            lanes.insert(id, lane);
        }
        lane_count += 1;
    }
    Ok((lanes, lane_count))
}

/// Picks the stalled piece to split and the jog row to split it at: the
/// first stalled piece (in stall order) that still has both source and
/// sink rows, at the unused jog row nearest the middle of its rows.
///
/// The split puts every source row on one piece and every sink row on the
/// other, so the source piece only has to stay below other nets and the
/// sink piece only above them; no cycle can pass through both.  The jog row
/// may lie outside the piece's own rows: both pieces then extend to it.
fn choose_jog<N: Ord + Copy>(
    pieces: &[Piece<N>],
    stalled: &[(N, usize)],
    jog_rows: &[i32],
    jogs_used: &BTreeSet<i32>,
) -> Option<(usize, i32)> {
    for &(net, index) in stalled {
        let position = pieces
            .iter()
            .position(|piece| piece.net == net && piece.index == index)?;
        let piece = &pieces[position];
        if piece.source_rows.is_empty() || piece.sink_rows.is_empty() {
            continue;
        }
        let low = piece.rows().min()?;
        let high = piece.rows().max()?;
        let middle = (low + high) / 2;
        let jog = jog_rows
            .iter()
            .copied()
            .filter(|row| !jogs_used.contains(row))
            .filter(|row| jogs_used.iter().all(|used| (used - row).abs() > 1))
            .min_by_key(|row| ((row - middle).abs(), *row));
        if let Some(jog) = jog {
            return Some((position, jog));
        }
    }
    None
}

fn hull(rows: impl Iterator<Item = i32>) -> (i32, i32) {
    rows.fold((i32::MAX, i32::MIN), |(low, high), row| {
        (low.min(row), high.max(row))
    })
}

fn split_piece<N: Copy>(piece: Piece<N>, jog: i32) -> (Piece<N>, Piece<N>) {
    let source_side = Piece {
        net: piece.net,
        index: piece.index,
        interval: hull(piece.source_rows.iter().copied().chain([jog])),
        source_rows: piece.source_rows.clone(),
        sink_rows: Vec::new(),
        jog_out: Some(jog),
    };
    let sink_side = Piece {
        net: piece.net,
        index: piece.index + 1,
        interval: hull(piece.sink_rows.iter().copied().chain([jog])),
        source_rows: Vec::new(),
        sink_rows: piece.sink_rows,
        jog_out: None,
    };
    (source_side, sink_side)
}

/// Keeps every net's piece indices dense, the source-side piece first.
fn renumber<N: Ord + Copy>(pieces: &mut [Piece<N>]) {
    pieces.sort_by_key(|piece| (piece.net, piece.jog_out.is_none(), piece.interval));
    let mut previous: Option<N> = None;
    let mut next_index = 0usize;
    for piece in pieces.iter_mut() {
        if previous != Some(piece.net) {
            previous = Some(piece.net);
            next_index = 0;
        }
        piece.index = next_index;
        next_index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(
        id: u32,
        interval: (i32, i32),
        source_rows: &[i32],
        sink_rows: &[i32],
    ) -> ChannelNet<u32> {
        ChannelNet {
            id,
            interval,
            source_rows: source_rows.to_vec(),
            sink_rows: sink_rows.to_vec(),
        }
    }

    #[test]
    fn lanes_sit_three_cells_in_and_three_apart() {
        assert_eq!(lane_forward(40, 0), 43);
        assert_eq!(lane_forward(40, 1), 46);
        assert_eq!(lane_forward(40, 3), 52);
        assert_eq!(channel_width(1), 9);
        assert_eq!(channel_width(4), 18);
        // A channel of width 18 starting at 40 spans 40..=57: lane 3 at 52
        // leaves 53..=57 for the descent cells, the sink line, and the edge.
        assert!(lane_forward(40, 3) + LANE_MARGIN <= 40 + channel_width(4) - 1 + 1);
    }

    #[test]
    fn lane_count_is_the_maximum_overlap() {
        assert_eq!(lane_count(&[]), 0);
        assert_eq!(lane_count(&[(0, 10), (12, 20)]), 1);
        assert_eq!(lane_count(&[(0, 10), (10, 20)]), 2);
        assert_eq!(lane_count(&[(0, 10), (11, 20)]), 2);
        assert_eq!(lane_count(&[(0, 30), (5, 8), (6, 9), (20, 25)]), 3);
    }

    fn no_jogs() -> BTreeSet<i32> {
        BTreeSet::new()
    }

    #[test]
    fn disjoint_intervals_share_a_lane_and_overlapping_ones_do_not() {
        let plan = plan_channel(
            &[
                net(1, (0, 10), &[0], &[10]),
                net(2, (12, 20), &[12], &[20]),
                net(3, (5, 15), &[5], &[15]),
            ],
            &no_jogs(),
        )
        .unwrap();
        assert_eq!(plan.lane_count, 2);
        assert_eq!(plan.single_lane(1), Some(0));
        assert_eq!(plan.single_lane(2), Some(0));
        assert_eq!(plan.single_lane(3), Some(1));
    }

    #[test]
    fn a_source_row_beside_a_sink_row_keeps_the_source_on_the_lower_lane() {
        // Net 2's source row 20 sits next to net 1's sink row 21: net 2's
        // ground line runs from the source edge, net 1's from the sink edge,
        // so net 2 must take the lower lane even though net 1 starts first.
        let plan = plan_channel(
            &[net(1, (0, 21), &[0], &[21]), net(2, (20, 40), &[20], &[40])],
            &no_jogs(),
        )
        .unwrap();
        assert_eq!(plan.single_lane(2), Some(0));
        assert_eq!(plan.single_lane(1), Some(1));
    }

    #[test]
    fn mutual_row_constraints_are_a_typed_cycle_without_a_jog_row() {
        let error = plan_channel(
            &[net(1, (0, 20), &[0], &[20]), net(2, (0, 20), &[20], &[0])],
            &no_jogs(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            ChannelPlanError::ConstraintCycle { nets: vec![1, 2] }
        );
    }

    #[test]
    fn a_free_jog_row_breaks_the_cycle_with_a_dogleg() {
        // Net 1 runs source row 0 -> sink row 20, net 2 runs source row 20
        // -> sink row 0: each must be below the other.  Splitting net 1 at
        // row 10 lets its source piece sit below net 2 and its sink piece
        // above.
        let plan = plan_channel(
            &[net(1, (0, 20), &[0], &[20]), net(2, (0, 20), &[20], &[0])],
            &BTreeSet::from([10]),
        )
        .unwrap();
        let first = &plan.segments[&1];
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].interval, (0, 10));
        assert_eq!(first[1].interval, (10, 20));
        let second = plan.single_lane(2).unwrap();
        assert!(first[0].lane < second, "source piece below net 2: {plan:?}");
        assert!(first[1].lane > second, "sink piece above net 2: {plan:?}");
        assert_eq!(plan.lane_count, 3);
    }

    #[test]
    fn two_nets_swapping_neighbouring_rows_dogleg_outside_their_span() {
        // Net 1 runs 30 -> 32 and net 2 runs 32 -> 30; no free row lies
        // between them, so net 1 jogs at 28 instead and its two pieces
        // straddle net 2's lane.
        let plan = plan_channel(
            &[
                net(1, (30, 32), &[30], &[32]),
                net(2, (30, 32), &[32], &[30]),
            ],
            &BTreeSet::from([28, 34]),
        )
        .unwrap();
        let first = &plan.segments[&1];
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].interval, (28, 30));
        assert_eq!(first[0].jog, Some(28));
        assert_eq!(first[1].interval, (28, 32));
        assert_eq!(first[1].jog, None);
        let second = plan.single_lane(2).unwrap();
        assert!(first[0].lane < second && second < first[1].lane, "{plan:?}");
        // Both pieces span the jog row and the rows between, so each row
        // must name the piece that owns it: the source row climbs onto the
        // first lane, the sink row leaves the second.
        assert_eq!(
            (
                first[0].source_rows.as_slice(),
                first[0].sink_rows.as_slice()
            ),
            (&[30][..], &[][..])
        );
        assert_eq!(
            (
                first[1].source_rows.as_slice(),
                first[1].sink_rows.as_slice()
            ),
            (&[][..], &[32][..])
        );
    }

    #[test]
    fn end_only_nets_take_lanes_counted_from_the_end_edge() {
        let plan = plan_channel(
            &[
                net(1, (0, 20), &[0], &[20]),
                net(2, (10, 30), &[], &[10, 30]),
            ],
            &no_jogs(),
        )
        .unwrap();
        let first = &plan.segments[&1][0];
        let second = &plan.segments[&2][0];
        assert!(!first.from_end);
        assert!(second.from_end);
        assert_eq!((first.lane, second.lane), (0, 0));
        assert_eq!(
            (plan.start_lanes, plan.end_lanes, plan.lane_count),
            (1, 1, 2)
        );
        assert_eq!(lane_forward_from_end(100, 0), 97);
        assert_eq!(lane_forward_from_end(100, 1), 94);
    }

    #[test]
    fn jog_rows_next_to_an_endpoint_row_are_never_used() {
        let error = plan_channel(
            &[net(1, (0, 20), &[0], &[20]), net(2, (0, 20), &[20], &[0])],
            &BTreeSet::from([1, 19]),
        )
        .unwrap_err();
        assert_eq!(
            error,
            ChannelPlanError::ConstraintCycle { nets: vec![1, 2] }
        );
    }

    #[test]
    fn two_source_rows_side_by_side_are_a_row_clash() {
        let error = plan_channel(
            &[net(1, (0, 20), &[7], &[20]), net(2, (0, 30), &[8], &[30])],
            &no_jogs(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            ChannelPlanError::RowClash {
                first: 1,
                second: 2,
                row: 8
            }
        );
    }

    #[test]
    fn the_same_net_may_use_neighbouring_rows() {
        let plan = plan_channel(&[net(1, (0, 20), &[10], &[11])], &no_jogs()).unwrap();
        assert_eq!(plan.single_lane(1), Some(0));
    }

    #[test]
    fn assignment_is_independent_of_input_order() {
        let forward = vec![
            net(4, (0, 50), &[0], &[50]),
            net(1, (3, 9), &[3], &[9]),
            net(2, (8, 12), &[8], &[12]),
            net(3, (30, 31), &[30], &[31]),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();
        assert_eq!(
            plan_channel(&forward, &no_jogs()),
            plan_channel(&reversed, &no_jogs())
        );
    }

    #[test]
    fn turnaround_allowance_is_channel_plus_closed_margin() {
        assert_eq!(legacy_turnaround_allowance(), 48);
        assert_eq!(
            legacy_turnaround_allowance(),
            LEGACY_TURNAROUND_CHANNEL + FORWARD_MARGIN
        );
        assert_eq!(
            bounded_turnaround_allowance(3),
            bounded_turnaround_channel(3) + FORWARD_MARGIN
        );
    }
}

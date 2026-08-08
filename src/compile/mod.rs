//! 把一個 NOR 邏輯閘網表編譯成一個能在 Minecraft 裡運作的紅石世界。
//!
//! 這是第一條端到端的編譯路徑：網表進去，`.litematic` 出來，模擬器驗證
//! 它跟真值表一致。
//!
//! # 只有一種閘：NOR
//!
//! 紅石的天然閘基底就是 NOR —— 多條紅石粉匯入一個方塊，旁邊插一支火把：
//! 任一輸入充能那個方塊，火把就熄滅。NOR 是通用閘，任何布林函數都能只用
//! NOR 組出來，所以這是唯一需要的 cell。
//!
//! # Placement and routing
//!
//! Gates are levelised and every gate of one level shares one row, so Z grows
//! with the netlist's depth rather than its gate count. Between two rows sits
//! a routing channel: east-west tracks on one Y layer, north-south columns on
//! another, so that nets can cross. Tracks are shared by left-edge assignment,
//! which makes a channel as deep as the netlist's local density instead of as
//! deep as its edge count. See the "Placement and routing" section further
//! down for the full picture.
//!
//! Every run of dust gets a repeater at least every 15 blocks, and a route
//! always ends in a repeater facing the next gate's support block -- redstone
//! dust does not charge a block sideways (only weakly, straight down), so
//! every wire has to be terminated by an active component.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::redstone::simulator::connectivity::{dust_connections, dust_reach};
use crate::redstone::simulator::position::{Position, HORIZONTAL};
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use crate::redstone::world::storage::World;

pub mod routing_stats;

// ---------------------------------------------------------------------
// 網表
// ---------------------------------------------------------------------

/// 一個 NOR 閘：任一輸入為高則輸出為低。
pub struct Gate {
    pub name: String,
    pub inputs: Vec<String>,
    pub output: String,
}

/// 一個邏輯閘網表。這是編譯器的輸入。
///
/// 只有 NOR 一種閘 —— 紅石的天然閘基底就是 NOR（多條紅石粉匯入一個方塊，
/// 旁邊插一支火把），而 NOR 是通用閘，任何布林函數都能用它組出來。
pub struct Netlist {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub gates: Vec<Gate>,
}

impl Netlist {
    /// 依相依關係排出計算順序。回傳 `None` 表示網表有迴路。
    ///
    /// 相依關係只看「這個閘的輸入是不是另一個閘的輸出」—— 外部輸入不算
    /// 相依，一開始就可用。用 Kahn 演算法，處理順序固定（依索引由小到大），
    /// 保證同一個網表每次排出來的順序都一樣。
    pub fn topological_order(&self) -> Option<Vec<usize>> {
        let gate_count = self.gates.len();

        let mut producer_of: HashMap<&str, usize> = HashMap::new();
        for (index, gate) in self.gates.iter().enumerate() {
            producer_of.insert(gate.output.as_str(), index);
        }

        let mut in_degree = vec![0usize; gate_count];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); gate_count];
        for (index, gate) in self.gates.iter().enumerate() {
            for input in &gate.inputs {
                if let Some(&producer_index) = producer_of.get(input.as_str()) {
                    dependents[producer_index].push(index);
                    in_degree[index] += 1;
                }
            }
        }

        let mut ready: VecDeque<usize> = (0..gate_count).filter(|&i| in_degree[i] == 0).collect();
        let mut order = Vec::with_capacity(gate_count);
        while let Some(index) = ready.pop_front() {
            order.push(index);
            for &dependent in &dependents[index] {
                in_degree[dependent] -= 1;
                if in_degree[dependent] == 0 {
                    ready.push_back(dependent);
                }
            }
        }

        if order.len() == gate_count {
            Some(order)
        } else {
            None
        }
    }

    /// 這個訊號名稱是不是某個外部輸入或某個閘的輸出。
    fn is_driven(&self, signal: &str) -> bool {
        self.inputs.iter().any(|name| name == signal)
            || self.gates.iter().any(|gate| gate.output == signal)
    }
}

// ---------------------------------------------------------------------
// Cell library：一個 NOR 閘在紅石裡的實體佈局
// ---------------------------------------------------------------------

/// 輸入的方向，固定順序。最多支援 3 個輸入 —— 第四個水平方向留給輸出。
const INPUT_DIRECTIONS: [Facing; 3] = [Facing::West, Facing::East, Facing::South];

/// 輸出固定朝北。
const OUTPUT_DIRECTION: Facing = Facing::North;

/// 一個 NOR 閘在紅石裡的實體佈局。
pub struct NorCell {
    /// 這個 cell 佔的空間
    pub size: (i32, i32, i32),
    /// 每個輸入的相對座標 —— 外部訊號要在這裡接一個中繼器（面朝支撐塊）
    /// 或拉桿才能驅動這個輸入。
    pub input_offsets: Vec<(i32, i32, i32)>,
    /// 輸出的相對座標 —— 就是輸出火把本身，讀它的 `lit` 就是這個閘的輸出。
    pub output_offset: (i32, i32, i32),
}

fn stone() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Solid;
    state.name = "minecraft:stone".to_string();
    state
}

fn dust() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::RedstoneWire;
    state.name = "minecraft:redstone_wire".to_string();
    state
}

fn wall_torch(facing: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::WallTorch;
    state.name = "minecraft:redstone_wall_torch".to_string();
    state.facing = Some(facing);
    state.lit = true;
    state
}

/// A redstone lamp: the block a person actually reads an output from.
///
/// `lit` starts `false` -- it is not self-consistent with the world around it
/// yet at construction time, unlike the other active components here, so the
/// simulator's first settle pass is what gives it a correct initial value
/// (see `Simulator::new`, which recomputes dust strengths and then schedules
/// any mismatched component before the caller ever reads anything).
fn lamp() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Lamp;
    state.name = "minecraft:redstone_lamp".to_string();
    state.lit = false;
    state
}

/// `place_primary_input` always stands the lever on top of a floor block
/// (`ensure_floor` runs right after this), never against a wall, so `face`
/// must be `Floor` -- Minecraft's own default is `Wall`, which is exactly
/// the mismatch that made every previous lever pop off as a dropped item on
/// paste (see `minecraft.wiki/w/Lever`'s blockstate table). `facing` is
/// purely cosmetic for a floor lever (it only orients the little handle),
/// so it is fixed to `North` for determinism rather than left unset.
fn lever(on: bool) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Lever;
    state.name = "minecraft:lever".to_string();
    state.lit = on;
    state.face = Some(Face::Floor);
    state.facing = Some(Facing::North);
    state
}

/// Place a repeater whose signal travels toward `direction` -- i.e. it reads
/// its input from `direction.opposite()` and drives its output toward
/// `direction`.
///
/// Minecraft's `facing` block state records the opposite: "the direction from
/// the output side to the input side" (see `minecraft.wiki/w/Redstone_Repeater`'s
/// blockstate table), so the stored `facing` is `direction.opposite()`, not
/// `direction` itself. Every call site below only ever cares about the
/// direction the signal is travelling, so that is what this helper takes --
/// the Minecraft-facing conversion happens once, here.
fn repeater(direction: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Repeater;
    state.name = "minecraft:repeater".to_string();
    state.facing = Some(direction.opposite());
    state.delay = 1;
    state.lit = true;
    state
}

/// 把一個 n 輸入的 NOR 閘畫進世界。
///
/// 佈局跟 `tests/simulator_circuits.rs` 裡手搭的 NOR 閘一樣：中心方塊
/// 正上方是匯流紅石粉；每個輸入各自一個支撐塊（西、東、南三個方向），
/// 支撐塊上方的紅石粉跟匯流粉水平相鄰；輸出是貼在中心方塊北側的牆上
/// 火把。輸入的「插座」留在支撐塊再往外一格（`input_offsets`），外部
/// 訊號要用主動元件（中繼器或拉桿）從那裡強充能支撐塊 —— 紅石粉本身不會
/// 水平充能方塊，這是它辦不到的。
pub fn place_nor_gate(world: &mut World, origin: (i32, i32, i32), input_count: usize) -> NorCell {
    assert!(
        input_count <= INPUT_DIRECTIONS.len(),
        "place_nor_gate 最多支援 {} 個輸入，收到 {input_count}",
        INPUT_DIRECTIONS.len()
    );

    let center = Position::new(origin.0, origin.1, origin.2);
    world.set(center.x, center.y, center.z, stone());

    let merge_dust = center.up();
    world.set(merge_dust.x, merge_dust.y, merge_dust.z, dust());

    let mut input_offsets = Vec::with_capacity(input_count);
    for &direction in INPUT_DIRECTIONS.iter().take(input_count) {
        let support = center.offset(direction);
        let support_dust = support.up();
        world.set(support.x, support.y, support.z, stone());
        world.set(support_dust.x, support_dust.y, support_dust.z, dust());

        // 插座本身留空 —— 由呼叫端（router）決定要接拉桿還是中繼器。
        let socket = support.offset(direction);
        input_offsets.push((socket.x - center.x, socket.y - center.y, socket.z - center.z));
    }

    let output_torch_pos = center.offset(OUTPUT_DIRECTION);
    world.set(
        output_torch_pos.x,
        output_torch_pos.y,
        output_torch_pos.z,
        wall_torch(OUTPUT_DIRECTION),
    );

    // 邊界盒：涵蓋中心、所有輸入插座、輸出插座（火把再往外一格，給繞線
    // 留出跟輸出火把不直接相鄰的緩衝），以及地板（y-1）跟匯流粉（y+1）。
    let output_socket = output_torch_pos.offset(OUTPUT_DIRECTION);
    let mut min = (center.x, center.y - 1, center.z);
    let mut max = (center.x, center.y + 1, center.z);
    let mut extend = |p: Position| {
        min.0 = min.0.min(p.x);
        min.1 = min.1.min(p.y);
        min.2 = min.2.min(p.z);
        max.0 = max.0.max(p.x);
        max.1 = max.1.max(p.y);
        max.2 = max.2.max(p.z);
    };
    extend(output_socket);
    for &(dx, dy, dz) in &input_offsets {
        extend(Position::new(center.x + dx, center.y + dy, center.z + dz));
    }

    let size = (max.0 - min.0 + 1, max.1 - min.1 + 1, max.2 - min.2 + 1);

    NorCell {
        size,
        input_offsets,
        output_offset: (
            output_torch_pos.x - center.x,
            output_torch_pos.y - center.y,
            output_torch_pos.z - center.z,
        ),
    }
}

// ---------------------------------------------------------------------
// Placement and routing
// ---------------------------------------------------------------------
//
// The floorplan is the classic row/channel shape of a standard-cell
// place-and-route, turned north so that it matches the way a NOR cell emits:
//
//     row 0            the primary inputs' levers            (largest Z)
//     channel 0        routing
//     row 1            every gate whose level is 0
//     channel 1        routing
//     row 2            every gate whose level is 1
//     ...                                                    (smallest Z)
//
// Signal flow is northwards (-Z), because `place_nor_gate` puts the output
// torch on the cell's north face. Every gate of one level shares one row, so Z
// grows with the *depth* of the netlist, not with its gate count.
//
// Inside a channel the two Manhattan directions live on two different Y
// layers, and that is what lets nets cross each other at all:
//
//   Y = TRACK_Y      east-west "tracks". One track carries several nets when
//                    their X spans are disjoint (left-edge assignment), so a
//                    channel is as deep as the netlist's local *density*, not
//                    as deep as its edge count.
//   Y = TRACK_Y - 1  the tracks' stone floor. It doubles as the shield that
//                    stops a track from reaching down into a column.
//   Y = GATE_Y       north-south "columns", one per pin. A column passes
//                    underneath any number of tracks without touching them.
//
// A net that has to reach a level further away than the next one takes a
// feed-through: a column that runs straight through the intervening rows in a
// reserved X slot and rejoins a track in the later channel. A long net
// therefore costs one column per level it crosses, instead of one dedicated
// lane for its whole length.
//
// A net with exactly one sink whose source column and sink approach column
// are close enough skips this whole climb-cross-descend dance and instead
// connects straight across at `GATE_Y`, never touching `TRACK_Y` at all.
// "Close enough" is decided two ways. Inside `BYPASS_MAX_DISTANCE`, it is not
// tuned at all -- that constant derives it from the same clearance invariant
// that keeps every other column apart, so a bypass route in that range is
// provably clear of every other net's column before it is ever laid (see
// that constant's doc comment for the proof). Beyond it, out to
// `BYPASS_QUERY_MAX_DISTANCE`, `resolve_bypass_and_geometry` asks instead of
// proving: it checks a candidate route's actual cells against the
// `Reservation` every other net's routing already claims, and against every
// row it crosses, and only takes the ones that come back clear. See
// `compute_bypass` and `resolve_bypass_and_geometry` for the two passes, and
// `lay_bent_path` for how either kind of route is built without paying for a
// mandatory strength refresh it does not need.

/// Y of the gate bodies, their input sockets, the output pins, and every
/// north-south routing column.
const GATE_Y: i32 = 1;

/// Y of the east-west routing tracks. `TRACK_Y - 1` is their stone floor.
const TRACK_Y: i32 = 3;

/// Floor, gates, merge dust / track floor, tracks, and one spare layer of air
/// above the tracks so nothing is ever written outside the world.
const WORLD_HEIGHT: i32 = 5;

/// X distance between two neighbouring gates of the same row.
///
/// A gate cell reaches out to `cx ± GATE_HALF_WIDTH`, so 14 leaves a five-wide
/// gap between two cells -- room for exactly one feed-through column that is
/// still at least `COLUMN_CLEARANCE` clear of everything on either side.
const SLOT_PITCH: i32 = 14;

/// Half the X width of a gate cell: the west and east socket approach columns
/// sit at `cx ± GATE_HALF_WIDTH`.
const GATE_HALF_WIDTH: i32 = 4;

/// Z distance between two neighbouring tracks of the same channel.
///
/// This was originally chosen because the old repeater ramp's last
/// descending step left a **strongly powered** support block one layer under
/// the track plane -- a strongly powered block drives every redstone dust
/// next to it in all six directions, so at spacing 4 that block would sit
/// directly beneath the next track and inject this net's signal into it.
/// Five put the ramp's landing on a Z row that could never hold a track.
///
/// The dust staircase that replaced that ramp (`move_between_layers`) has no
/// such block: every solid cell it places is a stair's support, only ever
/// *weakly* powered by the dust sitting on it (`dust_only_weakly_powers_the_
/// block_beneath_it`), and weak power cannot re-drive a neighbour. So the
/// original justification for 5 no longer applies -- but retuning it is a
/// track-spacing change, a size win, not the delay win this module exists to
/// produce right now (see `docs/superpowers/plans/2026-08-07-dust-staircase-
/// ramps.md`, "Out of scope"). Left at 5 deliberately.
const TRACK_SPACING: i32 = 5;

/// How far a ramp travels horizontally while changing layer, *and* how much
/// signal strength it spends doing so -- the two are the same number here,
/// not a coincidence pasted together.
///
/// The old ramp was built from repeaters and spent two blocks of horizontal
/// travel per Y level (a repeater, then the support block it drove). A dust
/// staircase instead climbs or descends via `redstone::simulator::
/// connectivity::dust_connections`' diagonal rule, which connects one dust
/// cell straight to the next diagonal one -- exactly one block of horizontal
/// travel per Y level, not two. So the footprint halves to `TRACK_Y - GATE_Y`.
///
/// That diagonal connection is still a single hop of `dust_connections`, and
/// `recompute_dust_strengths`'s BFS spends exactly one strength per hop it
/// walks, same-level or diagonal, without distinguishing them (see its
/// `HORIZONTAL` loop, which is the only place hops are counted). So a ramp
/// that changes `RAMP_LENGTH` levels also spends exactly `RAMP_LENGTH`
/// signal strength -- one block of horizontal footprint, one point of
/// strength, per level, always in lockstep. `MAX_DUST_RUN`'s comment below
/// derives what that costs the surrounding dust runs.
const RAMP_LENGTH: i32 = TRACK_Y - GATE_Y;

/// Minimum X gap between two nets that share one track.
const TRACK_SHARE_GAP: i32 = 4;

/// Minimum X gap between two routing columns. Redstone dust connects to its
/// four horizontal neighbours, so one empty block between two columns is
/// already enough -- but only exactly enough, which is why every other
/// clearance in this module is derived from it rather than written out.
const COLUMN_CLEARANCE: i32 = 2;

/// The largest X gap between a net's source column and its one sink's
/// approach column that a direct `GATE_Y` route -- no ramp, no track -- can
/// bridge without ever risking another net's column.
///
/// Every column this router places -- a net's source column, a gate's socket
/// approach column, or a feed-through hop -- sits at least
/// `COLUMN_CLEARANCE` away from every other one in the same channel (see
/// `reserve_columns`'s own doc comment, and the shifted row placement in
/// `build_floorplan` that makes it true even before a single feed-through is
/// chosen). A third column strictly between two points `d` apart would
/// itself have to be at least `COLUMN_CLEARANCE` away from *both* of them,
/// which needs `d >= 2 * COLUMN_CLEARANCE`. So whenever `d < 2 *
/// COLUMN_CLEARANCE`, no such column can exist anywhere in the world, at any
/// Z -- not merely unlikely to -- and a direct horizontal jog between the two
/// is provably clear of every other net's dust. That is what makes this a
/// derived threshold rather than a tuned one; see `compute_bypass`, the only
/// caller.
const BYPASS_MAX_DISTANCE: i32 = 2 * COLUMN_CLEARANCE - 1;

/// The largest X gap `resolve_bypass_and_geometry` will even *ask* about --
/// past `BYPASS_MAX_DISTANCE`, where the proof above no longer applies and
/// the answer has to come from an actual `Reservation` query instead of a
/// geometric guarantee.
///
/// This is a measured cutoff, not a derived one -- see
/// `resolve_bypass_and_geometry`'s doc comment for the numbers it was picked
/// from. Widening the *query* range costs nothing by itself (a query that
/// finds a collision just falls back to the ramp/track route it would have
/// taken anyway); what stops this from growing without bound is that a
/// candidate this far out is asking a horizontal jog to run past
/// `BYPASS_MAX_DISTANCE`'s own row-body margin (`GATE_HALF_WIDTH +
/// COLUMN_CLEARANCE` from the *next* gate in the same row, `SLOT_PITCH` away
/// from this one) -- so past a certain distance every remaining candidate is
/// rejected by `jog_crosses_another_row_zone` anyway, and asking further out
/// only spends compile time, not correctness.
const BYPASS_QUERY_MAX_DISTANCE: i32 = 12;

/// West edge of the floorplan. Everything is laid out eastwards from here, so
/// no coordinate can go negative on the X axis.
const ORIGIN_X: i32 = 8;

/// `lay_dust_run`'s `start` (or a track's `source_x`) is not always a fresh
/// signal source anymore -- it can also be a ramp's landing, which arrives
/// carrying less than the full 15. `MAX_SIGNAL_STRENGTH - incoming_strength`
/// is how much of that budget is already spent before this run's first cell;
/// counting from there (instead of from zero) is what keeps a long run whose
/// *input* is already weakened from silently running the wire dead before a
/// repeater would otherwise have been due -- see `plan_straight_run` and
/// `plan_track_run`, which both start their counters this way.
///
/// A cell one hop past an active source (lever, torch-adjacent dust, or a
/// repeater's output) is at strength 14; each further hop of flat dust costs
/// exactly 1 more (`recompute_dust_strengths`); the 14th hop is still alive
/// at strength 1, and the 15th would be 0 -- dead
/// (`signal_strength_falls_off_over_distance_in_a_real_circuit` verifies
/// exactly this boundary). So at most 14 dust cells may follow a fresh
/// source before a repeater is mandatory.
///
/// A run that empties into a ramp cannot spend the full 14, though: the ramp
/// itself is `RAMP_LENGTH` more hops (see that constant's comment) that has
/// to survive on whatever strength is left when the flat run hands off to
/// it, and a ramp cannot absorb a repeater partway up -- a repeater cannot
/// change Y. So such a run's last cell must still be carrying at least
/// `RAMP_LENGTH + 1` (enough to survive `RAMP_LENGTH` more hops and land on
/// a live 1, not a dead 0), which means it may only spend
/// `MAX_DUST_RUN - RAMP_LENGTH` cells per refresh cycle, not the full budget
/// -- `plan_straight_run` and `plan_track_run` take that shortfall as their
/// `reserve` parameter. Every dust run in this router either starts or ends
/// at a ramp (frequently both), so this reservation, not the bare constant,
/// is what most calls actually use.
const MAX_DUST_RUN: i32 = 14;

/// 編譯過程的錯誤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// 網表裡有迴路
    CyclicNetlist,
    /// 訊號沒有驅動來源
    UndrivenSignal(String),
    /// Two nets' routed dust physically joined into one electrical network --
    /// the connectivity invariant `compile` checks right before it would
    /// otherwise return a circuit (see `verify_connectivity`). This is the
    /// only failure mode this module cannot attribute to a specific input:
    /// it means the router itself produced a world whose actual
    /// connectivity does not match the netlist that asked for it.
    ConnectivityViolation {
        /// The dust cell where the merge was discovered while walking the
        /// network outward from `expected_cell`.
        cell: (i32, i32, i32),
        /// The net `cell` belongs to by the router's own bookkeeping --
        /// not the net whose network reached it.
        found_net: String,
        /// The cell that first established which net this whole dust
        /// network belongs to.
        expected_cell: (i32, i32, i32),
        /// The net `expected_cell` -- and therefore, if the invariant held,
        /// every dust cell reachable from it -- belongs to.
        expected_net: String,
    },
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::CyclicNetlist => write!(f, "netlist has a cycle"),
            CompileError::UndrivenSignal(name) => write!(f, "signal `{name}` is never driven"),
            CompileError::ConnectivityViolation { cell, found_net, expected_cell, expected_net } => {
                write!(
                    f,
                    "connectivity violation: the dust at {cell:?} belongs to net `{found_net}`, \
                     but is electrically connected to the network of net `{expected_net}` \
                     (established at {expected_cell:?})"
                )
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// 編譯完成的電路。
pub struct CompiledCircuit {
    pub world: World,
    /// 每個輸入訊號的拉桿座標
    pub input_positions: BTreeMap<String, (i32, i32, i32)>,
    /// Each output signal's reading point -- the coordinate of the redstone
    /// lamp that lights up when the signal is high, not the internal NOR
    /// gate's output torch. This is what a person standing in front of the
    /// pasted circuit actually looks at.
    pub output_positions: BTreeMap<String, (i32, i32, i32)>,
    /// Every gate's actual output position -- the wall torch that is this
    /// gate's real output -- keyed by the gate's output signal name.
    ///
    /// Unlike `output_positions`, which only covers the netlist's *declared*
    /// outputs and points one game tick further down the line at the
    /// readable lamp, this covers every gate including purely internal ones
    /// with no lamp at all. This is what dynamic timing analysis
    /// (`crate::timing`) watches to measure every net, not just the ones a
    /// person reads -- the lamp's own extra delay
    /// (`LAMP_TURN_ON_DELAY_GAME_TICKS` / `LAMP_TURN_OFF_DELAY_GAME_TICKS`)
    /// is a display convenience, not part of the logic.
    pub gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
}

/// 把一格地板鋪在 `pos` 正下方，讓紅石粉／拉桿／中繼器能立在上面。
fn ensure_floor(world: &mut World, pos: Position) {
    let floor = pos.down();
    world.set(floor.x, floor.y, floor.z, stone());
}

/// 從 `start` 走到 `end`（兩者必須沿同一軸對齊）是哪個方向。
fn direction_from(start: Position, end: Position) -> Facing {
    if end.x != start.x {
        if end.x > start.x { Facing::East } else { Facing::West }
    } else if end.z != start.z {
        if end.z > start.z { Facing::South } else { Facing::North }
    } else {
        unreachable!("direction_from 只處理水平直線上、起訖點不同的兩點")
    }
}

/// Number of cells strictly between `start` and `stop_before` along
/// `direction` -- the same count `lay_dust_run` would place blocks into, if
/// it were called with these same three arguments. Pure position arithmetic,
/// so the "Strength planning" pass in `compile` can use it to size a run
/// before any block for it exists.
fn straight_run_length(start: Position, direction: Facing, stop_before: Position) -> i32 {
    let mut len = 0;
    let mut pos = start.offset(direction);
    while pos != stop_before {
        len += 1;
        pos = pos.offset(direction);
    }
    len
}

/// Decide where repeaters must land along a straight run of `len` cells, and
/// report the strength of the last one -- what continues past it, whether
/// that is another run or a ramp. Pure (no `World`), so it can be replayed
/// during strength planning to learn a value before any block for it exists;
/// `lay_dust_run` below calls it to decide what to write.
///
/// `incoming_strength` is the strength of the cell immediately before this
/// run (the last cell of whatever preceded it: an active component, or
/// another run's own last cell). `reserve` is how much strength must remain
/// unspent in the last cell for whatever immediately follows to survive --
/// see `MAX_DUST_RUN`'s comment for the derivation of `RAMP_LENGTH` as that
/// reserve for a run that empties into a ramp, and 0 for one that empties
/// into a mandatory terminating repeater instead (which cannot die no matter
/// what strength reaches it, as long as it is nonzero).
fn plan_straight_run(len: i32, incoming_strength: u8, reserve: i32) -> (Vec<bool>, u8) {
    debug_assert!(incoming_strength > 0, "a run cannot start from an already-dead signal");
    let threshold = (MAX_DUST_RUN - reserve) as i64;
    let mut is_repeater = vec![false; len.max(0) as usize];
    let mut last_refresh: i64 = incoming_strength as i64 - (MAX_SIGNAL_STRENGTH as i64 + 1);
    let mut strength = incoming_strength;
    for (i, slot) in is_repeater.iter_mut().enumerate() {
        if (i as i64) - last_refresh > threshold {
            *slot = true;
            last_refresh = i as i64;
            strength = MAX_SIGNAL_STRENGTH;
        } else {
            strength -= 1;
        }
    }
    (is_repeater, strength)
}

/// Lay dust from `start` (exclusive, already lit at `incoming_strength`)
/// along `direction` to `stop_before` (exclusive), inserting a repeater
/// wherever `plan_straight_run` decides the budget is spent. Returns the
/// strength of the last cell placed (or `incoming_strength` unchanged if
/// `start` and `stop_before` turn out to be adjacent -- nothing to lay).
fn lay_dust_run(
    world: &mut World,
    start: Position,
    direction: Facing,
    stop_before: Position,
    incoming_strength: u8,
    reserve: i32,
    route: &mut Route,
) -> u8 {
    let len = straight_run_length(start, direction, stop_before);
    let (is_repeater, ending_strength) = plan_straight_run(len, incoming_strength, reserve);

    let mut pos = start.offset(direction);
    for &place_repeater in &is_repeater {
        ensure_floor(world, pos);
        route.claim(pos.down());
        if place_repeater {
            world.set(pos.x, pos.y, pos.z, repeater(direction));
        } else {
            world.set(pos.x, pos.y, pos.z, dust());
        }
        route.claim(pos);
        pos = pos.offset(direction);
    }
    ending_strength
}

/// 鋪一段路線，終點是一個**轉角**：下一段線要換一個軸繼續走，所以這一格
/// 必須是紅石粉，不能是中繼器 —— 中繼器只認一個方向，換軸就接不到訊號。
///
/// 為了保證轉角這一格一定是滿強度 15（讓下一段線可以放心地從頭算起自己
/// 的 15 格預算），轉角**前一格**強制放一個面朝這一段方向的中繼器，不管
/// 前面累計了幾格 —— 中繼器正前方的粉一定是新鮮的滿強度，這是它存在的
/// 意義。呼叫端保證 `start` 到 `corner` 至少有 2 格距離，所以「轉角前一
/// 格」不會撞到 `start` 自己。
///
/// This segment always ends in that mandatory repeater, so it can never die
/// no matter how weak `incoming_strength` is (as long as it is nonzero) --
/// `reserve` is 0.
fn lay_segment_to_corner(world: &mut World, start: Position, corner: Position, incoming_strength: u8, route: &mut Route) {
    let direction = direction_from(start, corner);
    let refresh_point = corner.offset(direction.opposite());

    lay_dust_run(world, start, direction, refresh_point, incoming_strength, 0, route);

    ensure_floor(world, refresh_point);
    route.claim(refresh_point.down());
    world.set(refresh_point.x, refresh_point.y, refresh_point.z, repeater(direction));
    route.claim(refresh_point);

    ensure_floor(world, corner);
    route.claim(corner.down());
    world.set(corner.x, corner.y, corner.z, dust());
    route.claim(corner);
}

/// 鋪一段路線的最後一段，終點是某個閘的輸入插座：這一格**一定**是中繼器，
/// 面朝這一段前進的方向 —— 而這個方向剛好就是「面朝支撐塊」的方向，因為
/// 插座本來就在支撐塊的正對外側。這是唯一能強充能支撐塊的方法：紅石粉
/// 水平方向不會充能方塊。
///
/// Same reasoning as `lay_segment_to_corner`: the mandatory repeater at
/// `socket` means this segment can never die, so `reserve` is 0.
fn lay_segment_to_socket(world: &mut World, start: Position, socket: Position, incoming_strength: u8, route: &mut Route) {
    let direction = direction_from(start, socket);
    lay_dust_run(world, start, direction, socket, incoming_strength, 0, route);
    ensure_floor(world, socket);
    route.claim(socket.down());
    world.set(socket.x, socket.y, socket.z, repeater(direction));
    route.claim(socket);
}

/// Break a multi-segment axis-aligned path from `start` through every point
/// of `waypoints` in order into its individual cells, in write order. Each
/// consecutive pair -- `start`/`waypoints[0]`, then each pair after it --
/// must share exactly one axis, same as every other segment helper here.
/// Pure position arithmetic (mirrors `straight_run_length`'s role for a
/// single segment), so `routing_stats` can replay the exact same cell list
/// `lay_bent_path` below writes without touching a `World`.
fn bent_path_cells(start: Position, waypoints: &[Position]) -> Vec<Position> {
    let mut cells = Vec::new();
    let mut prev = start;
    for &waypoint in waypoints {
        let direction = direction_from(prev, waypoint);
        let mut pos = prev.offset(direction);
        while pos != waypoint {
            cells.push(pos);
            pos = pos.offset(direction);
        }
        cells.push(waypoint);
        prev = waypoint;
    }
    cells
}

/// Lay a multi-segment axis-aligned dust path from `start` (exclusive,
/// already lit at `incoming_strength`) through every point of `waypoints` in
/// order, ending in a mandatory repeater facing into `waypoints`'s last
/// element -- same convention as `lay_segment_to_socket`, which this
/// generalises for a path used by `compute_bypass`'s direct routes: unlike
/// every fixed two-or-three-segment route elsewhere in this module, a bypass
/// may bend zero, one or two times depending on where its one sink sits
/// relative to its source column.
///
/// Every waypoint except the last stays plain dust, because the path
/// changes axis there: a repeater only reads what is directly behind it, so
/// one sitting where the path turns would not be connected to the segment
/// after the turn at all. Unlike `lay_segment_to_corner`, this does *not*
/// force a mandatory refresh at those turns -- a corner costs exactly the
/// same one hop of strength a straight cell does
/// (`recompute_dust_strengths`'s BFS does not distinguish them), so forcing
/// one would spend a repeater a short bypass never needs, which is exactly
/// what made an earlier version of this route regress settle time instead of
/// improving it. Instead the whole path shares one strength budget end to
/// end, exactly as if it had no bends at all, with turns simply excluded
/// from ever hosting the occasional repeater that budget calls for --
/// mirrors `plan_track_run`'s handling of taps it must route around.
///
/// This path always ends in its own mandatory repeater, so nothing after it
/// needs preserved strength -- `reserve` is 0, same as
/// `lay_segment_to_socket`.
fn lay_bent_path(world: &mut World, start: Position, waypoints: &[Position], incoming_strength: u8, route: &mut Route) {
    debug_assert!(!waypoints.is_empty(), "a bent path must have somewhere to end");
    debug_assert!(incoming_strength > 0, "a run cannot start from an already-dead signal");

    let cells = bent_path_cells(start, waypoints);
    let len = cells.len();
    let bend_indices: BTreeSet<usize> = waypoints[..waypoints.len() - 1]
        .iter()
        .map(|&waypoint| {
            cells
                .iter()
                .position(|&cell| cell == waypoint)
                .expect("every waypoint before the last is pushed onto `cells` by `bent_path_cells`")
        })
        .collect();

    // Same decision `plan_track_run` makes, generalised from X-coordinate
    // taps to index-based ones: a repeater must never land on a bend, so
    // when the budget would force one there, it goes on the last non-bend
    // cell before it instead.
    let threshold = MAX_DUST_RUN as i64;
    let mut is_repeater = vec![false; len];
    let mut last_refresh: i64 = incoming_strength as i64 - (MAX_SIGNAL_STRENGTH as i64 + 1);
    let mut i = 0usize;
    while i < len {
        if (i as i64) - last_refresh <= threshold {
            i += 1;
            continue;
        }
        let mut j = i;
        while (j as i64) > last_refresh + 1 && bend_indices.contains(&j) {
            j -= 1;
        }
        debug_assert!(
            !bend_indices.contains(&j),
            "bends must never be dense enough to leave no room for a repeater"
        );
        is_repeater[j] = true;
        last_refresh = j as i64;
        i = j + 1;
    }
    // The final cell is a mandatory repeater regardless of the budget --
    // `waypoints`'s last element is never a bend, so this can never collide
    // with `bend_indices`.
    is_repeater[len - 1] = true;

    let mut prev = start;
    for (index, &pos) in cells.iter().enumerate() {
        let direction = direction_from(prev, pos);
        ensure_floor(world, pos);
        route.claim(pos.down());
        if is_repeater[index] {
            world.set(pos.x, pos.y, pos.z, repeater(direction));
        } else {
            world.set(pos.x, pos.y, pos.z, dust());
        }
        route.claim(pos);
        prev = pos;
    }
}

/// The two horizontal directions perpendicular to `direction` -- the only
/// two `seal_cross_talk` may ever seal.
///
/// This is a fact about *this router's own layout* (a staircase only ever
/// writes its direction of travel and the reverse, so only the sides are
/// ever still open when a landing is placed), not a copy of any connection
/// rule -- the rule itself (which of those side cells actually matters) is
/// `dust_reach`'s job, called below.
fn side_directions(direction: Facing) -> [Facing; 2] {
    match direction {
        Facing::North | Facing::South => [Facing::East, Facing::West],
        Facing::East | Facing::West => [Facing::North, Facing::South],
        Facing::Up | Facing::Down => unreachable!("a ramp only ever travels horizontally"),
    }
}

/// Which net a conductor cell belongs to. Built once, before a single seal
/// is written for real, by replaying the whole circuit's placement against a
/// throwaway world (`Footprint::record`) -- so the real pass already knows,
/// for every net, exactly which cells *every other net* will end up using,
/// not just whatever happens to already be on the page.
type Reservation = HashMap<Position, usize>;

/// The state one `emit` pass threads through every routing write.
///
/// A keep-out cell only ever needs stone if nothing else in the entire
/// circuit will ever legitimately occupy it -- and that question can only be
/// answered once every net's footprint is known, not one net at a time as
/// the router happens to visit them. So `emit` runs twice: once recording,
/// against a throwaway world, to build a complete `Reservation`; once
/// enforcing, against the real world, consulting that now-complete
/// reservation before it ever writes a seal.
struct Footprint {
    reservation: Reservation,
    recording: bool,
}

impl Footprint {
    fn record() -> Self {
        Footprint { reservation: Reservation::new(), recording: true }
    }

    fn enforce(reservation: Reservation) -> Self {
        Footprint { reservation, recording: false }
    }

    /// Record that `pos` is this net's conductor cell -- dust, a repeater,
    /// or a block that physically supports either. A no-op once the
    /// reservation is complete (`recording == false`): nothing should still
    /// be discovering new cells at that point, only consulting them.
    fn claim(&mut self, pos: Position, net: usize) {
        if self.recording {
            self.reservation.insert(pos, net);
        }
    }
}

/// Which net a routing write belongs to, and where to record the cells it
/// touches -- bundled into one value purely so the low-level writers below
/// take one parameter instead of two (`net` and `footprint` always travel
/// together; every one of them ends up wanting both).
struct Route<'a> {
    net: usize,
    footprint: &'a mut Footprint,
}

impl Route<'_> {
    fn claim(&mut self, pos: Position) {
        self.footprint.claim(pos, self.net);
    }
}

/// Seal `pos`'s keep-out cells -- the neighbours that `dust_reach` says would
/// join its net if they held dust -- wherever the completed reservation shows
/// a *different* net will actually occupy one.
///
/// The other two cases are both left alone, and for the same underlying
/// reason: this router always writes unconditionally, so a seal placed here
/// now would simply be overwritten by whatever legitimately comes later --
/// there is no case where materialising stone changes the outcome, only ones
/// where it would either be redundant or futile:
///
/// - **Unclaimed** (`reservation` has no entry): nothing in the whole circuit
///   ever touches this cell, so it stays air either way and an unreachable
///   cell can never join anything. This is the case that made the old,
///   undirected `seal_cross_talk` expensive -- it sealed every such cell on
///   every ramp step, unconditionally, and every one of those blocks turns
///   out to have been unnecessary (see this module's own doc comment on
///   `compile`'s size, or the spacing-model spec's "Success" section).
/// - **This net's own cell**: whatever gets written here later is exactly
///   the connection this cell is supposed to carry.
///
/// Only the remaining case -- reserved for a *different* net -- is an actual
/// keep-out violation, and even then sealing it cannot prevent the
/// connection (the other net's write overwrites the seal regardless). It is
/// sealed anyway, as a physically visible marker of a threat that
/// `verify_connectivity` is what actually catches, loudly, once the world is
/// finished -- this is defence in depth, not the mechanism the invariant
/// depends on. In practice this case has never fired on any of this
/// project's reference circuits: their existing column/track clearances
/// already keep every net's footprint clear of every other's.
///
/// Only ever called while enforcing (`Footprint::recording == false`) --
/// during the recording pass the reservation is necessarily incomplete, so a
/// sealing decision made from it could not be trusted.
fn seal_cross_talk(world: &mut World, pos: Position, direction: Facing, route: &Route) {
    debug_assert!(!route.footprint.recording, "sealing must only happen once the reservation is complete");
    for side in side_directions(direction) {
        for candidate in dust_reach(world, pos, side).iter() {
            if world.get(candidate.x, candidate.y, candidate.z).kind != BlockKind::Air {
                continue;
            }
            match route.footprint.reservation.get(&candidate) {
                // Nobody will ever occupy this cell -- leaving it air costs
                // nothing, because nothing can ever turn it into a live
                // connection either. Sealing it would only spend a block to
                // guard against a write that will never happen.
                None => {}
                // This net's own material: whatever comes later here is
                // exactly the connection this cell is supposed to carry.
                Some(&owner) if owner == route.net => {}
                // A different net will legitimately place its own conductor
                // here. Sealing cannot stop that connection -- this router
                // always writes unconditionally, so any later write simply
                // overwrites the seal -- but it is a certain, not merely
                // possible, cross-talk threat, so it is worth making
                // physically obvious rather than silently trusting the later
                // write to happen to land exactly here. `verify_connectivity`
                // is what actually catches this, loudly, once the world is
                // finished; this is defence in depth, not the mechanism the
                // invariant depends on.
                Some(_) => {
                    world.set(candidate.x, candidate.y, candidate.z, stone());
                }
            }
        }
    }
}

/// Move a signal from `entry` (already lit, at whatever strength the caller
/// arranged for) along `direction` to height `target_y`, one dust staircase
/// step per Y level, and return the position of the final landing dust.
///
/// Climbing and descending are *not* mirror images of each other: they are
/// governed by two different, deliberately asymmetric halves of
/// `redstone::simulator::connectivity::dust_connections` (see that module's
/// doc comment for why).
///
/// - **Climb** requires the horizontal neighbour to be a conductor (the
///   "step" the next dust sits on) and requires the current cell's own space
///   above to stay open. So ascending places a solid riser one block ahead
///   at the current height, then lands new dust directly on top of it, one
///   level higher; `current`'s own space above is simply never written, so
///   it stays open air as the rule requires.
/// - **Descend** requires the opposite: the horizontal neighbour must stay
///   open, and the wire it connects to sits one level *below* that
///   neighbour. So descending lands new dust one block ahead and one level
///   down, with its own floor for support; the cell directly above that
///   landing (at the current cell's own height) is never written, so it
///   stays open.
///
/// Either direction spends exactly one signal strength per level walked
/// (`RAMP_LENGTH`'s comment derives why) and places no repeater -- the
/// caller is responsible for having reserved that strength in whatever run
/// feeds this ramp (`MAX_DUST_RUN`'s comment).
fn move_between_layers(world: &mut World, entry: Position, direction: Facing, target_y: i32, route: &mut Route) -> Position {
    let mut current = entry;
    if target_y >= current.y {
        while current.y != target_y {
            let riser = current.offset(direction);
            world.set(riser.x, riser.y, riser.z, stone());
            route.claim(riser);
            let landing = riser.up();
            world.set(landing.x, landing.y, landing.z, dust());
            route.claim(landing);
            if !route.footprint.recording {
                seal_cross_talk(world, landing, direction, route);
            }
            current = landing;
        }
    } else {
        while current.y != target_y {
            let stepped = current.offset(direction);
            let landing = Position::new(stepped.x, stepped.y - 1, stepped.z);
            ensure_floor(world, landing);
            route.claim(landing.down());
            world.set(landing.x, landing.y, landing.z, dust());
            route.claim(landing);
            if !route.footprint.recording {
                seal_cross_talk(world, landing, direction, route);
            }
            current = landing;
        }
    }
    current
}

/// Same decision as `plan_straight_run`, but for one direction of a track: a
/// run that must also never land a repeater on one of `taps`' cells -- a
/// repeater only reads what is directly behind it, so a wire that turns on
/// top of one is not connected at all. When the budget would force a
/// repeater onto a tap, it goes on the last non-tap cell before it instead;
/// the run after it is then shorter than the budget, never longer. Pure,
/// like `plan_straight_run`, so it can be replayed during strength planning
/// without a `World`.
fn plan_track_run(
    cells: &[i32],
    incoming_strength: u8,
    reserve: i32,
    taps: &BTreeSet<i32>,
) -> (Vec<bool>, Vec<u8>) {
    debug_assert!(incoming_strength > 0, "a run cannot start from an already-dead signal");
    let threshold = (MAX_DUST_RUN - reserve) as i64;

    // Pick the repeater cells before writing anything: where one repeater
    // ends up decides how much budget the cells after it have.
    let mut is_repeater = vec![false; cells.len()];
    let mut last_refresh: i64 = incoming_strength as i64 - (MAX_SIGNAL_STRENGTH as i64 + 1);
    let mut i = 0usize;
    while i < cells.len() {
        if (i as i64) - last_refresh <= threshold {
            i += 1;
            continue;
        }
        let mut j = i;
        while (j as i64) > last_refresh + 1 && taps.contains(&cells[j]) {
            j -= 1;
        }
        debug_assert!(
            !taps.contains(&cells[j]),
            "taps must never be dense enough to leave no room for a repeater"
        );
        is_repeater[j] = true;
        last_refresh = j as i64;
        i = j + 1;
    }

    let mut strengths = vec![0u8; cells.len()];
    let mut strength = incoming_strength;
    for (k, &placed_repeater) in is_repeater.iter().enumerate() {
        if placed_repeater {
            strength = MAX_SIGNAL_STRENGTH;
        } else {
            strength -= 1;
        }
        strengths[k] = strength;
    }
    (is_repeater, strengths)
}

/// The strength a track would deliver at each of `taps`, if laid with
/// `incoming_strength` arriving at `source_x` -- without touching a `World`.
///
/// This exists for the "Strength planning" pass in `compile`: a descending
/// ramp needs to know what the track hands it before it can tell the column
/// after it what it is starting from, but the track itself is not actually
/// written until the later Tracks pass (world-building order needs Ramps,
/// then Columns, then Tracks -- see the comment where `compile` calls them).
/// `lay_track` runs this identical pure decision again when it actually
/// writes the track, so the two can never silently disagree.
fn track_exit_strengths(
    source_x: i32,
    min_x: i32,
    max_x: i32,
    taps: &BTreeSet<i32>,
    incoming_strength: u8,
) -> BTreeMap<i32, u8> {
    let mut exit_strength = BTreeMap::new();
    for (end, step) in [(min_x, -1i32), (max_x, 1i32)] {
        let length = (end - source_x) * step;
        if length <= 0 {
            continue;
        }
        let cells: Vec<i32> = (1..=length).map(|k| source_x + k * step).collect();
        let (_is_repeater, strengths) = plan_track_run(&cells, incoming_strength, RAMP_LENGTH, taps);
        for (k, &x) in cells.iter().enumerate() {
            if taps.contains(&x) {
                exit_strength.insert(x, strengths[k]);
            }
        }
    }
    exit_strength
}

/// Lay one east-west track: dust from `source_x` out to `min_x` and to
/// `max_x`, with a repeater inserted before the signal can run out (see
/// `plan_track_run`). Returns the strength delivered at each of `taps`,
/// exactly as `track_exit_strengths` already predicted during planning.
///
/// `taps` are the X positions where a ramp joins or leaves the track; every
/// one of them reserves `RAMP_LENGTH` strength for the ramp it feeds (see
/// `MAX_DUST_RUN`'s comment) -- applied to the whole track rather than just
/// at each tap, which is simpler and only ever costs an occasional early
/// repeater on a run that was already close to the 14-cell limit, never a
/// correctness problem.
fn lay_track(
    world: &mut World,
    z: i32,
    source_x: i32,
    span: (i32, i32),
    taps: &BTreeSet<i32>,
    incoming_strength: u8,
    route: &mut Route,
) -> BTreeMap<i32, u8> {
    let (min_x, max_x) = span;
    let mut exit_strength = BTreeMap::new();
    for (end, step) in [(min_x, -1i32), (max_x, 1i32)] {
        let length = (end - source_x) * step;
        if length <= 0 {
            continue;
        }
        let direction = if step > 0 { Facing::East } else { Facing::West };
        let cells: Vec<i32> = (1..=length).map(|k| source_x + k * step).collect();
        let (is_repeater, strengths) = plan_track_run(&cells, incoming_strength, RAMP_LENGTH, taps);

        for (k, &x) in cells.iter().enumerate() {
            let pos = Position::new(x, TRACK_Y, z);
            ensure_floor(world, pos);
            route.claim(pos.down());
            if is_repeater[k] {
                world.set(pos.x, pos.y, pos.z, repeater(direction));
            } else {
                world.set(pos.x, pos.y, pos.z, dust());
            }
            route.claim(pos);
            if taps.contains(&x) {
                exit_strength.insert(x, strengths[k]);
            }
        }
    }
    exit_strength
}

/// 把一個外部輸入的拉桿與它的起始紅石粉畫進世界，回傳這個訊號的來源
/// （拉桿本身的座標，以及它驅動的第一格紅石粉）。
///
/// A lever is an active component, so it drives the dust next to it directly
/// (`power_emitted_by` reports `drives_dust` for a lever in every direction) --
/// no support block in between, unlike a gate's input socket.
///
/// The levers live in row 0, south of every gate row, and signal flow is
/// northwards, so the pin goes on the lever's **north** side: that is the way
/// the route has to leave anyway, and it keeps the route from turning back
/// into the lever and overwriting it with dust.
fn place_primary_input(world: &mut World, home: Position) -> (Position, Position) {
    world.set(home.x, home.y, home.z, lever(false));
    ensure_floor(world, home);

    let pin = home.offset(Facing::North);
    ensure_floor(world, pin);
    world.set(pin.x, pin.y, pin.z, dust());

    (home, pin)
}

/// Where a socket's approach column has to run.
///
/// The final repeater of a route must face the gate's support block, and a
/// repeater only reads from directly behind it, so each socket can only be
/// entered from one side: the west socket from the west, the east socket from
/// the east, and the south socket from the south. The first two therefore turn
/// a corner on the gate's own row, `GATE_HALF_WIDTH` out from the centre; the
/// third is reached by running straight north.
fn approach_column(centre_x: i32, input_index: usize) -> i32 {
    match input_index {
        0 => centre_x - GATE_HALF_WIDTH,
        1 => centre_x + GATE_HALF_WIDTH,
        _ => centre_x,
    }
}

/// Where a net's signal comes from.
#[derive(Debug, Clone, Copy)]
enum Source {
    Lever(usize),
    Gate(usize),
}

/// How a net leaves one channel.
#[derive(Debug, Clone, Copy)]
enum Exit {
    /// Down into one input socket of a gate in the row north of this channel.
    Socket { x: i32, gate: usize, input_index: usize },
    /// Straight on northwards, to rejoin a track in a later channel.
    Feedthrough { x: i32, next_slot: usize },
}

impl Exit {
    fn x(self) -> i32 {
        match self {
            Exit::Socket { x, .. } | Exit::Feedthrough { x, .. } => x,
        }
    }
}

/// One signal, and every channel it has to appear in.
///
/// `channels`, `tracks` and `sinks` are parallel: entry `i` describes the
/// net's presence in channel `channels[i]`. `hops[i]` is the feed-through
/// column that carries it from `channels[i]` to `channels[i + 1]`.
struct Net {
    source: Source,
    source_column: i32,
    channels: Vec<usize>,
    tracks: Vec<usize>,
    sinks: Vec<Vec<(usize, usize)>>,
    hops: Vec<i32>,
}

impl Net {
    fn entry_column(&self, slot: usize) -> i32 {
        if slot == 0 {
            self.source_column
        } else {
            self.hops[slot - 1]
        }
    }

    fn exits(&self, slot: usize, centre_x: &[i32]) -> Vec<Exit> {
        let mut exits: Vec<Exit> = self.sinks[slot]
            .iter()
            .map(|&(gate, input_index)| Exit::Socket {
                x: approach_column(centre_x[gate], input_index),
                gate,
                input_index,
            })
            .collect();
        if slot + 1 < self.channels.len() {
            exits.push(Exit::Feedthrough { x: self.hops[slot], next_slot: slot + 1 });
        }
        exits
    }

    /// The X range this net's track has to span inside channel `slot`.
    fn span(&self, slot: usize, centre_x: &[i32]) -> (i32, i32) {
        let mut lo = self.entry_column(slot);
        let mut hi = lo;
        for exit in self.exits(slot, centre_x) {
            lo = lo.min(exit.x());
            hi = hi.max(exit.x());
        }
        (lo, hi)
    }
}

/// The netlist after levelisation, row ordering and X placement.
struct Floorplan {
    /// Row of each gate. Row 0 holds the levers, so a gate of level `l` is in
    /// row `l + 1`.
    row_of: Vec<usize>,
    /// Gate indices per row, ordered west to east. `rows[0]` is always empty.
    rows: Vec<Vec<usize>>,
    /// Centre X of each gate.
    centre_x: Vec<i32>,
    /// X of each primary input's lever.
    lever_x: Vec<i32>,
}

/// ASAP level of every gate: one row deeper than its deepest predecessor.
/// `order` is topological, so one forward pass is enough.
///
/// # Why not ALAP, or something that uses the slack in between
///
/// ASAP puts a gate as early as its inputs allow with no regard for where
/// its *consumer* sits -- which is exactly what stretches a single-fanout
/// gate like `and4`'s `g5` (`¬d`) across the whole netlist depth even
/// though its only consumer is the very last gate (see this module's
/// top-of-file docs for the full trace). That looked like a real bug, so it
/// was measured, not assumed: two alternatives were implemented and run
/// through the exact same four reference circuits and the same routing
/// pipeline below (ASAP itself unchanged either way -- only which level a
/// gate gets handed to `build_floorplan` differs).
///
/// - **Full ALAP**: every gate sits one row above the *earliest* of its
///   consumers' own ALAP levels; a gate with no consumer is pinned to the
///   deepest level the netlist needs.
/// - **Consumer-pulled**: a gate with *exactly one* consumer is pulled all
///   the way to its ALAP level (right next to that consumer); every gate
///   with zero or more-than-one consumer stays at ASAP. The reasoning
///   going in: a single-fanout gate has nothing to lose by moving next to
///   its one consumer, while a multi-fanout gate would only shorten one
///   consumer's edge at the expense of lengthening the others (the sum of
///   a multi-fanout gate's edge lengths does not depend on where it sits;
///   only a single-fanout edge passes a level shift through unchanged).
///
/// Both looked plausible on paper. Neither won. Numbers (release build,
/// `cargo run --bin build_circuit` for box/blocks, `cargo test --test
/// reference_circuits`/`seven_segment -- --nocapture` for settle,
/// `cargo run --bin routing_cost_report` for bypass count):
///
/// ```text
/// non-air blocks (bounding box unchanged in Y/Z shape terms, only listing the number that matters):
///   circuit         ASAP    ALAP    consumer-pulled
///   and4             571     571     571   (same gate count, ¬d's cost just moves to whichever side is now the long edge)
///   full_adder      2246    2872    3388   (+27.9%, +50.9%)
///   segment_a       6716    8122    8110   (+21.0%, +20.7%)
///   seven_segment  16694   16968   16694   (+1.6%, +0.0%)
///
/// blocks/gate:
///   and4            81.6    81.6    81.6
///   full_adder     102.1   130.5   154.0
///   segment_a      146.0   176.6   176.3
///   seven_segment  198.7   202.0   198.7
///
/// worst-case settle (game ticks):
///   and4              30      30      30
///   full_adder        82      86      86
///   segment_a         94     100      98
///   seven_segment    124     116     124
///
/// bypass edges (direct GATE_Y route, no ramp/track) out of all routed edges:
///   and4             6/10    5/10    5/10
///   full_adder       6/32    6/32    6/32
///   segment_a       15/83   10/83   12/83
///   seven_segment  34/156  30/156  34/156
/// ```
///
/// ASAP wins or ties on every circuit's block count, blocks/gate, and
/// bypass count. Its only loss anywhere is `seven_segment`'s settle time,
/// where full ALAP is 8 ticks faster (116 vs. 124) -- but full ALAP is also
/// 1.6% larger there and meaningfully larger everywhere else, so that one
/// win does not generalise into a reason to switch.
///
/// The `g5`-shaped fix does not actually shrink anything: moving a
/// single-fanin/single-fanout gate within its slack window does not
/// eliminate its long edge, it relocates it -- `g5`'s incoming edge (from a
/// lever, fixed at row 0) and outgoing edge (to `g6`, fixed at the deepest
/// row) between them always span the netlist's full depth minus one hop,
/// no matter which row `g5` itself occupies; ASAP puts the long hop on the
/// output side, ALAP/consumer-pulled put it on the input side, and `and4`'s
/// identical block count under all three (571) confirms neither is
/// cheaper. Applied netlist-wide, the *only* effect visible in the larger
/// circuits is the downside the module's own docs warned about: pushing
/// gates later crowds the deepest rows (they must now fit more gates,
/// widening the row and the channel under it) while several early-to-middle
/// levels empty out without the channel machinery ever letting an empty
/// level cost zero (`assign_tracks`/`layout_z` always reserve at least one
/// track's depth per channel) -- pure overhead, no shorter critical path to
/// show for it.
///
/// Kept: ASAP, unmodified.
fn compute_asap_levels(netlist: &Netlist, order: &[usize], producer_of: &HashMap<&str, usize>) -> Vec<usize> {
    let mut level = vec![0usize; netlist.gates.len()];
    for &g in order {
        let mut deepest = 0usize;
        for input in &netlist.gates[g].inputs {
            if let Some(&p) = producer_of.get(input.as_str()) {
                deepest = deepest.max(level[p] + 1);
            }
        }
        level[g] = deepest;
    }
    level
}

/// Levelise the DAG, order each row by barycentre, and give every gate an X.
fn build_floorplan(
    netlist: &Netlist,
    order: &[usize],
    producer_of: &HashMap<&str, usize>,
) -> Floorplan {
    let gate_count = netlist.gates.len();

    let level = compute_asap_levels(netlist, order, producer_of);
    let level_count = level.iter().copied().max().map_or(0, |m| m + 1);
    let row_count = level_count + 1;

    let mut rows: Vec<Vec<usize>> = vec![Vec::new(); row_count];
    for &g in order {
        rows[level[g] + 1].push(g);
    }

    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); gate_count];
    for (g, gate) in netlist.gates.iter().enumerate() {
        for input in &gate.inputs {
            if let Some(&p) = producer_of.get(input.as_str()) {
                consumers[p].push(g);
            }
        }
    }

    let input_slot: HashMap<&str, usize> = netlist
        .inputs
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();

    let row_len: Vec<usize> = rows.iter().map(Vec::len).collect();
    let widest = row_len
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .max(netlist.inputs.len())
        .max(1);

    let mut slot = vec![0usize; gate_count];
    for row in &rows {
        for (i, &g) in row.iter().enumerate() {
            slot[g] = i;
        }
    }

    // Barycentre ordering. Sweeping down puts each gate near the average
    // position of what feeds it; sweeping back up puts it near the average
    // position of what it feeds. Both reduce crossings, and fewer crossings
    // mean narrower net spans -- which is exactly what lets the left-edge
    // track assignment below pack more nets onto one track.
    let spread = |position: usize, len: usize| -> f64 {
        (position as f64 + 0.5) * (widest as f64 / len.max(1) as f64)
    };
    let lever_spread = |name: &str| -> Option<f64> {
        input_slot
            .get(name)
            .map(|&i| spread(i, netlist.inputs.len()))
    };

    for _ in 0..3 {
        for r in 1..row_count {
            let mut keyed: Vec<(f64, usize, usize)> = rows[r]
                .iter()
                .enumerate()
                .map(|(i, &g)| {
                    let mut sum = 0.0;
                    let mut count = 0.0;
                    for input in &netlist.gates[g].inputs {
                        if let Some(&p) = producer_of.get(input.as_str()) {
                            sum += spread(slot[p], row_len[level[p] + 1]);
                            count += 1.0;
                        } else if let Some(s) = lever_spread(input) {
                            sum += s;
                            count += 1.0;
                        }
                    }
                    let key = if count > 0.0 { sum / count } else { spread(i, row_len[r]) };
                    (key, i, g)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            rows[r] = keyed.iter().map(|entry| entry.2).collect();
            for (i, &g) in rows[r].iter().enumerate() {
                slot[g] = i;
            }
        }
        for r in (1..row_count).rev() {
            let mut keyed: Vec<(f64, usize, usize)> = rows[r]
                .iter()
                .enumerate()
                .map(|(i, &g)| {
                    let mut sum = 0.0;
                    let mut count = 0.0;
                    for &c in &consumers[g] {
                        sum += spread(slot[c], row_len[level[c] + 1]);
                        count += 1.0;
                    }
                    let key = if count > 0.0 { sum / count } else { spread(i, row_len[r]) };
                    (key, i, g)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            rows[r] = keyed.iter().map(|entry| entry.2).collect();
            for (i, &g) in rows[r].iter().enumerate() {
                slot[g] = i;
            }
        }
    }

    // X placement. Rows alternate a `COLUMN_CLEARANCE` shift so that a row's
    // output columns (at its own `cx`) can never land on the next row's socket
    // approach columns (at `cx' - 4`, `cx'` and `cx' + 4`): with the shift,
    // every pair of columns meeting in one channel is at least
    // `COLUMN_CLEARANCE` apart, which is what keeps two unrelated signals from
    // running side by side.
    let mut centre_x = vec![0i32; gate_count];
    for (r, row) in rows.iter().enumerate() {
        let shift = if r % 2 == 0 { 0 } else { COLUMN_CLEARANCE };
        let left = ((widest - row.len()) / 2) as i32 * SLOT_PITCH;
        for (i, &g) in row.iter().enumerate() {
            centre_x[g] = ORIGIN_X + left + i as i32 * SLOT_PITCH + GATE_HALF_WIDTH + shift;
        }
    }
    let lever_left = ((widest - netlist.inputs.len()) / 2) as i32 * SLOT_PITCH;
    let lever_x: Vec<i32> = (0..netlist.inputs.len())
        .map(|i| ORIGIN_X + lever_left + i as i32 * SLOT_PITCH + GATE_HALF_WIDTH)
        .collect();

    let row_of: Vec<usize> = level.iter().map(|&l| l + 1).collect();

    Floorplan { row_of, rows, centre_x, lever_x }
}

/// Collect the nets: one per driven signal that actually has a sink.
fn build_nets(
    netlist: &Netlist,
    order: &[usize],
    plan: &Floorplan,
    producer_of: &HashMap<&str, usize>,
) -> Vec<Net> {
    let input_slot: HashMap<&str, usize> = netlist
        .inputs
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();

    // Signal name -> index into `nets`, built in a fixed order so that two
    // compiles of the same netlist produce the same world.
    let mut index_of: HashMap<String, usize> = HashMap::new();
    let mut nets: Vec<Net> = Vec::new();
    let mut sinks_of: Vec<Vec<(usize, usize)>> = Vec::new();

    for &g in order {
        for (input_index, input) in netlist.gates[g].inputs.iter().enumerate() {
            let net = match index_of.get(input.as_str()) {
                Some(&i) => i,
                None => {
                    let (source, column) = if let Some(&i) = input_slot.get(input.as_str()) {
                        (Source::Lever(i), plan.lever_x[i])
                    } else {
                        let driver = *producer_of.get(input.as_str()).expect(
                            "every input was checked to be driven before placement started",
                        );
                        (Source::Gate(driver), plan.centre_x[driver])
                    };
                    let i = nets.len();
                    nets.push(Net {
                        source,
                        source_column: column,
                        channels: Vec::new(),
                        tracks: Vec::new(),
                        sinks: Vec::new(),
                        hops: Vec::new(),
                    });
                    sinks_of.push(Vec::new());
                    index_of.insert(input.clone(), i);
                    i
                }
            };
            sinks_of[net].push((g, input_index));
        }
    }

    // Turn each net's sink list into the per-channel structure. Channel `c`
    // lies between row `c` and row `c + 1`, so a sink in row `t` is fed from
    // channel `t - 1`, and the net's own source row is always a channel too:
    // when the nearest sink is further away, that first channel is where the
    // feed-through starts.
    for (i, net) in nets.iter_mut().enumerate() {
        let source_row = match net.source {
            Source::Lever(_) => 0,
            Source::Gate(g) => plan.row_of[g],
        };
        let mut by_channel: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
        by_channel.entry(source_row).or_default();
        for &(gate, input_index) in &sinks_of[i] {
            by_channel
                .entry(plan.row_of[gate] - 1)
                .or_default()
                .push((gate, input_index));
        }
        for (channel, sinks) in by_channel {
            net.channels.push(channel);
            net.sinks.push(sinks);
        }
        net.tracks = vec![0; net.channels.len()];
    }

    nets.retain(|net| net.sinks.iter().any(|s| !s.is_empty()));
    nets
}

/// Which nets can skip the ramp/track machinery entirely and connect
/// straight across at `GATE_Y` -- see `BYPASS_MAX_DISTANCE` for why a small
/// X gap makes that provably safe.
///
/// Deliberately restricted to nets with exactly one channel (no
/// feed-through: a bypass never has to survive crossing into a second
/// channel, so it never has to reason about a hop column it does not fully
/// control) and exactly one sink (fan-out would need more than one jog
/// leaving the same trunk cell; two independently-planned corners could
/// disagree about a shared cell's repeater placement, which is exactly the
/// kind of bug this module's whole `Footprint`/`verify_connectivity` apparatus
/// exists to catch, not something worth risking for a case this router does
/// not need to handle yet). Every other net keeps routing exactly as before.
///
/// Pure function of the floorplan and each net's channel/sink structure --
/// like `reserve_columns` and `assign_tracks`, no world access, so it can run
/// before either of them and its answer feeds straight into `assign_tracks`
/// (a bypassed net needs no track at all).
///
/// This is the proof-only half of bypass eligibility. `resolve_bypass_and_
/// geometry` widens the *distance* this reaches beyond `BYPASS_MAX_DISTANCE`
/// by asking an actual `Reservation` instead of proving one, but keeps this
/// function's single-channel/single-sink restriction exactly as is for that
/// wider query too -- fan-out and feed-through bypasses are a different, and
/// harder, problem (see `resolve_bypass_and_geometry`'s own doc comment).
fn compute_bypass(nets: &[Net], plan: &Floorplan) -> Vec<bool> {
    nets.iter()
        .map(|net| {
            if net.channels.len() != 1 || net.sinks[0].len() != 1 {
                return false;
            }
            let (gate, input_index) = net.sinks[0][0];
            let exit_x = approach_column(plan.centre_x[gate], input_index);
            (exit_x - net.source_column).abs() <= BYPASS_MAX_DISTANCE
        })
        .collect()
}

/// Reserve one X column for a feed-through, clear of everything it would run
/// past on its way north.
///
/// A feed-through is a plain dust run at `GATE_Y` that passes straight through
/// whole gate rows, so it has to miss both the rows' bodies and every other
/// routing column in every channel it crosses. Candidates are searched
/// outwards from `target` so that a feed-through stays near the rest of its
/// net -- a column parked far away would stretch that net's track and push the
/// channel's track count up.
fn reserve_feedthrough(
    target: i32,
    channels: std::ops::RangeInclusive<usize>,
    rows: std::ops::RangeInclusive<usize>,
    used_columns: &[BTreeSet<i32>],
    row_blocked: &[Vec<(i32, i32)>],
    west_limit: i32,
) -> i32 {
    let channels: Vec<usize> = channels.collect();
    let rows: Vec<usize> = rows.collect();
    let fits = |x: i32| -> bool {
        if rows.iter().any(|&r| {
            row_blocked[r]
                .iter()
                .any(|&(lo, hi)| x >= lo && x <= hi)
        }) {
            return false;
        }
        !channels.iter().any(|&c| {
            used_columns[c]
                .iter()
                .any(|&used| (x - used).abs() < COLUMN_CLEARANCE)
        })
    };

    let centre = target - target.rem_euclid(2);
    for step in 0.. {
        let east = centre + 2 * step;
        if fits(east) {
            return east;
        }
        let west = centre - 2 * step;
        if west >= west_limit && fits(west) {
            return west;
        }
    }
    unreachable!("the search walks east without bound, so it always terminates")
}

/// Every row's keep-out X intervals: a lever's clearance zone (row 0) or a
/// gate's body padded by `COLUMN_CLEARANCE` on both sides (every other row).
/// Nothing may run a column, a feed-through, or (see
/// `resolve_bypass_and_geometry`'s widened bypass jog) a horizontal jog
/// through one of these without risking that row's own hardware.
///
/// Extracted out of `reserve_columns` so `resolve_bypass_and_geometry` can
/// run the identical check against a *candidate* jog before `reserve_columns`
/// itself has any reason to care about one -- a gate or lever body is never a
/// conductor, so it never shows up in a `Reservation` the way another net's
/// dust would, and this is the only place that keep-out is recorded at all.
fn row_body_zones(plan: &Floorplan, row_count: usize) -> Vec<Vec<(i32, i32)>> {
    let mut row_blocked: Vec<Vec<(i32, i32)>> = vec![Vec::new(); row_count];
    for &x in &plan.lever_x {
        row_blocked[0].push((x - COLUMN_CLEARANCE + 1, x + COLUMN_CLEARANCE - 1));
    }
    for (g, &cx) in plan.centre_x.iter().enumerate() {
        row_blocked[plan.row_of[g]].push((
            cx - GATE_HALF_WIDTH - COLUMN_CLEARANCE + 1,
            cx + GATE_HALF_WIDTH + COLUMN_CLEARANCE - 1,
        ));
    }
    row_blocked
}

/// Column reservation: fill in every net's `hops` (the feed-through columns
/// that connect consecutive channels it has to appear in). Pure function of
/// the floorplan and each net's channel/sink structure -- no world access,
/// so it is exactly as reusable by routing analysis as it is by `compile`.
///
/// Extracted verbatim from `compile`'s "Column reservation" section; see the
/// comment there (still in place) for why the forced columns come first and
/// feed-throughs are placed last, against everything else.
fn reserve_columns(plan: &Floorplan, nets: &mut [Net], row_count: usize, channel_count: usize) {
    let mut used_columns: Vec<BTreeSet<i32>> = vec![BTreeSet::new(); channel_count.max(1)];
    let row_blocked = row_body_zones(plan, row_count);

    for net in nets.iter() {
        used_columns[net.channels[0]].insert(net.source_column);
        for (slot, &channel) in net.channels.iter().enumerate() {
            for &(gate, input_index) in &net.sinks[slot] {
                used_columns[channel].insert(approach_column(plan.centre_x[gate], input_index));
            }
        }
    }

    for net in nets.iter_mut() {
        for slot in 0..net.channels.len().saturating_sub(1) {
            let from = net.channels[slot];
            let to = net.channels[slot + 1];
            let target = net.entry_column(slot);
            let column = reserve_feedthrough(
                target,
                from..=to,
                (from + 1)..=to,
                &used_columns,
                &row_blocked,
                ORIGIN_X - GATE_HALF_WIDTH,
            );
            for columns in used_columns[from..=to].iter_mut() {
                columns.insert(column);
            }
            net.hops.push(column);
        }
    }
}

/// Left-edge track assignment: fill in every net's per-slot `tracks` index,
/// and return how many tracks each channel ended up needing.
///
/// A net for which `bypass[n]` holds contributes no member at all -- it
/// connects directly at `GATE_Y` (see `compute_bypass`) and never touches a
/// track, so it must not inflate the channel's track count or claim a track
/// index nobody will ever lay.
///
/// Extracted verbatim from `compile`'s "Left-edge track assignment" section;
/// see the comment there for why one track can carry many nets.
fn assign_tracks(plan: &Floorplan, nets: &mut [Net], channel_count: usize, bypass: &[bool]) -> Vec<usize> {
    let mut track_count = vec![0usize; channel_count];
    for (channel, count) in track_count.iter_mut().enumerate() {
        let mut members: Vec<(i32, i32, usize, usize)> = Vec::new();
        for (n, net) in nets.iter().enumerate() {
            if bypass[n] {
                continue;
            }
            for (slot, &c) in net.channels.iter().enumerate() {
                if c == channel {
                    let (lo, hi) = net.span(slot, &plan.centre_x);
                    members.push((lo, hi, n, slot));
                }
            }
        }
        members.sort_by_key(|&(lo, _, n, slot)| (lo, n, slot));

        let mut track_end: Vec<i32> = Vec::new();
        for (lo, hi, n, slot) in members {
            let track = match track_end.iter().position(|&end| lo - end >= TRACK_SHARE_GAP) {
                Some(t) => t,
                None => {
                    track_end.push(i32::MIN / 2);
                    track_end.len() - 1
                }
            };
            track_end[track] = hi;
            nets[n].tracks[slot] = track;
        }
        *count = track_end.len();
    }
    track_count
}

/// Z layout: each row's Z and every channel's track Zs, laid out northwards
/// from row 0 and then shifted back into the non-negative range.
///
/// Extracted verbatim from `compile`'s "Z layout" section; see the comment
/// there for the per-channel depth derivation.
fn layout_z(row_count: usize, channel_count: usize, track_count: &[usize]) -> (Vec<i32>, Vec<Vec<i32>>) {
    let mut row_z = vec![0i32; row_count];
    let mut track_z: Vec<Vec<i32>> = vec![Vec::new(); channel_count];
    for channel in 0..channel_count {
        // Three blocks clear of the row's own south socket leaves the first
        // ramp somewhere to start.
        let channel_south = row_z[channel] - 3;
        track_z[channel] = (0..track_count[channel])
            .map(|k| channel_south - TRACK_SPACING * (k as i32 + 1))
            .collect();
        let depth = TRACK_SPACING * track_count[channel].max(1) as i32;
        // The last descending ramp lands `RAMP_LENGTH` north of the last
        // track, and the column then needs room to reach the next row's south
        // socket approach.
        row_z[channel + 1] = channel_south - depth - RAMP_LENGTH - 4;
    }

    let z_offset = 3 - row_z[row_count - 1] + 2;
    for z in &mut row_z {
        *z += z_offset;
    }
    for channel in &mut track_z {
        for z in channel {
            *z += z_offset;
        }
    }
    (row_z, track_z)
}

/// `(entry_strength[net][slot], exit_strength[net][slot][exit_x])` -- see
/// `plan_strengths`.
type StrengthPlan = (Vec<Vec<u8>>, Vec<Vec<BTreeMap<i32, u8>>>);

/// Work out, for every net and every slot it appears in, the signal strength
/// arriving at that slot's ramp entry (`entry_strength[net][slot]`) and the
/// strength the track hands each of that slot's exits before their
/// descending ramp (`exit_strength[net][slot]`, keyed by exit X).
///
/// This has to run before any of the Ramps/Columns/Tracks passes write a
/// single block. The Columns pass needs to know, for a feed-through, what
/// strength survived the previous slot's track and descending ramp before it
/// can lay the column that continues from there -- but that number depends
/// on the *next* slot's track layout, which is not written until the later
/// Tracks pass (world-building order has to stay Ramps, then Columns, then
/// Tracks -- see the comment above the Ramps loop in `compile` for why).
/// Computing every slot's numbers up front, net by net in slot order (each
/// slot's result depends only on the previous slot's, within the same net),
/// sidesteps that ordering conflict entirely: pure arithmetic, no `World`,
/// so it does not care what order the real blocks go down in.
///
/// `plan_straight_run` and `track_exit_strengths` are the exact same
/// decisions `lay_dust_run` and `lay_track` make when they actually write
/// blocks later, so the numbers this produces are guaranteed to match what
/// ends up in the world, not just a plausible estimate of it.
fn plan_strengths(
    nets: &[Net],
    plan: &Floorplan,
    track_z: &[Vec<i32>],
    lever_pin: &[Position],
    gate_pin: &[Position],
    bypass: &[bool],
) -> StrengthPlan {
    let mut entry_strength: Vec<Vec<u8>> = Vec::with_capacity(nets.len());
    let mut exit_strength: Vec<Vec<BTreeMap<i32, u8>>> = Vec::with_capacity(nets.len());

    for (n, net) in nets.iter().enumerate() {
        let mut net_entry = vec![0u8; net.channels.len()];
        let mut net_exit: Vec<BTreeMap<i32, u8>> = vec![BTreeMap::new(); net.channels.len()];

        // A bypassed net never ramps or touches a track (`emit`'s Columns
        // pass routes it directly instead), and its one channel may not even
        // have a `track_z` entry to read (a channel every one of whose nets
        // bypasses gets zero tracks -- see `assign_tracks`). Leaving these at
        // their zero default is safe precisely because nothing downstream
        // ever reads a bypassed net's entry/exit strength.
        if bypass[n] {
            entry_strength.push(net_entry);
            exit_strength.push(net_exit);
            continue;
        }

        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let z = track_z[channel][net.tracks[slot]];
            let entry = Position::new(net.entry_column(slot), GATE_Y, z + RAMP_LENGTH);

            let arriving = if slot == 0 {
                let pin = match net.source {
                    Source::Lever(i) => lever_pin[i],
                    Source::Gate(g) => gate_pin[g],
                };
                let len = straight_run_length(pin, Facing::North, entry.offset(Facing::North));
                plan_straight_run(len, MAX_SIGNAL_STRENGTH, RAMP_LENGTH).1
            } else {
                let prev_channel = net.channels[slot - 1];
                let prev_z = track_z[prev_channel][net.tracks[slot - 1]];
                let feed_x = net.hops[slot - 1];
                let track_strength = net_exit[slot - 1][&feed_x];
                let landing_strength = track_strength - RAMP_LENGTH as u8;
                let landing = Position::new(feed_x, GATE_Y, prev_z - RAMP_LENGTH);
                let len = straight_run_length(landing, Facing::North, entry.offset(Facing::North));
                plan_straight_run(len, landing_strength, RAMP_LENGTH).1
            };
            net_entry[slot] = arriving;

            let track_incoming = arriving - RAMP_LENGTH as u8;
            let source_x = net.entry_column(slot);
            let (lo, hi) = net.span(slot, &plan.centre_x);
            let mut taps: BTreeSet<i32> = BTreeSet::new();
            taps.insert(source_x);
            for exit in net.exits(slot, &plan.centre_x) {
                taps.insert(exit.x());
            }
            net_exit[slot] = track_exit_strengths(source_x, lo, hi, &taps, track_incoming);
        }

        entry_strength.push(net_entry);
        exit_strength.push(net_exit);
    }

    (entry_strength, exit_strength)
}

/// What `emit` produces besides the blocks it writes into `world` --
/// everything `CompiledCircuit` needs that is not the world itself.
struct EmitResult {
    input_positions: BTreeMap<String, (i32, i32, i32)>,
    output_positions: BTreeMap<String, (i32, i32, i32)>,
    gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
}

/// Everything the placement/routing stages before `emit` computed about
/// where things go, bundled into one value purely so `emit` takes one
/// parameter instead of five (`clippy::too_many_arguments`) -- every field
/// here already travels together from `compile` down to both `emit` calls.
struct RoutingGeometry<'a> {
    plan: &'a Floorplan,
    row_z: &'a [i32],
    nets: &'a [Net],
    track_z: &'a [Vec<i32>],
    /// Per-net: whether `compute_bypass` found this net's one sink close
    /// enough to connect directly at `GATE_Y` instead of via ramp and track.
    bypass: &'a [bool],
}

/// Write the whole circuit into `world`: every gate, every primary input,
/// every net's ramps/columns/tracks, and every output lamp.
///
/// This is called twice by `compile` against two different worlds and two
/// different `Footprint` modes (see `Footprint`'s own doc comment) -- once
/// to record where everything ends up before a single seal is written for
/// real, once to actually build the circuit enforcing keep-out against that
/// now-complete picture. Both calls run the exact same code, so the two
/// worlds can never disagree about where anything is -- only about whether
/// the orphaned keep-out cells around a ramp landing got sealed.
fn emit(world: &mut World, netlist: &Netlist, geometry: &RoutingGeometry, footprint: &mut Footprint) -> EmitResult {
    let RoutingGeometry { plan, row_z, nets, track_z, bypass } = *geometry;
    let mut gate_cell: Vec<NorCell> = Vec::with_capacity(netlist.gates.len());
    for _ in 0..netlist.gates.len() {
        gate_cell.push(NorCell { size: (0, 0, 0), input_offsets: Vec::new(), output_offset: (0, 0, 0) });
    }
    for (g, gate) in netlist.gates.iter().enumerate() {
        let origin = (plan.centre_x[g], GATE_Y, row_z[plan.row_of[g]]);
        gate_cell[g] = place_nor_gate(world, origin, gate.inputs.len());
    }

    let mut input_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    let mut lever_pin: Vec<Position> = Vec::with_capacity(netlist.inputs.len());
    for (i, name) in netlist.inputs.iter().enumerate() {
        let home = Position::new(plan.lever_x[i], GATE_Y, row_z[0]);
        let (lever_pos, pin) = place_primary_input(world, home);
        input_positions.insert(name.clone(), (lever_pos.x, lever_pos.y, lever_pos.z));
        lever_pin.push(pin);
    }

    let torch_of = |g: usize, cell: &NorCell| -> Position {
        Position::new(
            plan.centre_x[g] + cell.output_offset.0,
            GATE_Y + cell.output_offset.1,
            row_z[plan.row_of[g]] + cell.output_offset.2,
        )
    };

    let mut gate_pin: Vec<Position> = Vec::with_capacity(netlist.gates.len());
    let mut gate_output_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    for (g, cell) in gate_cell.iter().enumerate() {
        let torch = torch_of(g, cell);
        gate_output_positions.insert(netlist.gates[g].output.clone(), (torch.x, torch.y, torch.z));
        let pin = torch.offset(OUTPUT_DIRECTION);
        ensure_floor(world, pin);
        world.set(pin.x, pin.y, pin.z, dust());
        gate_pin.push(pin);
    }

    // Strength planning: work out what every ramp's entry and every track's
    // exits will carry, before any of them are actually built. See
    // `plan_strengths` for why this has to happen up front rather than
    // inline in the passes below.
    let (entry_strength, exit_strength) = plan_strengths(nets, plan, track_z, &lever_pin, &gate_pin, bypass);

    // Every net's own pin -- the first cell of its route -- belongs to that
    // net too, exactly like everything the passes below claim as they write
    // it. Claiming it here, once, up front covers every net regardless of
    // whether it turns out to have any ramps at all.
    for (n, net) in nets.iter().enumerate() {
        match net.source {
            Source::Lever(i) => footprint.claim(lever_pin[i], n),
            Source::Gate(g) => footprint.claim(gate_pin[g], n),
        }
    }

    // Ramps first. `move_between_layers` seals the blocks around each landing,
    // and a seal only fills air -- so anything that has to run *through* a
    // sealed cell (the tracks, and the columns) has to be laid afterwards to
    // overwrite it.
    for (n, net) in nets.iter().enumerate() {
        if bypass[n] {
            // No ramp at all: `compute_bypass` only admits nets whose one
            // sink is close enough to connect directly at `GATE_Y` (see
            // `BYPASS_MAX_DISTANCE`). Handled entirely in the Columns pass
            // below.
            continue;
        }
        let mut route = Route { net: n, footprint: &mut *footprint };
        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let z = track_z[channel][net.tracks[slot]];
            let entry = Position::new(net.entry_column(slot), GATE_Y, z + RAMP_LENGTH);
            move_between_layers(world, entry, Facing::North, TRACK_Y, &mut route);
            for exit in net.exits(slot, &plan.centre_x) {
                let top = Position::new(exit.x(), TRACK_Y, z);
                move_between_layers(world, top, Facing::North, GATE_Y, &mut route);
            }
        }
    }

    // Columns at `GATE_Y`: from a source pin up to its ramp, and from a ramp's
    // landing on to whatever it feeds.
    for (n, net) in nets.iter().enumerate() {
        let mut route = Route { net: n, footprint: &mut *footprint };

        if bypass[n] {
            // A direct connection: no ramp, no track. `resolve_bypass_and_
            // geometry` guarantees this net has exactly one channel and
            // exactly one sink, and that the direct path is safe -- either
            // proven so by `compute_bypass` (the sink's approach column is
            // within `BYPASS_MAX_DISTANCE` of this net's own source column,
            // close enough that no other net's column can possibly sit
            // between them -- see that constant's derivation), or checked so
            // against the actual `Reservation` and every row it crosses
            // (`resolve_bypass_and_geometry`'s widened pass, for a source/sink
            // gap up to `BYPASS_QUERY_MAX_DISTANCE`). Either way, this exact
            // path -- the same waypoints, laid out the same way below -- is
            // what was checked, so a plain path from the source pin straight
            // to the socket is safe here too.
            //
            // The path bends at most twice: once to get from the pin's own
            // column onto the sink's approach column (skipped if they are
            // already the same column), and once more at the destination
            // row -- the same final jog every socket gets, whether its input
            // is west/east (approach column does not line up with the
            // socket) or south (it does, and this second bend never
            // happens). `lay_bent_path` handles all of that with one shared
            // strength budget end to end, rather than a mandatory refresh at
            // every bend -- see its own doc comment for why that used to
            // regress settle time instead of improving it.
            let (gate, input_index) = net.sinks[0][0];
            let pin = match net.source {
                Source::Lever(i) => lever_pin[i],
                Source::Gate(g) => gate_pin[g],
            };
            let exit_x = approach_column(plan.centre_x[gate], input_index);
            let row_z_gate = row_z[plan.row_of[gate]];
            let (dx, dy, dz) = gate_cell[gate].input_offsets[input_index];
            let socket = Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);

            let mut waypoints: Vec<Position> = Vec::new();
            if pin.x != exit_x {
                waypoints.push(Position::new(exit_x, GATE_Y, pin.z));
            }
            if socket.x != exit_x {
                waypoints.push(Position::new(exit_x, GATE_Y, row_z_gate));
            }
            waypoints.push(socket);
            lay_bent_path(world, pin, &waypoints, MAX_SIGNAL_STRENGTH, &mut route);
            continue;
        }

        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let z = track_z[channel][net.tracks[slot]];
            let entry = Position::new(net.entry_column(slot), GATE_Y, z + RAMP_LENGTH);
            if slot == 0 {
                let pin = match net.source {
                    Source::Lever(i) => lever_pin[i],
                    Source::Gate(g) => gate_pin[g],
                };
                lay_dust_run(
                    world,
                    pin,
                    Facing::North,
                    entry.offset(Facing::North),
                    MAX_SIGNAL_STRENGTH,
                    RAMP_LENGTH,
                    &mut route,
                );
            }
            for exit in net.exits(slot, &plan.centre_x) {
                let landing = Position::new(exit.x(), GATE_Y, z - RAMP_LENGTH);
                let landing_strength = exit_strength[n][slot][&exit.x()] - RAMP_LENGTH as u8;
                match exit {
                    Exit::Socket { gate, input_index, .. } => {
                        let (dx, dy, dz) = gate_cell[gate].input_offsets[input_index];
                        let socket = Position::new(
                            plan.centre_x[gate] + dx,
                            GATE_Y + dy,
                            row_z[plan.row_of[gate]] + dz,
                        );
                        if socket.x == landing.x {
                            lay_segment_to_socket(world, landing, socket, landing_strength, &mut route);
                        } else {
                            let corner =
                                Position::new(landing.x, GATE_Y, row_z[plan.row_of[gate]]);
                            lay_segment_to_corner(world, landing, corner, landing_strength, &mut route);
                            // `corner` always follows `lay_segment_to_corner`'s own
                            // mandatory repeater, so it is always fresh.
                            lay_segment_to_socket(world, corner, socket, MAX_SIGNAL_STRENGTH, &mut route);
                        }
                    }
                    Exit::Feedthrough { x, next_slot } => {
                        let next_channel = net.channels[next_slot];
                        let next_z = track_z[next_channel][net.tracks[next_slot]];
                        let next_entry = Position::new(x, GATE_Y, next_z + RAMP_LENGTH);
                        lay_dust_run(
                            world,
                            landing,
                            Facing::North,
                            next_entry.offset(Facing::North),
                            landing_strength,
                            RAMP_LENGTH,
                            &mut route,
                        );
                    }
                }
            }
        }
    }

    // Tracks last, so they overwrite the ramps' seal blocks where they have to
    // pass through them.
    for (n, net) in nets.iter().enumerate() {
        if bypass[n] {
            // Never touches a track -- see the Ramps pass above.
            continue;
        }
        let mut route = Route { net: n, footprint: &mut *footprint };
        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let z = track_z[channel][net.tracks[slot]];
            let source_x = net.entry_column(slot);
            let (lo, hi) = net.span(slot, &plan.centre_x);
            let mut taps: BTreeSet<i32> = BTreeSet::new();
            taps.insert(source_x);
            for exit in net.exits(slot, &plan.centre_x) {
                taps.insert(exit.x());
            }
            let track_incoming = entry_strength[n][slot] - RAMP_LENGTH as u8;
            lay_track(world, z, source_x, (lo, hi), &taps, track_incoming, &mut route);
        }
    }

    // Every netlist output gets a lamp so a person can read it by eye instead
    // of having to know which buried wall torch to stare at.
    //
    // The obvious placement -- directly above the output torch, the one
    // direction a wall torch's power is never withheld (`power_emitted_toward`:
    // `direction == Facing::Up => full`, and `full.block_power` there is
    // `Strong`) -- turns out to be wrong. Verified in the simulator: it makes
    // `compile()`'s own reference circuits oscillate and never settle. The
    // reason is indirect power: `recompute_dust_strengths` treats *any*
    // strongly-powered solid block as a conduit that recharges every redstone
    // wire adjacent to it, not just wires that are meant to be on that net
    // (this is also how a NOR cell's own merge dust weakly powers its centre
    // block -- the same mechanism, just at `Strong` instead of `Weak`
    // strength). A lamp sitting right above the torch is orthogonally
    // adjacent to that gate's own merge dust one row south of it at the same
    // Y, so a strongly-lit lamp would immediately re-inject the gate's own
    // output back into its input pool -- output high -> merge dust charged ->
    // centre block powered -> torch (and lamp) should go off -> merge dust
    // uncharged -> torch back on -> repeat forever.
    //
    // The safe fix is to only ever weakly power the lamp, since
    // `recompute_dust_strengths` only treats *strongly* powered blocks as
    // conduits (`if kind == BlockPower::Strong`) -- a weakly powered lamp can
    // never leak back into a neighbouring wire, whatever it happens to be
    // adjacent to. Redstone dust only ever weakly powers the block directly
    // *beneath* it (`power_emitted_toward`'s `RedstoneWire` arm), so the lamp
    // goes under the gate's own output pin dust (`gate_pin`), replacing the
    // plain floor block `ensure_floor` already put there to hold that dust up
    // -- a lamp satisfies the same "full top face" support requirement a
    // stone floor does, so nothing else about that dust changes. That
    // location is otherwise never touched by any other net (each gate owns
    // its output column exclusively), so the lamp is guaranteed to be powered
    // by nothing but its own gate's output.
    let mut output_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    for output_name in &netlist.outputs {
        let g = netlist
            .gates
            .iter()
            .position(|gate| &gate.output == output_name)
            .expect("every output was checked to be driven by a gate above");
        let lamp_pos = gate_pin[g].down();
        world.set(lamp_pos.x, lamp_pos.y, lamp_pos.z, lamp());
        output_positions.insert(output_name.clone(), (lamp_pos.x, lamp_pos.y, lamp_pos.z));
    }

    EmitResult { input_positions, output_positions, gate_output_positions }
}

/// The X/Z footprint the world needs to hold `plan`/`nets` laid out with
/// `row_z` -- shared by every world `compile` and `resolve_bypass_and_geometry`
/// allocate (the real one, and the throwaway probe the latter builds).
///
/// `size_x` never depends on `row_z` (every column's X, including a
/// feed-through hop, is fixed by `reserve_columns` before any bypass decision
/// exists at all), so the same value serves the baseline probe's world and
/// the final one; only `size_z` moves with `row_z`.
fn world_size(plan: &Floorplan, nets: &[Net], row_z: &[i32]) -> (i32, i32) {
    let size_x = plan
        .centre_x
        .iter()
        .chain(plan.lever_x.iter())
        .copied()
        .max()
        .unwrap_or(ORIGIN_X)
        .max(
            nets.iter()
                .flat_map(|net| net.hops.iter())
                .copied()
                .max()
                .unwrap_or(ORIGIN_X),
        )
        + GATE_HALF_WIDTH
        + 4;
    let size_z = row_z[0] + 4;
    (size_x, size_z)
}

/// One gate cell's socket geometry per distinct input count (1..=3) --
/// `NorCell`'s offsets are relative, so they do not depend on where a given
/// gate actually sits, only on how many inputs it has. Shared by
/// `resolve_bypass_and_geometry` (needs a candidate socket position before
/// any real gate is placed) and `routing_stats` (needs the same lookup to
/// read results back out of an already-compiled world).
fn cell_geometry_by_input_count(netlist: &Netlist) -> HashMap<usize, NorCell> {
    let mut cells = HashMap::new();
    let mut scratch = World::new(20, WORLD_HEIGHT, 20);
    for gate in &netlist.gates {
        cells
            .entry(gate.inputs.len())
            .or_insert_with(|| place_nor_gate(&mut scratch, (8, GATE_Y, 8), gate.inputs.len()));
    }
    cells
}

/// Where a net's own source signal enters the router -- the same position
/// `emit` computes when it actually places the lever/gate output pin
/// (`place_primary_input`, `torch_of`), recomputed purely from geometry and a
/// `NorCell` lookup so `resolve_bypass_and_geometry` can size up a
/// *candidate* bypass path before any real `World` exists to place it in.
fn source_pin_position(
    netlist: &Netlist,
    plan: &Floorplan,
    row_z: &[i32],
    cell_of_count: &HashMap<usize, NorCell>,
    source: Source,
) -> Position {
    match source {
        Source::Lever(i) => Position::new(plan.lever_x[i], GATE_Y, row_z[0]).offset(Facing::North),
        Source::Gate(g) => {
            let cell = &cell_of_count[&netlist.gates[g].inputs.len()];
            let torch = Position::new(
                plan.centre_x[g] + cell.output_offset.0,
                GATE_Y + cell.output_offset.1,
                row_z[plan.row_of[g]] + cell.output_offset.2,
            );
            torch.offset(OUTPUT_DIRECTION)
        }
    }
}

/// Whether `pos` -- one cell of a *candidate* bypass path -- is safe to
/// write: neither `pos` itself nor any of its four same-layer neighbours may
/// already belong to a different net in `reservation`. Same-layer adjacency
/// is exactly what `dust_connections` joins unconditionally (see its own doc
/// comment: same-layer is unconditional there, unlike the climb/descend
/// rules), so it is the one hazard a flat `GATE_Y` bypass path -- which never
/// ramps, so never needs those climb/descend rules `dust_reach` also models
/// -- actually has to avoid.
fn cell_is_free_for(reservation: &Reservation, pos: Position, net: usize) -> bool {
    let owned_by_other = |p: Position| matches!(reservation.get(&p), Some(&owner) if owner != net);
    !owned_by_other(pos) && HORIZONTAL.iter().all(|&direction| !owned_by_other(pos.offset(direction)))
}

/// Whether a horizontal jog from `lo` to `hi` (inclusive, one row's own Z --
/// see `resolve_bypass_and_geometry`) crosses any *other* gate's or lever's
/// body in that row. `self_zone` is the jog's own source's body, which the
/// jog necessarily starts inside of; a gate or lever body is never a
/// conductor, so `row_body_zones` is the only place this keep-out is
/// recorded at all -- a `Reservation` alone would miss it entirely.
fn jog_crosses_another_row_zone(zones: &[(i32, i32)], self_zone: (i32, i32), lo: i32, hi: i32) -> bool {
    zones.iter().any(|&zone| zone != self_zone && zone.0 <= hi && lo <= zone.1)
}

/// Resolve which nets bypass the ramp/track machinery, and lay out the
/// geometry that follows from that decision.
///
/// Two passes:
///
/// 1. **Proven-safe pass.** `compute_bypass`'s geometric proof (see
///    `BYPASS_MAX_DISTANCE`) decides an initial, conservative bypass set;
///    `assign_tracks`/`layout_z` turn that into a real geometry, and one
///    throwaway `emit` (`Footprint::record`, against a scratch `World`) turns
///    that geometry into a complete `Reservation` -- exactly what `compile`
///    itself built before this function existed.
/// 2. **Widened pass.** Every net the proof does not already cover, but whose
///    source/sink columns are within `query_limit`, gets a candidate direct
///    path built against that same baseline geometry, then checked against
///    the `Reservation` from step 1 (`cell_is_free_for`, for every *other*
///    net's conductors) and against every row it jogs through
///    (`jog_crosses_another_row_zone`, for gate/lever bodies, which are never
///    conductors and so never appear in a `Reservation` at all). A clear
///    candidate is promoted.
///
/// # Why the *baseline* reservation is still exactly right for the *final*
/// (larger) bypass set
///
/// Every column this router ever places is, by construction
/// (`reserve_columns`'s own doc comment), at least `COLUMN_CLEARANCE` from
/// every other column in the same channel. A net's own source and sink
/// columns sit at the same X whether or not that net ends up bypassing --
/// bypassing only changes whether it ever reaches `TRACK_Y`, never which X it
/// occupies -- so "does some *other* net have a column between mine" is a
/// fact about `reserve_columns`'s output alone, never about who else happens
/// to be promoted already. The baseline `Reservation` already contains every
/// such column, as either a conventionally-routed net's own entry/exit dust
/// or a feed-through's permanent hop (feed-throughs are never bypass-eligible
/// at all, so their columns are identical in every candidate reservation).
/// So it is exactly as informative as any "final" reservation would be, and
/// every candidate can be checked against the one baseline pass and promoted
/// all at once -- no candidate's promotion can invalidate another's answer.
///
/// # Fan-out and feed-through: still excluded, on purpose
///
/// This still only ever considers nets with exactly one channel and exactly
/// one sink -- the same restriction `compute_bypass` already had. The
/// `Reservation` query does not, by itself, make either of the excluded
/// shapes safe to add:
///
/// - **Fan-out** (one channel, several sinks) needs more than one jog leaving
///   the same trunk cell. Those jogs would have to share one strength budget
///   from a single branch point, the way `lay_track`'s taps already do for a
///   real track -- but a track's taps are all on one shared straight run,
///   while a bypass fan-out's branches point in different directions from
///   the trunk, which `lay_bent_path` (built for one single-source-to-single-
///   sink path) cannot express at all. That is new plumbing, not a
///   consequence of trusting the `Reservation` instead of a proof; the query
///   would still be answerable, but the *route* it would be answering for
///   does not exist yet.
/// - **Feed-through** (more than one channel) is harder for a sharper reason:
///   its whole reason to exist is reaching a row that is *not* the next one,
///   which means its direct path would have to cross an entire intervening
///   row -- gate bodies and all, at the row's own Z, not just skirt one row's
///   edge the way a single-channel jog does. `jog_crosses_another_row_zone`
///   generalises to "any number of rows" without difficulty, but a feed-
///   through's *own* channel-to-channel hop column (`net.hops`) already
///   exists specifically to solve this by going around the row instead of
///   through it -- so the reservation-query win here would only ever be
///   skipping a hop column's own two short ramp-free stretches, not the
///   track/ramp machinery a single-channel bypass skips. Far smaller payoff
///   for materially more surface area to get wrong.
///
/// Both are left to the existing feed-through/track machinery. If a later
/// measurement shows either payoff is worth the extra plumbing, this is
/// where it would plug in -- the `Reservation` this function already builds
/// does not need to change to support it.
///
/// # Where `BYPASS_QUERY_MAX_DISTANCE` came from
///
/// Measured on the four reference circuits, release build (`cargo run --bin
/// build_circuit` for box/blocks, `cargo run --bin routing_cost_report` for
/// settle and bypass counts), sweeping the query limit with
/// `BYPASS_MAX_DISTANCE` (3, i.e. the proof alone, no query at all) as the
/// "off" baseline:
///
/// ```text
/// bypass edges (direct GATE_Y route) out of all routed edges:
///   limit            and4    full_adder   segment_a   seven_segment
///   3  (off)         6/10     6/32        15/83        34/156
///   6                7/10    11/32        26/83        45/156
///   8                8/10    13/32        27/83        45/156
///   9-11             8/10    13/32        27/83        45/156
///   12               8/10    13/32        27/83        46/156
///   15-30            8/10    13/32        27/83        46/156  (unchanged past 12)
///
/// non-air blocks:
///   limit            and4    full_adder   segment_a   seven_segment
///   3  (off)          571     2246         6716        16694
///   6                 551     2116         6686        16694
///   8                 551     2066         6686        16694
///   9-11              551     2066         6686        16694
///   12                551     2066         6686        16654
///   15-30             551     2066         6686        16654  (unchanged past 12)
///
/// worst-case settle (game ticks):
///   limit            and4    full_adder   segment_a   seven_segment
///   3  (off)           30       82           94          124
///   6                  28       78           92          124
///   8                  28       76           92          124
///   9-11               28       76           92          124
///   12                 28       76           92          122
///   15-30              28       76           92          122  (unchanged past 12)
/// ```
///
/// Every circuit is flat from 12 all the way to 30 -- checked directly, not
/// extrapolated -- which matches the geometric ceiling this router actually
/// has: past a certain jog length a gate-sourced candidate runs into the
/// *next* gate in its own row (`jog_crosses_another_row_zone`), and once
/// every net that will ever clear that check has been found, asking about a
/// longer one only spends compile time, never finds another win. 12 is the
/// smallest limit at which all four circuits already show that flatness (11
/// still leaves `seven_segment` one edge and 40 blocks short of where 12-30
/// all land), so it is what `BYPASS_QUERY_MAX_DISTANCE` is set to -- nothing
/// past it was observed to help even once.
fn resolve_bypass_and_geometry(
    netlist: &Netlist,
    plan: &Floorplan,
    nets: &mut [Net],
    row_count: usize,
    channel_count: usize,
    query_limit: i32,
) -> (Vec<bool>, Vec<i32>, Vec<Vec<i32>>) {
    let bypass_proven = compute_bypass(nets, plan);
    let baseline_track_count = assign_tracks(plan, nets, channel_count, &bypass_proven);
    let (baseline_row_z, baseline_track_z) = layout_z(row_count, channel_count, &baseline_track_count);

    let (size_x, size_z) = world_size(plan, nets, &baseline_row_z);
    let mut scratch = World::new(size_x.max(8), WORLD_HEIGHT, size_z.max(8));
    let mut footprint = Footprint::record();
    {
        let geometry = RoutingGeometry {
            plan,
            row_z: &baseline_row_z,
            nets,
            track_z: &baseline_track_z,
            bypass: &bypass_proven,
        };
        emit(&mut scratch, netlist, &geometry, &mut footprint);
    }
    let probe_reservation = footprint.reservation;
    drop(scratch);

    let cell_of_count = cell_geometry_by_input_count(netlist);
    let row_zones = row_body_zones(plan, row_count);
    let mut bypass_final = bypass_proven.clone();

    for (n, net) in nets.iter().enumerate() {
        if bypass_proven[n] || net.channels.len() != 1 || net.sinks[0].len() != 1 {
            continue;
        }
        let (gate, input_index) = net.sinks[0][0];
        let exit_x = approach_column(plan.centre_x[gate], input_index);
        let distance = (exit_x - net.source_column).abs();
        if distance <= BYPASS_MAX_DISTANCE || distance > query_limit {
            continue;
        }

        let pin = source_pin_position(netlist, plan, &baseline_row_z, &cell_of_count, net.source);
        let row_z_gate = baseline_row_z[plan.row_of[gate]];
        let cell = &cell_of_count[&netlist.gates[gate].inputs.len()];
        let (dx, dy, dz) = cell.input_offsets[input_index];
        let socket = Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);

        if pin.x != exit_x {
            let self_zone = match net.source {
                Source::Lever(_) => {
                    (net.source_column - COLUMN_CLEARANCE + 1, net.source_column + COLUMN_CLEARANCE - 1)
                }
                Source::Gate(_) => (
                    net.source_column - GATE_HALF_WIDTH - COLUMN_CLEARANCE + 1,
                    net.source_column + GATE_HALF_WIDTH + COLUMN_CLEARANCE - 1,
                ),
            };
            let (lo, hi) = (pin.x.min(exit_x), pin.x.max(exit_x));
            if jog_crosses_another_row_zone(&row_zones[net.channels[0]], self_zone, lo, hi) {
                continue;
            }
        }

        let mut waypoints: Vec<Position> = Vec::new();
        if pin.x != exit_x {
            waypoints.push(Position::new(exit_x, GATE_Y, pin.z));
        }
        if socket.x != exit_x {
            waypoints.push(Position::new(exit_x, GATE_Y, row_z_gate));
        }
        waypoints.push(socket);

        let cells = bent_path_cells(pin, &waypoints);
        if cells.iter().all(|&pos| cell_is_free_for(&probe_reservation, pos, n)) {
            bypass_final[n] = true;
        }
    }

    let track_count = assign_tracks(plan, nets, channel_count, &bypass_final);
    let (row_z, track_z) = layout_z(row_count, channel_count, &track_count);
    (bypass_final, row_z, track_z)
}

/// Which net `nets[index]` is, by the name a person compiling the netlist
/// would recognise -- the lever's own input name, or the gate output the net
/// carries. Used only for naming cells in a `ConnectivityViolation`.
fn net_name(netlist: &Netlist, nets: &[Net], index: usize) -> String {
    match nets[index].source {
        Source::Lever(i) => netlist.inputs[i].clone(),
        Source::Gate(g) => netlist.gates[g].output.clone(),
    }
}

/// The connectivity invariant: every dust network the finished world
/// actually contains must belong to exactly one net.
///
/// This does not know anything about tracks, columns or ramps -- it only
/// knows what `dust_connections` says is physically joined (the same rule
/// the simulator itself walks) and what `reservation` says every cell was
/// *for*, and it fails the moment those two disagree. That independence is
/// the point: it catches a routing bug regardless of which pass caused it,
/// including ones this module's own keep-out logic has never heard of.
fn verify_connectivity(world: &World, reservation: &Reservation, netlist: &Netlist, nets: &[Net]) -> Result<(), CompileError> {
    let mut visited: HashSet<Position> = HashSet::new();

    for flat in world.positions_of(BlockKind::RedstoneWire) {
        let (x, y, z) = world.decode(flat);
        let start = Position::new(x, y, z);
        if !visited.insert(start) {
            continue;
        }

        let mut owner: Option<(usize, Position)> = reservation.get(&start).map(|&net| (net, start));
        let mut queue: VecDeque<Position> = VecDeque::new();
        queue.push_back(start);

        while let Some(pos) = queue.pop_front() {
            for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                for next in dust_connections(world, pos, direction).iter() {
                    if !visited.insert(next) {
                        continue;
                    }
                    queue.push_back(next);

                    if let Some(&found_net) = reservation.get(&next) {
                        match owner {
                            None => owner = Some((found_net, next)),
                            Some((expected_net, expected_cell)) if expected_net != found_net => {
                                return Err(CompileError::ConnectivityViolation {
                                    cell: (next.x, next.y, next.z),
                                    found_net: net_name(netlist, nets, found_net),
                                    expected_cell: (expected_cell.x, expected_cell.y, expected_cell.z),
                                    expected_net: net_name(netlist, nets, expected_net),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// 把一個網表編譯成一個紅石世界。
pub fn compile(netlist: &Netlist) -> Result<CompiledCircuit, CompileError> {
    for gate in &netlist.gates {
        for input in &gate.inputs {
            if !netlist.is_driven(input) {
                return Err(CompileError::UndrivenSignal(input.clone()));
            }
        }
    }
    for output in &netlist.outputs {
        if !netlist.gates.iter().any(|gate| &gate.output == output) {
            return Err(CompileError::UndrivenSignal(output.clone()));
        }
    }

    let order = netlist.topological_order().ok_or(CompileError::CyclicNetlist)?;

    let mut producer_of: HashMap<&str, usize> = HashMap::new();
    for (index, gate) in netlist.gates.iter().enumerate() {
        producer_of.insert(gate.output.as_str(), index);
    }

    let plan = build_floorplan(netlist, &order, &producer_of);
    let row_count = plan.rows.len();
    let channel_count = row_count.saturating_sub(1);
    let mut nets = build_nets(netlist, &order, &plan, &producer_of);

    reserve_columns(&plan, &mut nets, row_count, channel_count);
    let (bypass, row_z, track_z) =
        resolve_bypass_and_geometry(netlist, &plan, &mut nets, row_count, channel_count, BYPASS_QUERY_MAX_DISTANCE);

    let (size_x, size_z) = world_size(&plan, &nets, &row_z);

    // ---------------------------------------------------------------
    // Emission
    // ---------------------------------------------------------------
    //
    // Two passes over the exact same code (`emit`). The first, recording
    // pass runs against a throwaway world purely to learn where every net's
    // conductor cells end up -- ramps, columns and tracks alike, for every
    // net, not just the ones already written when a given ramp is placed.
    // Only once that whole-circuit picture exists does the second pass run
    // for real, consulting it so a keep-out cell is only ever sealed with
    // stone when nothing else in the circuit will ever legitimately use it.
    // See `Footprint` and `seal_cross_talk` for why that split is what makes
    // the spacing constraint free where nothing is near.

    let geometry = RoutingGeometry { plan: &plan, row_z: &row_z, nets: &nets, track_z: &track_z, bypass: &bypass };

    let mut scratch = World::new(size_x.max(8), WORLD_HEIGHT, size_z.max(8));
    let mut footprint = Footprint::record();
    emit(&mut scratch, netlist, &geometry, &mut footprint);
    drop(scratch);

    let mut footprint = Footprint::enforce(footprint.reservation);
    let mut world = World::new(size_x.max(8), WORLD_HEIGHT, size_z.max(8));
    let EmitResult { input_positions, output_positions, gate_output_positions } =
        emit(&mut world, netlist, &geometry, &mut footprint);

    // The connectivity invariant: whatever the two passes above actually
    // wrote, it must partition into exactly the nets the netlist asked for.
    // Checked here, unconditionally, on every compile -- not just the ones a
    // test happens to exercise -- because a violation is a bug in *this*
    // router, not in the netlist it was given.
    verify_connectivity(&world, &footprint.reservation, netlist, &nets)?;

    Ok(CompiledCircuit { world, input_positions, output_positions, gate_output_positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `Net` for tests that only care about `net_name` / ownership
    /// lookups, not real routing geometry -- `verify_connectivity` never
    /// looks at anything but `source`.
    fn nameless_net(source: Source) -> Net {
        Net { source, source_column: 0, channels: Vec::new(), tracks: Vec::new(), sinks: Vec::new(), hops: Vec::new() }
    }

    /// The connectivity invariant, built directly rather than hoped for:
    /// two separate dust cells, each reserved for a different net, placed
    /// next to each other so `dust_connections`' same-layer rule joins them
    /// into one electrical network. No router, no netlist compile -- this
    /// exercises `verify_connectivity` on a world constructed by hand,
    /// exactly the "two nets routed so their dust touches" case the spacing
    /// model spec asks for.
    #[test]
    fn verify_connectivity_rejects_two_nets_whose_dust_touches() {
        let netlist = Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: Vec::new(), gates: Vec::new() };
        let nets = vec![nameless_net(Source::Lever(0)), nameless_net(Source::Lever(1))];

        let mut world = World::new(5, 5, 5);
        let net_a_cell = Position::new(1, 1, 2);
        let net_b_cell = Position::new(2, 1, 2);
        world.set(net_a_cell.x, net_a_cell.y - 1, net_a_cell.z, stone());
        world.set(net_a_cell.x, net_a_cell.y, net_a_cell.z, dust());
        world.set(net_b_cell.x, net_b_cell.y - 1, net_b_cell.z, stone());
        world.set(net_b_cell.x, net_b_cell.y, net_b_cell.z, dust());

        let mut reservation = Reservation::new();
        reservation.insert(net_a_cell, 0);
        reservation.insert(net_b_cell, 1);

        let err = verify_connectivity(&world, &reservation, &netlist, &nets)
            .expect_err("adjacent dust reserved for two different nets must be rejected");

        assert_eq!(
            err,
            CompileError::ConnectivityViolation {
                cell: (net_b_cell.x, net_b_cell.y, net_b_cell.z),
                found_net: "b".to_string(),
                expected_cell: (net_a_cell.x, net_a_cell.y, net_a_cell.z),
                expected_net: "a".to_string(),
            }
        );

        let message = err.to_string();
        assert!(message.contains("(2, 1, 2)"), "message must name the offending cell: {message}");
        assert!(message.contains('a') && message.contains('b'), "message must name both nets: {message}");
    }

    /// The same two cells, but far enough apart that `dust_connections`
    /// never joins them -- the invariant must stay silent when nothing
    /// actually touches.
    #[test]
    fn verify_connectivity_accepts_two_nets_whose_dust_never_touches() {
        let netlist = Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: Vec::new(), gates: Vec::new() };
        let nets = vec![nameless_net(Source::Lever(0)), nameless_net(Source::Lever(1))];

        let mut world = World::new(6, 5, 6);
        let net_a_cell = Position::new(1, 1, 2);
        let net_b_cell = Position::new(4, 1, 2);
        world.set(net_a_cell.x, net_a_cell.y - 1, net_a_cell.z, stone());
        world.set(net_a_cell.x, net_a_cell.y, net_a_cell.z, dust());
        world.set(net_b_cell.x, net_b_cell.y - 1, net_b_cell.z, stone());
        world.set(net_b_cell.x, net_b_cell.y, net_b_cell.z, dust());

        let mut reservation = Reservation::new();
        reservation.insert(net_a_cell, 0);
        reservation.insert(net_b_cell, 1);

        assert_eq!(verify_connectivity(&world, &reservation, &netlist, &nets), Ok(()));
    }
}

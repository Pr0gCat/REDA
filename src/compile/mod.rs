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
//! # Placement and routing: levels stack along Y, not Z
//!
//! Gates are levelised, and every logic level gets its own horizontal band
//! stacked along Y -- level 0's band sits at the bottom, level 1's directly
//! above it, and so on. Within one band, gates of that level share a row
//! (spread along X), and a routing channel above that row carries their
//! outputs to the next band's row: east-west tracks on one Y layer, then a
//! short climb to the next band's gate plane. Because every band reuses the
//! *same* X/Z footprint (only Y differs), the whole circuit's Z extent is
//! bounded by one band's own local routing depth, not by the number of
//! levels -- levels no longer compete for a footprint that has to fit inside
//! Minecraft's horizontal chunk-loading radius. See "Placement and routing"
//! further down for the full picture, including how an edge that skips
//! several levels (a signal produced at level 0 and consumed at level 8)
//! climbs straight through the intervening bands via a dedicated shaft
//! instead of visiting each one.
//!
//! Every run of dust gets a repeater at least every 15 blocks, and a route
//! always ends in a repeater facing the next gate's support block -- redstone
//! dust does not charge a block sideways (only weakly, straight down), so
//! every wire has to be terminated by an active component.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::redstone::simulator::position::Position;
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

fn repeater(facing: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Repeater;
    state.name = "minecraft:repeater".to_string();
    state.facing = Some(facing);
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
// Level *i* gets its own horizontal band, stacked along Y:
//
//     band 0   row 0 (the primary inputs' levers) + channel 0
//     band 1   row 1 (every gate whose level is 0) + channel 1
//     band 2   row 2 (every gate whose level is 1) + channel 2
//     ...
//
// A band is five Y layers tall, in this order from its own bottom:
//
//   offset 0   floor: sparse support blocks for whatever sits at offset 1.
//   offset 1   GATE_Y_OFFSET: gate bodies, lever homes, primary-input pins,
//              and every north-south routing column.
//   offset 2   the isolation floor / merge-dust plane: doubles as a NOR
//              cell's own bus wire support *and* as the solid floor a
//              track needs directly beneath it. Same role `Y=2` played in
//              the pre-3D design, just now repeated once per band.
//   offset 3   TRACK_Y_OFFSET: east-west routing tracks.
//   offset 4   a genuinely empty layer, never written to by anything. Not
//              here for the diagonal climb rule (which only ever bridges
//              wire that is exactly one level apart through a specific
//              neighbour, and this band's own track and the next band's
//              floor are never that neighbour to each other) but for a
//              completely different mechanism -- see `DESCEND_LENGTH`'s
//              doc comment, which is where this layer's existence is
//              actually derived from, not this diagram.
//
// The *next* band's own floor picks up at offset 5 (= its own offset 0).
// Signal flow only ever moves *up* through the whole stack, so the hop from
// one band's track into the next band's gate plane is itself another
// ascend, `DESCEND_LENGTH` levels instead of `RAMP_LENGTH` -- see
// `DESCEND_LENGTH`'s own comment for why the two are not the same number.
//
// Every band reuses the *same* Z template (a row's gates sit at a single
// constant `ROW_Z`, and each channel's tracks sit at `ROW_Z` minus some
// multiple of `TRACK_SPACING`, sized for the *widest single channel*, not
// summed across every level the way the old Z-stacked design had to). Only
// Y tells two bands apart. Consecutive levels are therefore vertically
// adjacent: the hop from a gate to its next-level consumer is two small
// climbs (`RAMP_LENGTH` up to this band's own track, `DESCEND_LENGTH` on to
// the next band's gate plane) plus a short horizontal run within one band's
// own local channel, not a traverse across the whole circuit's Z extent.
//
// Edges that skip levels. A netlist edge does not always connect consecutive
// levels -- a signal produced at level 0 may be consumed at level 8. Such an
// edge does not visit every intervening band's gate row at all: after
// reaching its own band's track, it keeps climbing straight past every
// intervening band's Y range (using a route that never enters their X/Z
// footprint -- see `climb_levels` and the "Skip-level shafts" section below)
// until it reaches the destination band's track. See `assign_shafts`.

/// Offset within a band where gate bodies, lever homes, primary-input pins,
/// and every north-south routing column sit.
const GATE_Y_OFFSET: i32 = 1;

/// How far a **single-band** ramp travels horizontally while changing layer,
/// *and* how much signal strength it spends doing so -- the two are the same
/// number here, not a coincidence pasted together (see the pre-3D design's
/// identical constant, whose derivation via `dust_connections`' diagonal rule
/// still applies unchanged: one block of horizontal travel, one point of
/// strength, per Y level, always in lockstep).
///
/// This is the *only* ramp height that is architecturally fixed by the cell
/// library. A skip-level edge's climb (`climb_levels`) is instead a multiple
/// of a whole `BAND_HEIGHT`, and its own strength cost is accounted for
/// separately -- see `track_reserve`.
const RAMP_LENGTH: i32 = 2;

/// Offset within a band where east-west routing tracks sit. One layer below
/// this (`TRACK_Y_OFFSET - 1`) is a track's own stone floor, which is the
/// *same* layer a gate cell's merge dust sits on -- see the isolation
/// argument above.
const TRACK_Y_OFFSET: i32 = GATE_Y_OFFSET + RAMP_LENGTH;

/// How far a channel's own track sits below the *next* band's gate plane --
/// deliberately `RAMP_LENGTH + 1`, not `RAMP_LENGTH` again, even though
/// signal flow only ever moves up through the whole stack and this hop is
/// architecturally just another ascend (`move_between_layers` does not care
/// which of the two it is asked to build).
///
/// The `+ 1` exists for a completely different reason than the climb rules:
/// `redstone::simulator::propagate::recompute_dust_strengths` treats *any*
/// strongly-powered solid block as a conduit that recharges every redstone
/// wire directly adjacent to it -- not just via the diagonal climb rule, but
/// via plain orthogonal adjacency (the same mechanism `compile`'s own output
/// -lamp placement comment works around, one level down instead of one level
/// sideways). Every repeater in the next band's own approach columns sits on
/// a support floor at that band's own floor offset (0). If that floor sat
/// directly on top of *this* band's track (offset `RAMP_LENGTH` = 2, one
/// level below), a repeater anywhere in the next band -- and every band's
/// approach columns reuse the *same* wide X range this band's track already
/// spans, so this is not a rare coincidence to guard against, it is the
/// common case -- would strongly power that floor block and leak straight
/// down into whatever net's track cell happens to sit under it. `+ 1` buys
/// back the one genuinely empty Y layer (`RAMP_LENGTH + 1`, never written to
/// by anything) that keeps the next band's floor two levels above this
/// band's track instead of one, out of reach of plain adjacency the same
/// way the pre-3D design's single shared `WORLD_HEIGHT` kept its own track
/// plane two levels clear of its own gate plane's merge dust.
///
/// Getting this wrong does not fail loudly: the circuit still compiles and
/// still places every repeater exactly where the strength budget says to,
/// it just also strongly re-powers unrelated dust, which reads as answers
/// stuck high regardless of input -- not a settle failure, not a panic.
const DESCEND_LENGTH: i32 = RAMP_LENGTH + 1;

/// Height of one logic level's whole band: floor, gate plane, isolation
/// floor / merge dust, track plane, and the one genuinely empty layer
/// `DESCEND_LENGTH` buys back (see its own doc comment). This is exactly
/// the number `docs/superpowers/specs/2026-08-07-3d-placement.md`'s "band
/// height" asks for, and this derivation -- `RAMP_LENGTH` up to this band's
/// own track, `DESCEND_LENGTH` on to the next band's gate plane -- is how it
/// was arrived at, not guessed.
///
/// Multiplying this by the level count is what decides whether a circuit
/// fits under Minecraft's 384-block ceiling; see `compile`'s size
/// computation and this module's tests for what that comes out to on the
/// four reference circuits.
const BAND_HEIGHT: i32 = RAMP_LENGTH + DESCEND_LENGTH;

/// Absolute Y of row `row`'s gate plane (and lever home, and every
/// north-south column in its own band).
fn gate_y(row: usize) -> i32 {
    row as i32 * BAND_HEIGHT + GATE_Y_OFFSET
}

/// Absolute Y of channel `row`'s east-west track plane -- the channel that
/// carries row `row`'s outputs onward (to row `row + 1`, or further, via a
/// skip-level shaft).
fn track_y(row: usize) -> i32 {
    row as i32 * BAND_HEIGHT + TRACK_Y_OFFSET
}

/// X distance between two neighbouring gates of the same row.
///
/// A gate cell reaches out to `cx ± GATE_HALF_WIDTH`, so 14 leaves a five-wide
/// gap between two cells -- room for exactly one feed-through column that is
/// still at least `COLUMN_CLEARANCE` clear of everything on either side.
const SLOT_PITCH: i32 = 14;

/// Half the X width of a gate cell: the west and east socket approach columns
/// sit at `cx ± GATE_HALF_WIDTH`.
const GATE_HALF_WIDTH: i32 = 4;

/// X offset of the south socket's own approach column, out past the west and
/// east ones (`± GATE_HALF_WIDTH`) with `COLUMN_CLEARANCE` to spare on both
/// sides -- see `approach_column`'s doc comment for why south needs a column
/// of its own at all.
const SOUTH_APPROACH_OFFSET: i32 = GATE_HALF_WIDTH + 2 * COLUMN_CLEARANCE;

/// How far south of a row's own Z the south approach's dogleg turns back
/// west, before heading north into the socket -- see `socket_approach_
/// corners`. Only needs to clear the south socket itself (`ROW_Z + 2`, fixed
/// by `place_nor_gate`'s cell geometry) by `lay_segment_to_corner`'s own
/// minimum spacing of 2.
const SOUTH_TURN_MARGIN: i32 = 4;

/// Z distance between two neighbouring tracks of the same channel.
///
/// Every band reuses this same spacing at its own Y, so this bounds Z by the
/// *widest single channel*'s own track count, not by anything that grows
/// with the number of levels -- see `layout_row_z`.
const TRACK_SPACING: i32 = 5;

/// Minimum X gap between two nets that share one track.
const TRACK_SHARE_GAP: i32 = 4;

/// Minimum X gap between two routing columns. Redstone dust connects to its
/// four horizontal neighbours, so one empty block between two columns is
/// already enough -- but only exactly enough, which is why every other
/// clearance in this module is derived from it rather than written out.
const COLUMN_CLEARANCE: i32 = 2;

/// West edge of the floorplan. Everything is laid out eastwards from here, so
/// no coordinate can go negative on the X axis.
const ORIGIN_X: i32 = 8;

/// How far east of every band's own content a skip-level shaft's lanes start
/// (see `assign_shafts`). Chosen generously past `COLUMN_CLEARANCE` purely
/// for readability at the boundary; the shafts themselves need no clearance
/// search at all, since content never extends this far east in any band.
const SHAFT_MARGIN: i32 = 6;

/// X gap between two concurrent skip-level shafts' lanes.
///
/// Two shafts climb at the same rate (one X per Y level, see
/// `assign_shafts`), so a constant gap at their starting Y is a constant
/// gap at every Y both are active -- they can never converge in the sense
/// of ending up at the *same* cell. But `COLUMN_CLEARANCE` (2) is not
/// enough to keep them from interfering with *each other's* diagonal climb
/// rule, which is a stricter requirement than plain adjacency: a climbing
/// shaft's own ascend needs the cell directly above its *current* landing
/// to stay open (`climb_levels`'s doc comment), and a second shaft exactly
/// 2 lanes over has its own riser (a solid block, laid one Y level later)
/// land in exactly that cell -- gap 1 fails the same way via plain
/// horizontal adjacency, and gap 2 fails this way instead, so 3 is the
/// smallest gap that avoids both. This is exactly the kind of interaction
/// `assign_shafts`' "no clearance search needed" reasoning does not cover,
/// since it only reasoned about a shaft's X never falling inside a band's
/// *content*, not about two shafts' diagonal footprints reaching each
/// other. Doubled here for headroom rather than shipped at the bare
/// minimum (3), since every sub-case of this interaction (a landing's own
/// future riser, the symmetric direction) was not exhaustively re-verified
/// once one concrete instance of it was found and fixed.
const SHAFT_LANE_PITCH: i32 = 4;

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
/// A run that empties into a single-band ramp cannot spend the full 14,
/// though: the ramp itself is `RAMP_LENGTH` more hops that has to survive on
/// whatever strength is left when the flat run hands off to it, and a ramp
/// cannot absorb a repeater partway up -- a repeater cannot change Y. So such
/// a run's last cell must still be carrying at least `RAMP_LENGTH + 1`,
/// which means it may only spend `MAX_DUST_RUN - RAMP_LENGTH` cells per
/// refresh cycle, not the full budget. A run that empties into a
/// **skip-level** shaft instead reserves that shaft's own climb height --
/// see `track_reserve`, which generalises this same reasoning to a climb
/// that spans many bands instead of one.
const MAX_DUST_RUN: i32 = 14;

/// 編譯過程的錯誤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// 網表裡有迴路
    CyclicNetlist,
    /// 訊號沒有驅動來源
    UndrivenSignal(String),
}

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
    /// person reads -- the lamp's own extra delay (`LAMP_DELAY_GAME_TICKS`)
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
/// reserve for a run that empties into a single-band ramp, `track_reserve`
/// for a run that empties into a skip-level shaft instead, and 0 for one
/// that empties into a mandatory terminating repeater (which cannot die no
/// matter what strength reaches it, as long as it is nonzero).
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
) -> u8 {
    let len = straight_run_length(start, direction, stop_before);
    let (is_repeater, ending_strength) = plan_straight_run(len, incoming_strength, reserve);

    let mut pos = start.offset(direction);
    for &place_repeater in &is_repeater {
        ensure_floor(world, pos);
        if place_repeater {
            world.set(pos.x, pos.y, pos.z, repeater(direction));
        } else {
            world.set(pos.x, pos.y, pos.z, dust());
        }
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
fn lay_segment_to_corner(world: &mut World, start: Position, corner: Position, incoming_strength: u8) {
    let direction = direction_from(start, corner);
    let refresh_point = corner.offset(direction.opposite());

    lay_dust_run(world, start, direction, refresh_point, incoming_strength, 0);

    ensure_floor(world, refresh_point);
    world.set(refresh_point.x, refresh_point.y, refresh_point.z, repeater(direction));

    ensure_floor(world, corner);
    world.set(corner.x, corner.y, corner.z, dust());
}

/// 鋪一段路線的最後一段，終點是某個閘的輸入插座：這一格**一定**是中繼器，
/// 面朝這一段前進的方向 —— 而這個方向剛好就是「面朝支撐塊」的方向，因為
/// 插座本來就在支撐塊的正對外側。這是唯一能強充能支撐塊的方法：紅石粉
/// 水平方向不會充能方塊。
///
/// Same reasoning as `lay_segment_to_corner`: the mandatory repeater at
/// `socket` means this segment can never die, so `reserve` is 0.
fn lay_segment_to_socket(world: &mut World, start: Position, socket: Position, incoming_strength: u8) {
    let direction = direction_from(start, socket);
    lay_dust_run(world, start, direction, socket, incoming_strength, 0);
    ensure_floor(world, socket);
    world.set(socket.x, socket.y, socket.z, repeater(direction));
}

/// The two horizontal directions perpendicular to `direction` -- the only
/// two `seal_cross_talk` may ever seal.
fn side_directions(direction: Facing) -> [Facing; 2] {
    match direction {
        Facing::North | Facing::South => [Facing::East, Facing::West],
        Facing::East | Facing::West => [Facing::North, Facing::South],
        Facing::Up | Facing::Down => unreachable!("a ramp only ever travels horizontally"),
    }
}

/// Seal `pos`'s two neighbours perpendicular to `direction` with stone,
/// wherever they are still air.
///
/// This is a single-band ramp's cross-talk guard (`move_between_layers`),
/// deliberately narrower than sealing all four horizontal neighbours: a dust
/// staircase is built from `dust_connections`' diagonal rule, and that rule
/// requires one specific neighbour -- the one along the direction of travel
/// -- to stay exactly as the climb/descend step leaves it; sealing it would
/// sever the very connection the staircase exists to make. Only the two side
/// neighbours are ever free of that constraint.
///
/// A skip-level shaft (`climb_levels`) does not call this at all -- it never
/// runs anywhere near another band's content in the first place (see
/// "Skip-level shafts" below), so there is nothing nearby for it to guard
/// against.
fn seal_cross_talk(world: &mut World, pos: Position, direction: Facing) {
    for side in side_directions(direction) {
        let neighbour = pos.offset(side);
        if world.get(neighbour.x, neighbour.y, neighbour.z).kind == BlockKind::Air {
            world.set(neighbour.x, neighbour.y, neighbour.z, stone());
        }
    }
}

/// Move a signal from `entry` (already lit, at whatever strength the caller
/// arranged for) along `direction` to height `target_y`, one dust staircase
/// step per Y level, and return the position of the final landing dust.
///
/// This is the **single-band** ramp: it only ever moves `RAMP_LENGTH` levels
/// (a band's own gate plane to its own track plane, or back), which is
/// always safely inside the strength budget by construction, so it needs no
/// `reserve` parameter of its own -- unlike `climb_levels`, the shaft used
/// for edges that skip whole bands.
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
/// Either direction spends exactly one signal strength per level walked and
/// places no repeater -- the caller is responsible for having reserved that
/// strength in whatever run feeds this ramp (`MAX_DUST_RUN`'s comment).
fn move_between_layers(world: &mut World, entry: Position, direction: Facing, target_y: i32) -> Position {
    let mut current = entry;
    if target_y >= current.y {
        while current.y != target_y {
            let riser = current.offset(direction);
            world.set(riser.x, riser.y, riser.z, stone());
            let landing = riser.up();
            world.set(landing.x, landing.y, landing.z, dust());
            seal_cross_talk(world, landing, direction);
            current = landing;
        }
    } else {
        while current.y != target_y {
            let stepped = current.offset(direction);
            let landing = Position::new(stepped.x, stepped.y - 1, stepped.z);
            ensure_floor(world, landing);
            world.set(landing.x, landing.y, landing.z, dust());
            seal_cross_talk(world, landing, direction);
            current = landing;
        }
    }
    current
}

/// A skip-level shaft's climb: ascend `levels` Y-levels from `entry` in
/// `direction` (always `Facing::East`, in practice -- see `assign_shafts`),
/// exactly like `move_between_layers`'s ascend half, but generalised to a
/// span that may cross many bands -- and so, unlike a single-band ramp,
/// generally too tall to survive on one fresh 15 (a netlist edge's two
/// levels can be arbitrarily far apart; nothing bounds how many bands a
/// shaft climbs).
///
/// A climb cannot refresh its own strength the way a flat run can, though:
/// `plan_straight_run` can swap a repeater in for any dust cell of a flat
/// run because a repeater simply *replaces* what would have been that run's
/// next dust cell, but the diagonal climb rule
/// (`redstone::simulator::connectivity::dust_connections`) only ever
/// connects **dust** to dust -- a repeater does not participate in it at
/// all. So refreshing mid-climb costs a small **detour** instead: step
/// sideways, in one of the two directions perpendicular to travel, onto a
/// repeater and then a fresh dust cell, then resume climbing from there.
/// This is safe for the same reason a shaft needs no cross-talk sealing at
/// all (contrast `move_between_layers`): a shaft's X is always east of
/// every band's own content (`assign_shafts`), so a sideways detour can
/// never step into anything real, and two concurrent shafts can never
/// converge (`assign_shafts`'s doc comment) -- so a detour's exact Z does
/// not matter, only that the climb keeps going afterwards. Landing on the
/// destination channel's actual track Z is `land_shaft`'s job, once the
/// climb is done, however much Z drifted from however many detours this
/// took.
///
/// The detour direction is fixed (one of `side_directions(direction)`, not
/// `direction` itself and not its opposite) so that it can never retrace a
/// cell the climb has already been through, which is what keeps a detour
/// from ever needing to reason about what an earlier step already placed.
///
/// Decide, for a climb of `levels` Y-steps starting at `incoming_strength`,
/// which of them need a detour-and-refresh first. Pure, exactly like
/// `plan_straight_run` -- so `routing_stats` can replay this identical
/// decision while reading back an already-built world, without duplicating
/// (and risking drifting from) the strength arithmetic that produced it.
fn plan_climb(levels: i32, incoming_strength: u8) -> Vec<bool> {
    debug_assert!(incoming_strength > 0, "a climb cannot start from an already-dead signal");
    let mut needs_detour = vec![false; levels.max(0) as usize];
    let mut since_refresh = (MAX_SIGNAL_STRENGTH - incoming_strength) as i32;
    for slot in needs_detour.iter_mut() {
        if since_refresh >= MAX_DUST_RUN {
            *slot = true;
            since_refresh = 0;
        }
        since_refresh += 1;
    }
    needs_detour
}

fn climb_levels(world: &mut World, entry: Position, direction: Facing, levels: i32, incoming_strength: u8) -> (Position, u8) {
    let needs_detour = plan_climb(levels, incoming_strength);
    let detour_direction = side_directions(direction)[0];
    let mut current = entry;
    let mut strength = incoming_strength;
    for &detour in &needs_detour {
        if detour {
            let repeater_pos = current.offset(detour_direction);
            ensure_floor(world, repeater_pos);
            world.set(repeater_pos.x, repeater_pos.y, repeater_pos.z, repeater(detour_direction));
            let fresh = repeater_pos.offset(detour_direction);
            ensure_floor(world, fresh);
            world.set(fresh.x, fresh.y, fresh.z, dust());
            current = fresh;
            strength = MAX_SIGNAL_STRENGTH;
        }
        let riser = current.offset(direction);
        world.set(riser.x, riser.y, riser.z, stone());
        let landing = riser.up();
        world.set(landing.x, landing.y, landing.z, dust());
        current = landing;
        strength -= 1;
    }
    (current, strength)
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
/// ramp (or a skip-level shaft) needs to know what the track hands it before
/// it can tell whatever comes after it what it is starting from, but the
/// track itself is not actually written until the later Tracks pass
/// (world-building order needs Ramps, then Columns, then Tracks -- see the
/// comment where `compile` calls them). `lay_track` runs this identical pure
/// decision again when it actually writes the track, so the two can never
/// silently disagree.
///
/// `reserve` is `track_reserve`'s result for this (net, slot) -- see its own
/// doc comment for what it actually returns and why.
fn track_exit_strengths(
    source_x: i32,
    min_x: i32,
    max_x: i32,
    taps: &BTreeSet<i32>,
    incoming_strength: u8,
    reserve: i32,
) -> BTreeMap<i32, u8> {
    let mut exit_strength = BTreeMap::new();
    for (end, step) in [(min_x, -1i32), (max_x, 1i32)] {
        let length = (end - source_x) * step;
        if length <= 0 {
            continue;
        }
        let cells: Vec<i32> = (1..=length).map(|k| source_x + k * step).collect();
        let (_is_repeater, strengths) = plan_track_run(&cells, incoming_strength, reserve, taps);
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
/// `taps` are the X positions where a ramp or a skip-level shaft joins or
/// leaves the track; every one of them reserves `reserve` strength for
/// whatever it feeds (see `MAX_DUST_RUN`'s comment and `track_reserve`) --
/// applied to the whole track rather than just at each tap, which is
/// simpler and only ever costs an occasional early repeater on a run that
/// was already close to the 14-cell limit, never a correctness problem.
#[allow(clippy::too_many_arguments)]
fn lay_track(
    world: &mut World,
    y: i32,
    z: i32,
    source_x: i32,
    min_x: i32,
    max_x: i32,
    taps: &BTreeSet<i32>,
    incoming_strength: u8,
    reserve: i32,
) -> BTreeMap<i32, u8> {
    let mut exit_strength = BTreeMap::new();
    for (end, step) in [(min_x, -1i32), (max_x, 1i32)] {
        let length = (end - source_x) * step;
        if length <= 0 {
            continue;
        }
        let direction = if step > 0 { Facing::East } else { Facing::West };
        let cells: Vec<i32> = (1..=length).map(|k| source_x + k * step).collect();
        let (is_repeater, strengths) = plan_track_run(&cells, incoming_strength, reserve, taps);

        for (k, &x) in cells.iter().enumerate() {
            let pos = Position::new(x, y, z);
            ensure_floor(world, pos);
            if is_repeater[k] {
                world.set(pos.x, pos.y, pos.z, repeater(direction));
            } else {
                world.set(pos.x, pos.y, pos.z, dust());
            }
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
/// The levers live in row 0's band, south of every gate row, and signal flow
/// is northwards, so the pin goes on the lever's **north** side: that is the
/// way the route has to leave anyway, and it keeps the route from turning
/// back into the lever and overwriting it with dust.
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
/// entered from one side: the west socket from the west, the east socket
/// from the east, and the south socket from the south.
///
/// The first two turn a single corner on the gate's own row, `GATE_HALF_
/// WIDTH` out from the centre, and their sockets sit at the row's own Z, so
/// a ramp landing (always north of the row -- every route arrives from a
/// channel, and every channel this module ever places is north of the row
/// it feeds) reaches them by simply continuing north into that corner and
/// then east/west into the socket (`socket_route`'s single-corner case).
///
/// The south socket is different in a way that has no equivalent for west
/// or east: it sits two blocks *south* of the row, needing a wire that
/// arrives travelling north into it -- but every ramp landing is itself
/// north of the row. A straight run south from the landing would have to
/// pass straight through the gate's own body (the centre block and the
/// output torch both sit between the landing and the socket). So the south
/// approach gets a column of its own, offset in X exactly like west and
/// east are, purely so it has room to dogleg *around* the gate on its way
/// to a point south of it, before turning back north into the socket --
/// see `socket_route`'s two-corner case.
fn approach_column(centre_x: i32, input_index: usize) -> i32 {
    match input_index {
        0 => centre_x - GATE_HALF_WIDTH,
        1 => centre_x + GATE_HALF_WIDTH,
        _ => centre_x + SOUTH_APPROACH_OFFSET,
    }
}

/// Waypoints a socket's approach route must corner through between a ramp's
/// `landing` (always north of the row -- see `approach_column`'s doc
/// comment) and the `socket` itself, in order, not including either
/// endpoint.
///
/// - **Zero** if they already share both X and Z (never actually happens
///   for any of the three real input directions today, but costs nothing to
///   handle rather than assume away).
/// - **One**, at the row's own `row_z`, for west or east: their sockets sit
///   at the row's own Z, so the landing only has to turn once, from
///   travelling north to travelling east/west.
/// - **Two**, for south: its socket sits *south* of the row, which a wire
///   arriving from the north cannot reach without first getting past the
///   gate's own body. The first waypoint continues north-to-south past the
///   row entirely (at the landing's own X, clear of the gate); the second
///   turns back toward the socket's X, still south of the row -- so the
///   final leg (`lay_segment_to_socket`/`scan_to_socket`, laid by the
///   caller) approaches heading north, exactly as the socket requires.
fn socket_approach_corners(landing: Position, socket: Position, row_z: i32) -> Vec<Position> {
    if landing.x == socket.x && landing.z == socket.z {
        Vec::new()
    } else if socket.z == row_z {
        vec![Position::new(landing.x, landing.y, row_z)]
    } else {
        let turn_z = row_z + SOUTH_TURN_MARGIN;
        vec![Position::new(landing.x, landing.y, turn_z), Position::new(socket.x, landing.y, turn_z)]
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
    /// Up a skip-level shaft, to rejoin a track in a later channel.
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
/// net's presence in channel `channels[i]`. For a net that spans more than
/// one channel, `hop_exit[i]` / `hop_entry[i]` are the skip-level shaft that
/// carries it from `channels[i]` to `channels[i + 1]` -- see `assign_shafts`.
struct Net {
    source: Source,
    source_column: i32,
    channels: Vec<usize>,
    tracks: Vec<usize>,
    sinks: Vec<Vec<(usize, usize)>>,
    /// The X where this net's signal leaves `channels[i]`'s track to start
    /// climbing toward `channels[i + 1]` -- a tap on `channels[i]`'s track.
    hop_exit: Vec<i32>,
    /// The X where the climb from `channels[i]` lands, on `channels[i + 1]`'s
    /// track -- `entry_column(i + 1)` reads this. Not necessarily equal to
    /// `hop_exit[i]`: the shaft only ever moves in one direction (east), so
    /// climbing to a taller band moves this east of where the climb started.
    hop_entry: Vec<i32>,
}

impl Net {
    fn entry_column(&self, slot: usize) -> i32 {
        if slot == 0 {
            self.source_column
        } else {
            self.hop_entry[slot - 1]
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
            exits.push(Exit::Feedthrough { x: self.hop_exit[slot], next_slot: slot + 1 });
        }
        exits
    }

    /// The X range this net's track has to span inside channel `slot`.
    fn span(&self, slot: usize, centre_x: &[i32]) -> (i32, i32) {
        self.span_padded(slot, centre_x, 0)
    }

    /// Same as `span`, but any X that comes from a skip-level shaft tap --
    /// this slot's own incoming `entry_column` if it was reached via a
    /// shaft (`slot > 0`), or this slot's own outgoing `Feedthrough` exit if
    /// it has one -- is treated as `pad` further east than it really sits.
    ///
    /// `assign_tracks` is the only caller that ever passes a nonzero `pad`
    /// (`shaft_rail_offset`'s own value): a shaft tap's real electrical
    /// position never moves, but `move_through_shaft` physically extends
    /// past it, all the way out to that same offset, before it is safe to
    /// change Z at all (see that function's doc comment for why). If left-
    /// edge track assignment planned packing around only the real,
    /// un-padded tap position, a second net's own track could legally start
    /// as close as `TRACK_SHARE_GAP` past it -- well inside the reach
    /// `move_through_shaft` actually needs -- and the two would end up
    /// physically wired together despite belonging to different nets.
    /// Padding here reserves that reach up front, in the one place that
    /// decides which nets may ever share a physical track, so the shaft's
    /// own rail extension can never collide with a neighbour it never knew
    /// about. Every other caller (strength planning, the actual track draw)
    /// keeps using the unpadded `span`, since none of them place a single
    /// block out past the real tap themselves.
    fn span_padded(&self, slot: usize, centre_x: &[i32], pad: i32) -> (i32, i32) {
        let entry = self.entry_column(slot);
        let padded_entry = if slot > 0 { entry + pad } else { entry };
        let mut lo = padded_entry;
        let mut hi = padded_entry;
        for exit in self.exits(slot, centre_x) {
            let x = match exit {
                Exit::Feedthrough { x, .. } => x + pad,
                Exit::Socket { x, .. } => x,
            };
            lo = lo.min(x);
            hi = hi.max(x);
        }
        (lo, hi)
    }

    /// How much strength a track serving this (net, slot) pair must reserve
    /// for whatever it feeds. A socket exit needs `DESCEND_LENGTH` to
    /// survive its own fixed climb onward into the next band's gate plane;
    /// a skip-level shaft (`climb_levels`) needs no particular amount at
    /// all -- it can self-refresh mid-climb no matter how little it started
    /// with, as long as it started with more than 0 -- so using
    /// `DESCEND_LENGTH` here for every slot regardless of which kind of
    /// exit it has is simply the larger of the two genuine requirements,
    /// not a bound the shaft actually depends on.
    fn track_reserve(&self, _slot: usize) -> i32 {
        DESCEND_LENGTH
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

/// Levelise the DAG, order each row by barycentre, and give every gate an X.
fn build_floorplan(
    netlist: &Netlist,
    order: &[usize],
    producer_of: &HashMap<&str, usize>,
) -> Floorplan {
    let gate_count = netlist.gates.len();

    // ASAP levels: a gate sits one row deeper than its deepest predecessor.
    // `order` is topological, so one pass is enough.
    let mut level = vec![0usize; gate_count];
    for &g in order {
        let mut deepest = 0usize;
        for input in &netlist.gates[g].inputs {
            if let Some(&p) = producer_of.get(input.as_str()) {
                deepest = deepest.max(level[p] + 1);
            }
        }
        level[g] = deepest;
    }
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
                        hop_exit: Vec::new(),
                        hop_entry: Vec::new(),
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
    // skip-level shaft starts.
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

/// Assign every net's skip-level shafts: `hop_exit` / `hop_entry` for every
/// consecutive pair of channels it has to cross without a direct socket in
/// between.
///
/// A shaft's lane is identified by one number, `offset`, chosen once per
/// shaft from a strictly increasing global counter so that no two shafts
/// ever share one: at any absolute Y, a shaft climbing east sits at
/// `offset + y` (`assign_shafts` computes this at just the two Y values that
/// matter, `track_y` of each end, since the shaft only ever needs to *reach*
/// those two points, never anywhere in between). Two shafts' X therefore
/// differ by their constant `offset` difference at every Y both are active,
/// so spacing every `offset` at least `SHAFT_LANE_PITCH` apart is enough to
/// guarantee they can never collide, however their active Y ranges overlap.
///
/// `offset` is chosen so that even the earliest possible shaft (starting at
/// row 0's channel) starts east of every band's own content -- and since
/// `track_y` only grows for a later-starting shaft, every other shaft starts
/// even further east. No clearance search is needed against a band's own
/// gates, columns or tracks, unlike the pre-3D design's feed-through
/// columns -- a shaft's *tap* is simply never inside the range those ever
/// use.
///
/// That guarantee does not extend to a shaft's own Z-sweep once it leaves
/// its tap, though: two *different* nets' taps in the same channel, on two
/// different tracks, are only ever `SHAFT_LANE_PITCH` apart at minimum, but
/// whichever of the two has the larger tap value gives its own track a span
/// reaching all the way out to it -- and that span's west edge is ordinary
/// band content, which routinely sits well west of *every* tap. So the
/// larger-tapped track's own dust almost always physically covers the
/// smaller tap's X position too, just on a different Z. A sweep that
/// changes Z while sitting at the smaller tap's X necessarily crosses the
/// larger track's own Z along the way, at an X inside that track's laid
/// span -- an unintended electrical bridge between two unrelated nets. See
/// `shaft_rail_offset` and `move_through_shaft`'s doc comment for how this
/// is actually avoided.
fn assign_shafts(nets: &mut [Net], content_max_x: i32) {
    let base_offset = content_max_x + SHAFT_MARGIN - TRACK_Y_OFFSET;
    let mut lane = 0i32;
    for net in nets.iter_mut() {
        let hop_count = net.channels.len().saturating_sub(1);
        for slot in 0..hop_count {
            let offset = base_offset + lane * SHAFT_LANE_PITCH;
            lane += 1;
            net.hop_exit.push(offset + track_y(net.channels[slot]));
            net.hop_entry.push(offset + track_y(net.channels[slot + 1]));
        }
    }
}

/// How far east of its own tap a skip-level shaft's Z-sweep must travel
/// before it is safe from every other track's own laid span in the same
/// channel -- see `assign_shafts`'s doc comment for the collision this
/// avoids, and `move_through_shaft` for where this is actually spent.
///
/// One `SHAFT_LANE_PITCH` per shaft hop anywhere in the whole netlist is
/// always enough: two taps that share a channel are themselves at most
/// `(total hop count - 1) * SHAFT_LANE_PITCH` apart (`assign_shafts`'
/// strictly-increasing global `lane` counter), so a rail one whole
/// `total hop count * SHAFT_LANE_PITCH` past the *smaller* of the two is
/// guaranteed to clear the *larger* one's tap -- and therefore every track
/// it could possibly have extended out to reach it -- by at least one
/// `SHAFT_LANE_PITCH`, regardless of which of the two this value is
/// computed from (the `+ track_y(channel)` term every tap and every rail
/// alike carries is identical on both sides of that comparison, so it
/// cancels out and never affects the ordering).
fn shaft_rail_offset(nets: &[Net]) -> i32 {
    let total_hops: i32 = nets.iter().map(|net| net.channels.len().saturating_sub(1) as i32).sum();
    total_hops * SHAFT_LANE_PITCH
}

/// Left-edge track assignment: fill in every net's per-slot `tracks` index,
/// and return how many tracks each channel ended up needing.
///
/// Extracted verbatim from `compile`'s "Left-edge track assignment" section;
/// see the comment there for why one track can carry many nets.
///
/// `rail_offset` is `shaft_rail_offset`'s value, used to pad any member's
/// shaft-tap X (`Net::span_padded`) before packing -- reserving the reach
/// `move_through_shaft` will need for its own rail jog, so a later, closer
/// member can never be packed onto the same track within that reach. Every
/// other user of a net's span keeps using the real, unpadded position.
fn assign_tracks(plan: &Floorplan, nets: &mut [Net], channel_count: usize, rail_offset: i32) -> Vec<usize> {
    let mut track_count = vec![0usize; channel_count];
    for (channel, count) in track_count.iter_mut().enumerate() {
        let mut members: Vec<(i32, i32, usize, usize)> = Vec::new();
        for (n, net) in nets.iter().enumerate() {
            for (slot, &c) in net.channels.iter().enumerate() {
                if c == channel {
                    let (lo, hi) = net.span_padded(slot, &plan.centre_x, rail_offset);
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

/// Z layout: one row Z shared by every band, and every channel's track Zs
/// beneath it.
///
/// Every band reuses the exact same Z template -- only Y tells two bands
/// apart -- so `row_z` only has to be big enough for the *widest single
/// channel*'s own track count, not for the sum across every channel the way
/// the pre-3D design's per-row Z had to be. This is the heart of the size
/// win: Z no longer grows with the netlist's depth at all.
fn layout_row_z(channel_count: usize, track_count: &[usize]) -> (i32, Vec<Vec<i32>>) {
    let max_tracks = track_count.iter().copied().max().unwrap_or(0).max(1) as i32;
    // Three blocks clear of the row's own south socket leaves the first
    // ramp somewhere to start (same margin the pre-3D design used), plus
    // `DESCEND_LENGTH` more of buffer -- every track's own exit descends
    // (really ascends, in Y, but still travels north in Z -- see
    // `DESCEND_LENGTH`'s doc comment) that many cells further north of it,
    // and the deepest channel's last track is the one with the least room
    // to spare before that would run Z negative.
    let row_z = 3 + TRACK_SPACING * max_tracks + DESCEND_LENGTH;
    let track_z: Vec<Vec<i32>> = (0..channel_count)
        .map(|c| (0..track_count[c]).map(|k| row_z - 3 - TRACK_SPACING * (k as i32 + 1)).collect())
        .collect();
    (row_z, track_z)
}

/// `(entry_strength[net][slot], exit_strength[net][slot][exit_x])` -- see
/// `plan_strengths`.
type StrengthPlan = (Vec<Vec<u8>>, Vec<Vec<BTreeMap<i32, u8>>>);

/// Work out, for every net and every slot it appears in, the signal strength
/// arriving at that slot's ramp entry (`entry_strength[net][slot]`) and the
/// strength the track hands each of that slot's exits (`exit_strength[net]
/// [slot]`, keyed by exit X) before whatever continues from there -- a
/// descending ramp, or a skip-level shaft.
///
/// This has to run before any of the Ramps/Columns/Tracks passes write a
/// single block, for the same reason it always has: the Columns pass needs
/// to know what strength survived the previous slot's shaft before it can
/// lay whatever continues from there, but that number depends on the *next*
/// slot's track layout, which is not written until the later Tracks pass.
/// Computing every slot's numbers up front, net by net in slot order (each
/// slot's result depends only on the previous slot's, within the same net),
/// sidesteps that ordering conflict entirely.
///
/// A slot reached via a skip-level shaft (`slot > 0`) always arrives at
/// `MAX_SIGNAL_STRENGTH` if the shaft needed a Z-correction corner (which
/// ends in a mandatory repeater, same guarantee a socket's own corner gives
/// -- see `land_shaft`), or at whatever strength survived the climb
/// otherwise (no correction needed, so no repeater forced it fresh either).
/// Both branches are computed here exactly as `land_shaft` computes them
/// later, so the two can never silently disagree.
fn plan_strengths(
    nets: &[Net],
    plan: &Floorplan,
    track_z: &[Vec<i32>],
    lever_pin: &[Position],
    gate_pin: &[Position],
) -> StrengthPlan {
    let mut entry_strength: Vec<Vec<u8>> = Vec::with_capacity(nets.len());
    let mut exit_strength: Vec<Vec<BTreeMap<i32, u8>>> = Vec::with_capacity(nets.len());

    for net in nets {
        let mut net_entry = vec![0u8; net.channels.len()];
        let mut net_exit: Vec<BTreeMap<i32, u8>> = vec![BTreeMap::new(); net.channels.len()];

        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let z = track_z[channel][net.tracks[slot]];
            let entry = Position::new(net.entry_column(slot), gate_y(channel), z + RAMP_LENGTH);

            let arriving = if slot == 0 {
                let pin = match net.source {
                    Source::Lever(i) => lever_pin[i],
                    Source::Gate(g) => gate_pin[g],
                };
                let len = straight_run_length(pin, Facing::North, entry.offset(Facing::North));
                plan_straight_run(len, MAX_SIGNAL_STRENGTH, RAMP_LENGTH).1
            } else {
                // A slot reached via a skip-level shaft always arrives
                // fresh: `move_through_shaft` unconditionally ends in a
                // mandatory-repeater correction onto the destination
                // channel's own track Z, exactly like a socket's own
                // corner -- see `shaft_diagonal_z`'s doc comment for why
                // that correction is never skipped, even when the origin
                // and destination Z already happen to match.
                MAX_SIGNAL_STRENGTH
            };
            net_entry[slot] = arriving;

            // Slot 0 arrives at `gate_y`, RAMP_LENGTH below the track, and
            // has to climb to reach it; a slot reached via a skip-level
            // shaft is already sitting on the track (`land_shaft` landed it
            // there directly), so there is no climb left to subtract.
            let track_incoming = if slot == 0 { arriving - RAMP_LENGTH as u8 } else { arriving };
            let source_x = net.entry_column(slot);
            let (lo, hi) = net.span(slot, &plan.centre_x);
            let mut taps: BTreeSet<i32> = BTreeSet::new();
            taps.insert(source_x);
            for exit in net.exits(slot, &plan.centre_x) {
                taps.insert(exit.x());
            }
            net_exit[slot] = track_exit_strengths(source_x, lo, hi, &taps, track_incoming, net.track_reserve(slot));
        }

        entry_strength.push(net_entry);
        exit_strength.push(net_exit);
    }

    (entry_strength, exit_strength)
}

/// The Z a skip-level shaft's diagonal climb happens at -- deliberately
/// none of any real channel's own track Z (all of which are strictly less
/// than `row_z`, by construction of `layout_row_z`), so that nothing the
/// climb touches can ever coincide with a track that legitimately needs to
/// extend out to meet it.
///
/// This is not a defensive margin against a rare coincidence; it is
/// structural. A diagonal climb's own landing at `(x, y)` necessarily has
/// its supporting cell one step behind it at `(x - 1, y - 1)` (`climb_
/// levels`' doc comment). The destination channel's own track, tapping the
/// shaft at that same `(x, y)`, necessarily extends further west from
/// there to reach whatever it actually feeds -- so it necessarily has dust
/// at `(x - 1, y)` too, which necessarily needs a floor at `(x - 1, y - 1)`.
/// That is the exact cell the climb's own diagonal step already placed
/// dust on. Landing the diagonal directly on the destination track's own Z
/// makes this collision inevitable, not occasional -- whichever pass runs
/// second overwrites the other's cell, and for the reference circuits
/// measured here, it happens whenever a shaft's origin and destination
/// channels happen to pick the same track index (common, since index 0 is
/// the most heavily used track everywhere). Landing the diagonal somewhere
/// no real track ever reaches removes the possibility entirely, rather
/// than relying on it not coming up.
fn shaft_diagonal_z(row_z: i32) -> i32 {
    row_z + 10
}

/// Move a skip-level shaft's signal from a tap on its origin channel's
/// track onto a tap on its destination channel's track: a rail jog east
/// (mandatory repeater, so what follows always starts fresh -- see below
/// for why this leg exists at all), a perpendicular run out to `shaft_
/// diagonal_z` (mandatory repeater again), the climb itself, a
/// perpendicular run back onto the destination channel's own track Z
/// (mandatory repeater, so whatever continues from here can always assume
/// a fresh signal -- the same guarantee a socket's own corner gives), and a
/// final rail jog back west onto the real destination tap. See `shaft_
/// diagonal_z`'s doc comment for why the two Z corrections happen
/// unconditionally, never skipped even when the origin and destination Z
/// already happen to match.
///
/// The two rail jogs are what `shaft_rail_offset` exists for: without them,
/// each perpendicular Z run would sit at a *fixed* X equal to the raw tap
/// (`origin.x`, or the climb's landing X) the entire way from that tap's own
/// track Z out to `shaft_diagonal_z` -- crossing every *other* track Z that
/// channel has along the way, at that same X. `assign_shafts`'s doc comment
/// works through why that X is routinely inside some other, larger-tapped
/// track's own laid span in the same channel: an unintended electrical
/// bridge between two unrelated nets, sitting quietly until one net's
/// signal happens to disagree with the other's. Doing the Z sweep only
/// after jogging out to `origin.x + rail_offset` moves it somewhere no real
/// track's span ever reaches (`shaft_rail_offset`'s own doc comment), and
/// jogging back onto the real tap afterwards keeps the returned position
/// exactly where the rest of this module already expects it
/// (`net.hop_entry[slot]`'s X and Y, at the destination's own Z) -- nothing
/// downstream of this function has to know the rail detour ever happened.
/// Both jogs travel at a fixed Z equal to their own end's real tap, so they
/// only ever extend that same net's own track further out, never crossing
/// another track's Z at all; `assign_tracks`' padded packing
/// (`Net::span_padded`) is what keeps another *net* from ever being placed
/// within that reach on the same track index.
///
/// Returns the position that becomes the tap on the destination channel's
/// track (`net.hop_entry[slot]`'s X and Y, at the destination's own Z).
fn move_through_shaft(
    world: &mut World,
    origin: Position,
    row_z: i32,
    climb: i32,
    target_z: i32,
    rail_offset: i32,
    incoming_strength: u8,
) -> Position {
    let diagonal_z = shaft_diagonal_z(row_z);

    let rail_entry = Position::new(origin.x + rail_offset, origin.y, origin.z);
    lay_segment_to_corner(world, origin, rail_entry, incoming_strength);

    let pre_corner = Position::new(rail_entry.x, rail_entry.y, diagonal_z);
    lay_segment_to_corner(world, rail_entry, pre_corner, MAX_SIGNAL_STRENGTH);

    let (landing, ending) = climb_levels(world, pre_corner, Facing::East, climb, MAX_SIGNAL_STRENGTH);

    let post_corner = Position::new(landing.x, landing.y, target_z);
    lay_segment_to_corner(world, landing, post_corner, ending);

    let destination_tap = Position::new(post_corner.x - rail_offset, post_corner.y, post_corner.z);
    lay_segment_to_corner(world, post_corner, destination_tap, MAX_SIGNAL_STRENGTH);
    destination_tap
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

    let content_max_x = plan
        .centre_x
        .iter()
        .chain(plan.lever_x.iter())
        .copied()
        .max()
        .unwrap_or(ORIGIN_X)
        + GATE_HALF_WIDTH;
    assign_shafts(&mut nets, content_max_x);
    let rail_offset = shaft_rail_offset(&nets);
    let track_count = assign_tracks(&plan, &mut nets, channel_count, rail_offset);
    let (row_z, track_z) = layout_row_z(channel_count, &track_count);

    let shaft_max_x = nets
        .iter()
        .flat_map(|net| net.hop_exit.iter().chain(net.hop_entry.iter()))
        .copied()
        .max()
        .unwrap_or(content_max_x)
        + rail_offset;

    let size_x = content_max_x.max(shaft_max_x) + 4;
    let size_y = row_count as i32 * BAND_HEIGHT;
    // `+ 4` past `shaft_diagonal_z` even when no net actually has a
    // skip-level shaft: cheap, and keeps this formula from needing to know
    // whether one exists.
    let size_z = shaft_diagonal_z(row_z) + 4;

    let mut world = World::new(size_x.max(8), size_y.max(8), size_z.max(8));

    // ---------------------------------------------------------------
    // Emission
    // ---------------------------------------------------------------

    let mut gate_cell: Vec<NorCell> = Vec::with_capacity(netlist.gates.len());
    for _ in 0..netlist.gates.len() {
        gate_cell.push(NorCell { size: (0, 0, 0), input_offsets: Vec::new(), output_offset: (0, 0, 0) });
    }
    for (g, gate) in netlist.gates.iter().enumerate() {
        let origin = (plan.centre_x[g], gate_y(plan.row_of[g]), row_z);
        gate_cell[g] = place_nor_gate(&mut world, origin, gate.inputs.len());
    }

    let mut input_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    let mut lever_pin: Vec<Position> = Vec::with_capacity(netlist.inputs.len());
    for (i, name) in netlist.inputs.iter().enumerate() {
        let home = Position::new(plan.lever_x[i], gate_y(0), row_z);
        let (lever_pos, pin) = place_primary_input(&mut world, home);
        input_positions.insert(name.clone(), (lever_pos.x, lever_pos.y, lever_pos.z));
        lever_pin.push(pin);
    }

    let torch_of = |g: usize, cell: &NorCell| -> Position {
        Position::new(
            plan.centre_x[g] + cell.output_offset.0,
            gate_y(plan.row_of[g]) + cell.output_offset.1,
            row_z + cell.output_offset.2,
        )
    };

    let mut gate_pin: Vec<Position> = Vec::with_capacity(netlist.gates.len());
    let mut gate_output_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    for (g, cell) in gate_cell.iter().enumerate() {
        let torch = torch_of(g, cell);
        gate_output_positions.insert(netlist.gates[g].output.clone(), (torch.x, torch.y, torch.z));
        let pin = torch.offset(OUTPUT_DIRECTION);
        ensure_floor(&mut world, pin);
        world.set(pin.x, pin.y, pin.z, dust());
        gate_pin.push(pin);
    }

    // Strength planning: work out what every ramp's entry and every track's
    // exits will carry, before any of them are actually built. See
    // `plan_strengths` for why this has to happen up front rather than
    // inline in the passes below.
    let (entry_strength, exit_strength) = plan_strengths(&nets, &plan, &track_z, &lever_pin, &gate_pin);

    // Ramps first (including skip-level shafts). `move_between_layers` seals
    // the blocks around each single-band landing, and a seal only fills air
    // -- so anything that has to run *through* a sealed cell (the tracks,
    // and the columns) has to be laid afterwards to overwrite it.
    for (n, net) in nets.iter().enumerate() {
        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let z = track_z[channel][net.tracks[slot]];
            if slot == 0 {
                let entry = Position::new(net.entry_column(0), gate_y(channel), z + RAMP_LENGTH);
                move_between_layers(&mut world, entry, Facing::North, track_y(channel));
            }
            for exit in net.exits(slot, &plan.centre_x) {
                match exit {
                    Exit::Socket { .. } => {
                        // South, not north: `ROW_Z` is shared by every band
                        // (unlike the pre-3D design's per-row Z), so the
                        // corner/socket approach that continues from this
                        // climb's landing still has to travel further south
                        // to reach it. Climbing north here would have this
                        // land *further* from the row, forcing that
                        // approach run to backtrack south through the same
                        // Z the climb just used -- see `socket_approach_
                        // corners`'s landing parameter and `DESCEND_LENGTH`'s
                        // doc comment for why that backtrack is not merely
                        // wasteful but actively wrong: the approach run's
                        // own `ensure_floor` calls would overwrite this
                        // climb's own intermediate dust with solid stone.
                        let top = Position::new(exit.x(), track_y(channel), z);
                        move_between_layers(&mut world, top, Facing::South, gate_y(channel + 1));
                    }
                    Exit::Feedthrough { x, next_slot } => {
                        let next_channel = net.channels[next_slot];
                        let climb = track_y(next_channel) - track_y(channel);
                        let incoming = exit_strength[n][slot][&x];
                        let origin = Position::new(x, track_y(channel), z);
                        let target_z = track_z[next_channel][net.tracks[next_slot]];
                        move_through_shaft(&mut world, origin, row_z, climb, target_z, rail_offset, incoming);
                    }
                }
            }
        }
    }

    // Columns at each band's own `gate_y`: from a source pin up to its ramp,
    // and from a ramp's landing on to whatever socket it feeds. A
    // skip-level shaft needs no column of its own here -- the Ramps pass
    // above already carried it all the way to its destination channel's
    // track.
    for (n, net) in nets.iter().enumerate() {
        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let z = track_z[channel][net.tracks[slot]];
            let entry = Position::new(net.entry_column(slot), gate_y(channel), z + RAMP_LENGTH);
            if slot == 0 {
                let pin = match net.source {
                    Source::Lever(i) => lever_pin[i],
                    Source::Gate(g) => gate_pin[g],
                };
                lay_dust_run(
                    &mut world,
                    pin,
                    Facing::North,
                    entry.offset(Facing::North),
                    MAX_SIGNAL_STRENGTH,
                    RAMP_LENGTH,
                );
            }
            for exit in net.exits(slot, &plan.centre_x) {
                let Exit::Socket { gate, input_index, .. } = exit else { continue };
                let landing = Position::new(exit.x(), gate_y(channel + 1), z + DESCEND_LENGTH);
                let landing_strength = exit_strength[n][slot][&exit.x()] - DESCEND_LENGTH as u8;
                let (dx, dy, dz) = gate_cell[gate].input_offsets[input_index];
                let socket = Position::new(plan.centre_x[gate] + dx, gate_y(channel + 1) + dy, row_z + dz);
                let mut current = landing;
                let mut strength = landing_strength;
                for corner in socket_approach_corners(landing, socket, row_z) {
                    lay_segment_to_corner(&mut world, current, corner, strength);
                    // `corner` always follows `lay_segment_to_corner`'s own
                    // mandatory repeater, so it is always fresh.
                    current = corner;
                    strength = MAX_SIGNAL_STRENGTH;
                }
                lay_segment_to_socket(&mut world, current, socket, strength);
            }
        }
    }

    // Tracks last, so they overwrite the ramps' seal blocks where they have to
    // pass through them.
    for (n, net) in nets.iter().enumerate() {
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
            let track_incoming =
                if slot == 0 { entry_strength[n][slot] - RAMP_LENGTH as u8 } else { entry_strength[n][slot] };
            lay_track(&mut world, track_y(channel), z, source_x, lo, hi, &taps, track_incoming, net.track_reserve(slot));
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

    Ok(CompiledCircuit { world, input_positions, output_positions, gate_output_positions })
}

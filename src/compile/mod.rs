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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::redstone::simulator::position::{Position, HORIZONTAL};
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
/// Four would be just enough for `move_between_layers`' four-block ramp, but
/// not *safely*: the last step of a descending ramp leaves a **strongly
/// powered** support block one layer under the track plane, and a strongly
/// powered block drives every redstone dust next to it. At spacing 4 that
/// block would sit directly beneath the next track and inject this net's
/// signal into it. Five puts the ramp's landing on a Z row that can never hold
/// a track.
const TRACK_SPACING: i32 = 5;

/// How far a ramp travels horizontally while changing layer:
/// `move_between_layers` spends two blocks per Y level.
const RAMP_LENGTH: i32 = 2 * (TRACK_Y - GATE_Y);

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

/// `lay_dust_run` 的 `start` 永遠是一格已經是滿強度 15 的紅石粉（拉桿／
/// 火把旁邊的那格，或是剛被中繼器重新充能過的轉角），不是主動元件本身。
/// 粉接粉每多一格就再衰減 1，所以從 `start` 算起，下一格已經是 14，
/// 第 14 格是 1（還活著），第 15 格就是 0（死透，
/// `signal_strength_falls_off_over_distance_in_a_real_circuit` 驗證過這個
/// 邊界）。所以最多再鋪 14 格紅石粉，第 15 格一定換成中繼器。
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

/// 沿著 `direction` 鋪設紅石粉，從 `start`（不含，已經是主動元件，power
/// 15）鋪到 `stop_before`（不含），每 `MAX_DUST_RUN` 格插一個中繼器補強度。
/// `start` 到 `stop_before` 之間如果沒有任何格子，什麼都不做。
fn lay_dust_run(world: &mut World, start: Position, direction: Facing, stop_before: Position) {
    let mut counter = 0i32;
    let mut pos = start.offset(direction);
    while pos != stop_before {
        ensure_floor(world, pos);
        if counter >= MAX_DUST_RUN {
            world.set(pos.x, pos.y, pos.z, repeater(direction));
            counter = 0;
        } else {
            world.set(pos.x, pos.y, pos.z, dust());
            counter += 1;
        }
        pos = pos.offset(direction);
    }
}

/// 鋪一段路線，終點是一個**轉角**：下一段線要換一個軸繼續走，所以這一格
/// 必須是紅石粉，不能是中繼器 —— 中繼器只認一個方向，換軸就接不到訊號。
///
/// 為了保證轉角這一格一定是滿強度 15（讓下一段線可以放心地從頭算起自己
/// 的 15 格預算），轉角**前一格**強制放一個面朝這一段方向的中繼器，不管
/// 前面累計了幾格 —— 中繼器正前方的粉一定是新鮮的滿強度，這是它存在的
/// 意義。呼叫端保證 `start` 到 `corner` 至少有 2 格距離，所以「轉角前一
/// 格」不會撞到 `start` 自己。
fn lay_segment_to_corner(world: &mut World, start: Position, corner: Position) {
    let direction = direction_from(start, corner);
    let refresh_point = corner.offset(direction.opposite());

    lay_dust_run(world, start, direction, refresh_point);

    ensure_floor(world, refresh_point);
    world.set(refresh_point.x, refresh_point.y, refresh_point.z, repeater(direction));

    ensure_floor(world, corner);
    world.set(corner.x, corner.y, corner.z, dust());
}

/// 鋪一段路線的最後一段，終點是某個閘的輸入插座：這一格**一定**是中繼器，
/// 面朝這一段前進的方向 —— 而這個方向剛好就是「面朝支撐塊」的方向，因為
/// 插座本來就在支撐塊的正對外側。這是唯一能強充能支撐塊的方法：紅石粉
/// 水平方向不會充能方塊。
fn lay_segment_to_socket(world: &mut World, start: Position, socket: Position) {
    let direction = direction_from(start, socket);
    lay_dust_run(world, start, direction, socket);
    ensure_floor(world, socket);
    world.set(socket.x, socket.y, socket.z, repeater(direction));
}

/// 把 `pos` 四個水平方向的鄰居，凡是目前還空氣的都填成石頭。
///
/// 這是為了擋住紅石粉「往下爬」的對角規則：`dust_connections` 判斷能不能
/// 往下爬，只看水平鄰居**不是導體**（例如空氣）且鄰居正下方是紅石粉 ——
/// 完全不管那格紅石粉是哪個訊號、哪一段線路放的。`move_between_layers`
/// 疊出來的紅石粉如果水平鄰居剛好是空氣、空氣下面剛好是**另一條完全不
/// 相干的線**的紅石粉，兩條線就會透過這個對角線悄悄短接在一起 —— 這正是
/// 這個專案第一次踩到的「意外相鄰」：某一閘的輸入通道爬升／下降時，會
/// 從高處對角碰到同一閘輸出插頭那一整排紅石粉。把水平鄰居填實心，這條
/// 對角規則的前提（鄰居不是導體）就不成立，自然斷開。
fn seal_horizontal_neighbours(world: &mut World, pos: Position) {
    for direction in HORIZONTAL {
        let neighbour = pos.offset(direction);
        if world.get(neighbour.x, neighbour.y, neighbour.z).kind == BlockKind::Air {
            world.set(neighbour.x, neighbour.y, neighbour.z, stone());
        }
    }
}

/// 把訊號從 `entry`（一格滿強度 15 的紅石粉）沿著 `direction` 搬到高度
/// `target_y`，回傳落地那一格紅石粉的座標。
///
/// 每差一層就重複一次「中繼器 + 支撐塊 + 支撐塊上（或下）一層的紅石粉」
/// ——這跟 `place_nor_gate` 的輸入插座接法（外部主動元件強充能支撐塊，
/// 支撐塊再驅動疊在它上面的紅石粉）是同一招；`recompute_dust_strengths`
/// 判斷「強充能的方塊能不能驅動紅石粉」時，六個方向（含上下）一視同仁，
/// 所以疊在支撐塊**下面**一樣有效，能用同一招往下搬。
///
/// 每爬一層會沿 `direction` 前進兩格（中繼器一格、支撐塊一格），呼叫端
/// 得自己預留這段水平距離 —— 也就是 `RAMP_LENGTH`。每落地一次都會把新
/// 紅石粉的水平鄰居補實心（見 `seal_horizontal_neighbours`），避免它在
/// 半空中對角碰到別條線。
fn move_between_layers(
    world: &mut World,
    entry: Position,
    direction: Facing,
    target_y: i32,
) -> Position {
    let y_step = (target_y - entry.y).signum();
    let mut current = entry;
    while current.y != target_y {
        let repeater_pos = current.offset(direction);
        ensure_floor(world, repeater_pos);
        world.set(repeater_pos.x, repeater_pos.y, repeater_pos.z, repeater(direction));

        let support_pos = repeater_pos.offset(direction);
        world.set(support_pos.x, support_pos.y, support_pos.z, stone());

        let landing = Position::new(support_pos.x, support_pos.y + y_step, support_pos.z);
        ensure_floor(world, landing);
        world.set(landing.x, landing.y, landing.z, dust());
        seal_horizontal_neighbours(world, landing);

        current = landing;
    }
    current
}

/// Lay one east-west track: dust from `source_x` out to `min_x` and to
/// `max_x`, with a repeater inserted before the signal can run out.
///
/// `taps` are the X positions where a ramp joins or leaves the track. A
/// repeater on a tap silently cuts the route -- a repeater only reads what is
/// directly behind it and only drives what is directly in front, so a wire
/// that turns on top of one is not connected at all. When the 15-block budget
/// would force a repeater onto a tap, it goes on the last non-tap cell before
/// it instead; the run after it is then shorter than the budget, never longer.
fn lay_track(
    world: &mut World,
    z: i32,
    source_x: i32,
    min_x: i32,
    max_x: i32,
    taps: &BTreeSet<i32>,
) {
    for (end, step) in [(min_x, -1i32), (max_x, 1i32)] {
        let length = (end - source_x) * step;
        if length <= 0 {
            continue;
        }
        let direction = if step > 0 { Facing::East } else { Facing::West };
        let cells: Vec<i32> = (1..=length).map(|k| source_x + k * step).collect();

        // Pick the repeater cells before writing anything: where one repeater
        // ends up decides how much budget the cells after it have.
        let mut is_repeater = vec![false; cells.len()];
        let mut last_refresh: i64 = -1; // the source cell itself, at full strength
        let mut i = 0usize;
        while i < cells.len() {
            if (i as i64) - last_refresh <= MAX_DUST_RUN as i64 {
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

        for (k, &x) in cells.iter().enumerate() {
            let pos = Position::new(x, TRACK_Y, z);
            ensure_floor(world, pos);
            if is_repeater[k] {
                world.set(pos.x, pos.y, pos.z, repeater(direction));
            } else {
                world.set(pos.x, pos.y, pos.z, dust());
            }
        }
    }
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
/// Extracted verbatim from `compile`'s "Left-edge track assignment" section;
/// see the comment there for why one track can carry many nets.
fn assign_tracks(plan: &Floorplan, nets: &mut [Net], channel_count: usize) -> Vec<usize> {
    let mut track_count = vec![0usize; channel_count];
    for (channel, count) in track_count.iter_mut().enumerate() {
        let mut members: Vec<(i32, i32, usize, usize)> = Vec::new();
        for (n, net) in nets.iter().enumerate() {
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
    let track_count = assign_tracks(&plan, &mut nets, channel_count);
    let (row_z, track_z) = layout_z(row_count, channel_count, &track_count);

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

    let mut world = World::new(size_x.max(8), WORLD_HEIGHT, size_z.max(8));

    // ---------------------------------------------------------------
    // Emission
    // ---------------------------------------------------------------

    let mut gate_cell: Vec<NorCell> = Vec::with_capacity(netlist.gates.len());
    for _ in 0..netlist.gates.len() {
        gate_cell.push(NorCell { size: (0, 0, 0), input_offsets: Vec::new(), output_offset: (0, 0, 0) });
    }
    for (g, gate) in netlist.gates.iter().enumerate() {
        let origin = (plan.centre_x[g], GATE_Y, row_z[plan.row_of[g]]);
        gate_cell[g] = place_nor_gate(&mut world, origin, gate.inputs.len());
    }

    let mut input_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    let mut lever_pin: Vec<Position> = Vec::with_capacity(netlist.inputs.len());
    for (i, name) in netlist.inputs.iter().enumerate() {
        let home = Position::new(plan.lever_x[i], GATE_Y, row_z[0]);
        let (lever_pos, pin) = place_primary_input(&mut world, home);
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
        ensure_floor(&mut world, pin);
        world.set(pin.x, pin.y, pin.z, dust());
        gate_pin.push(pin);
    }

    // Ramps first. `move_between_layers` seals the blocks around each landing,
    // and a seal only fills air -- so anything that has to run *through* a
    // sealed cell (the tracks, and the columns) has to be laid afterwards to
    // overwrite it.
    for net in &nets {
        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let z = track_z[channel][net.tracks[slot]];
            let entry = Position::new(net.entry_column(slot), GATE_Y, z + RAMP_LENGTH);
            move_between_layers(&mut world, entry, Facing::North, TRACK_Y);
            for exit in net.exits(slot, &plan.centre_x) {
                let top = Position::new(exit.x(), TRACK_Y, z);
                move_between_layers(&mut world, top, Facing::North, GATE_Y);
            }
        }
    }

    // Columns at `GATE_Y`: from a source pin up to its ramp, and from a ramp's
    // landing on to whatever it feeds.
    for net in &nets {
        for slot in 0..net.channels.len() {
            let channel = net.channels[slot];
            let z = track_z[channel][net.tracks[slot]];
            let entry = Position::new(net.entry_column(slot), GATE_Y, z + RAMP_LENGTH);
            if slot == 0 {
                let pin = match net.source {
                    Source::Lever(i) => lever_pin[i],
                    Source::Gate(g) => gate_pin[g],
                };
                lay_dust_run(&mut world, pin, Facing::North, entry.offset(Facing::North));
            }
            for exit in net.exits(slot, &plan.centre_x) {
                let landing = Position::new(exit.x(), GATE_Y, z - RAMP_LENGTH);
                match exit {
                    Exit::Socket { gate, input_index, .. } => {
                        let (dx, dy, dz) = gate_cell[gate].input_offsets[input_index];
                        let socket = Position::new(
                            plan.centre_x[gate] + dx,
                            GATE_Y + dy,
                            row_z[plan.row_of[gate]] + dz,
                        );
                        if socket.x == landing.x {
                            lay_segment_to_socket(&mut world, landing, socket);
                        } else {
                            let corner =
                                Position::new(landing.x, GATE_Y, row_z[plan.row_of[gate]]);
                            lay_segment_to_corner(&mut world, landing, corner);
                            lay_segment_to_socket(&mut world, corner, socket);
                        }
                    }
                    Exit::Feedthrough { x, next_slot } => {
                        let next_channel = net.channels[next_slot];
                        let next_z = track_z[next_channel][net.tracks[next_slot]];
                        let next_entry = Position::new(x, GATE_Y, next_z + RAMP_LENGTH);
                        lay_dust_run(
                            &mut world,
                            landing,
                            Facing::North,
                            next_entry.offset(Facing::North),
                        );
                    }
                }
            }
        }
    }

    // Tracks last, so they overwrite the ramps' seal blocks where they have to
    // pass through them.
    for net in &nets {
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
            lay_track(&mut world, z, source_x, lo, hi, &taps);
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

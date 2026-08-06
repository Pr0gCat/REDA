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
//! # 佈局與繞線：故意做到最簡單
//!
//! 這個階段的目標是「能動」，不是「小」。佈局是一閘一列，沿著 Z 軸排開，
//! 間距寬鬆到不用擔心意外相鄰。繞線是直線紅石粉沿著專屬通道走，每 15
//! 格插一個中繼器補訊號，末端一定接一個中繼器把訊號強充能進下一個閘的
//! 支撐塊 —— 紅石粉本身水平方向不會充能方塊（只會弱充能正下方），所以
//! 每一段線都必須用主動元件收尾。
//!
//! 每個訊號（不管是外部輸入還是某個閘的輸出）都有專屬的繞線通道，通道
//! 之間、通道跟閘本體之間都留了遠超過 3 格的間隔，避免意外的訊號路徑。

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::redstone::simulator::position::{Position, HORIZONTAL};
use crate::redstone::world::block::{BlockKind, BlockState, Facing};
use crate::redstone::world::storage::World;

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

fn lever(on: bool) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Lever;
    state.name = "minecraft:lever".to_string();
    state.lit = on;
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
// 佈局與繞線
// ---------------------------------------------------------------------

/// 兩列閘之間的間距，寬鬆到繞線通道不會碰到任何一列的本體。
const ROW_SPACING: i32 = 24;

/// 訊號通道離閘本體中軸的基礎距離。
const AVENUE_OFFSET: i32 = 12;

/// 相鄰兩條訊號通道之間的間距 —— 保證留下遠超過 3 格的緩衝。
const LANE_SPACING: i32 = 6;

/// 通道（線段二：跨列的長距離那一段）所在的高度，跟閘本體、插座所在的
/// `GATE_Y` 刻意分開一層。
///
/// 每條邊的通道用不同的 X（或 Z）互相錯開，彼此之間不會撞在一起；但
/// 一條「跨很多列」的通道，跟另一條邊在它自己那一列附近的短插頭
/// （線段一、三，就在 `GATE_Y` 那層）投影到平面上完全有可能交叉——通道
/// 要穿過中間所有列，插頭卻是從閘本體橫向拉出去的，兩者的座標範圍本來
/// 就會重疊。把通道整段墊高到 `LANE_Y`，插頭留在 `GATE_Y`，兩者處在不同
/// 高度，投影上的交叉就不會變成真的相鄰——這是先前一版線路會震盪的原因：
/// 一條邊的通道跟另一條邊的插頭在同一層的同一格重疊，等於把兩個不相干
/// 的訊號短接在一起。
const GATE_Y: i32 = 1;
const LANE_Y: i32 = 3;

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
    /// 每個輸出訊號的讀取座標
    pub output_positions: BTreeMap<String, (i32, i32, i32)>,
}

/// 水平方向沿著哪一軸移動：東西是 X 軸，南北是 Z 軸。
fn axis_of(direction: Facing) -> Axis {
    match direction {
        Facing::West | Facing::East => Axis::X,
        Facing::South | Facing::North => Axis::Z,
        Facing::Up | Facing::Down => unreachable!("輸入/輸出方向只會是水平的"),
    }
}

/// 沿著該軸移動時，這個方向對應 +1 還是 -1。
fn sign_of(direction: Facing) -> i32 {
    match direction {
        Facing::East | Facing::South => 1,
        Facing::West | Facing::North => -1,
        Facing::Up | Facing::Down => unreachable!("輸入/輸出方向只會是水平的"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    Z,
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
/// 得自己預留這段水平距離。每落地一次都會把新紅石粉的水平鄰居補實心
/// （見 `seal_horizontal_neighbours`），避免它在半空中對角碰到別條線。
fn move_between_layers(world: &mut World, entry: Position, direction: Facing, target_y: i32) -> Position {
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

/// 插座朝南時，「回到插座」那一步跟「跨列」共用同一個軸（Z），沒辦法
/// 直接落地到 `socket.z`——見 `route_to_input` 的說明。這是那一步額外的
/// 緩衝距離：跨列的下降點先停在 `socket.z` 之外這麼多格，把 X 切回
/// `socket.x` 之後，再單獨沿 Z 走完最後這一小段，方向才會是真正朝插座
/// 前進、而不是跟橫向切換的那一步疊在一起。只要小於 `ROW_SPACING` 的一半
/// 就不會撞進下一列。
const SOCKET_APPROACH_BUFFER: i32 = 4;

/// 把一個訊號從它的來源接到某個閘的某個輸入插座，中間用專屬通道繞線，
/// 末端一定是一個面朝支撐塊的中繼器 —— 這是唯一能強充能支撐塊的方法。
///
/// 路線是三段直線：先沿著跟目的地方向同一軸移動到專屬通道，再沿另一軸
/// 走到目的地那一列，最後沿原本的軸走回插座。`lane_index` 保證每條邊
/// 都有自己專屬的通道，不會跟其他邊共用同一條線。
///
/// 跨列的那一段（線段二）刻意墊高到 `LANE_Y`，跟閘本體、插座所在的
/// `GATE_Y` 分開一層再落地——見 `LANE_Y` 的說明，這是避免不同邊的通道跟
/// 別條邊的插頭意外相鄰的關鍵。
///
/// 插座朝西／東（軸 X）時，「切進通道」跟「切出通道回插座」都沿 X——
/// 剛好跟插座本來的進入方向一樣，這兩段可以直接當作頭尾兩段，不必再多走
/// 一段：`compile()` 裡每一列閘的本體都放在同一個 X，列跟列只靠 Z 拉開，
/// 所以「跨列」永遠沿 Z，跟這裡的頭尾（沿 X）是不同軸，天然不會疊在一起。
///
/// 插座朝南（軸 Z）時就不是這樣了：插座的進入方向也是 Z，跟跨列共用
/// 同一個軸。如果照西／東的公式直接切齊 `socket.z` 落地，X 還停在專屬
/// 通道的 `lane_x` 上，不在 `socket.x`——`waypoint_a` 跟 `waypoint_b` 的
/// X、Z 會完全相等（`compile()` 裡所有閘的本體共用同一個 X，只有 Z 隨列
/// 而變），變成起點等於終點，`direction_from` 判斷不出方向而 panic。這是
/// 這個電路第一次真正逼出這條路徑：更早的測試電路每個閘最多兩個輸入，
/// 只用得到西／東方向，從沒排過三輸入閘、也就沒用過南方向插座。
/// 修法是幫南方向插座多插一段：跨列先在專屬通道（沿 X 偏移的
/// `lane_x`）上，停在插座之外 `SOCKET_APPROACH_BUFFER` 格的緩衝點，
/// 而不是插座本身；把 X 切回 `socket.x` 之後（新的一段轉角），才單獨
/// 沿 Z 走完最後這一小段真正進插座——方向純粹是 Z，跟中繼器該面朝的
/// 方向（朝支撐塊）一致。
fn route_to_input(
    world: &mut World,
    source_pin: Position,
    socket: Position,
    input_direction: Facing,
    lane_index: i32,
) {
    let axis = axis_of(input_direction);
    let sign = sign_of(input_direction);
    let lane_offset = AVENUE_OFFSET + lane_index * LANE_SPACING;

    // `tail_start` 是最後一段（`lay_segment_to_socket`）的起點。軸 X 的
    // 情形頭尾都沿 X，`waypoint_b` 本身就是插座前的起點；軸 Z 的情形需要
    // 一個額外的緩衝點（見上面的說明），`waypoint_b` 只是跨列結束、還沒
    // 切回 `socket.x` 的中繼站。
    let (waypoint_a, waypoint_b, tail_start) = match axis {
        Axis::X => {
            let lane_x = socket.x + sign * lane_offset;
            let waypoint_a = Position::new(lane_x, source_pin.y, source_pin.z);
            let waypoint_b = Position::new(lane_x, source_pin.y, socket.z);
            (waypoint_a, waypoint_b, waypoint_b)
        }
        Axis::Z => {
            let lane_x = socket.x + lane_offset;
            let approach_z = socket.z + sign * SOCKET_APPROACH_BUFFER;
            let waypoint_a = Position::new(lane_x, source_pin.y, source_pin.z);
            let waypoint_b = Position::new(lane_x, source_pin.y, approach_z);
            let tail_start = Position::new(socket.x, source_pin.y, approach_z);
            (waypoint_a, waypoint_b, tail_start)
        }
    };

    lay_segment_to_corner(world, source_pin, waypoint_a);

    let crossing_direction = direction_from(waypoint_a, waypoint_b);
    let steps_per_layer = 2; // move_between_layers 每爬一層前進兩格
    let layers = LANE_Y - GATE_Y;
    let horizontal_margin = steps_per_layer * layers;

    // 從 waypoint_a 爬到 LANE_Y，再從 waypoint_b 往回退 horizontal_margin
    // 格算出下降的起點,兩段爬升／下降各自消耗 horizontal_margin 格水平
    // 距離,中間剩下的距離才是真正在 LANE_Y 那層鋪線的部分。
    let lane_entry = move_between_layers(world, waypoint_a, crossing_direction, LANE_Y);

    let mut descent_target = waypoint_b;
    for _ in 0..horizontal_margin {
        descent_target = descent_target.offset(crossing_direction.opposite());
    }
    let descent_entry = Position::new(descent_target.x, LANE_Y, descent_target.z);

    lay_segment_to_corner(world, lane_entry, descent_entry);

    let landed = move_between_layers(world, descent_entry, crossing_direction, GATE_Y);
    debug_assert_eq!(landed, waypoint_b, "下降後必須精確落在 waypoint_b 上");

    if tail_start != waypoint_b {
        lay_segment_to_corner(world, waypoint_b, tail_start);
    }
    lay_segment_to_socket(world, tail_start, socket);
}

/// 把一個外部輸入的拉桿與它的起始紅石粉畫進世界，回傳這個訊號的來源
/// （拉桿本身的座標，以及它驅動的第一格紅石粉）。
///
/// 拉桿是主動元件，直接驅動緊鄰的紅石粉（`drives_dust` 不分方向、不分
/// 強弱充能）——不需要像閘的輸入插座那樣經過支撐塊。這條紅石粉跟閘的
/// 輸出插座、輸入插座刻意放在同一個 y 層，讓 `route_to_input` 全程只需
/// 處理水平方向。
///
/// 拉桿的家永遠在所有閘的列之北（見 `compile` 裡 `base_z` 的算法），
/// 所以每個閘都在拉桿的南邊。訊號的起點刻意放在拉桿**南側**：往南正是
/// 繞線接下來一定要走的方向，這樣走線不會反過來往北撞回拉桿本身，把它
/// 蓋成紅石粉。
fn place_primary_input(world: &mut World, home: Position) -> (Position, Position) {
    world.set(home.x, home.y, home.z, lever(false));
    ensure_floor(world, home);

    let pin = home.offset(Facing::South);
    ensure_floor(world, pin);
    world.set(pin.x, pin.y, pin.z, dust());

    (home, pin)
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

    let gate_count = netlist.gates.len();
    let primary_input_count = netlist.inputs.len() as i32;
    let max_edges: i32 = netlist.gates.iter().map(|g| g.inputs.len() as i32).sum();

    // 世界原點刻意留出足夠的正向緩衝，讓所有通道座標（包含往負方向延伸
    // 的西側／北側通道）換算成世界座標後都還是非負值。
    let base_z = (primary_input_count + 1) * ROW_SPACING;
    let base_x = AVENUE_OFFSET + (max_edges + 1) * LANE_SPACING;

    let size_x = base_x * 2 + AVENUE_OFFSET + (max_edges + 1) * LANE_SPACING + 20;
    let size_z = base_z + (gate_count as i32 + 1) * ROW_SPACING + 20;
    let size_y = LANE_Y + 3;

    let mut world = World::new(size_x.max(10), size_y, size_z.max(10));

    const GATE_X: i32 = 0; // 加上 base_x 之後才是世界座標

    // 先把每個閘的本體畫出來，記住它的原點跟 cell，方便之後查詢插座座標。
    let mut gate_origin: Vec<Position> = vec![Position::new(0, 0, 0); gate_count];
    let mut gate_cell: Vec<NorCell> = Vec::with_capacity(gate_count);

    // 先分配空間再填入，因為畫圖需要照拓樸順序，但索引要照原本的閘索引存。
    for _ in 0..gate_count {
        gate_cell.push(NorCell { size: (0, 0, 0), input_offsets: Vec::new(), output_offset: (0, 0, 0) });
    }

    for (row, &gate_index) in order.iter().enumerate() {
        let row = row as i32;
        let origin = (base_x + GATE_X, GATE_Y, base_z + row * ROW_SPACING);
        let cell = place_nor_gate(&mut world, origin, netlist.gates[gate_index].inputs.len());
        gate_origin[gate_index] = Position::new(origin.0, origin.1, origin.2);
        gate_cell[gate_index] = cell;
    }

    // 每個外部輸入的拉桿家：獨立的一列，在所有閘的列之前（負方向），彼此
    // 間隔跟閘的列間距一樣寬鬆。
    let mut primary_source: HashMap<&str, Position> = HashMap::new();
    let mut input_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    for (index, name) in netlist.inputs.iter().enumerate() {
        let home = Position::new(
            base_x + GATE_X,
            GATE_Y,
            base_z - (index as i32 + 1) * ROW_SPACING,
        );
        let (lever_pos, pin) = place_primary_input(&mut world, home);
        primary_source.insert(name.as_str(), pin);
        input_positions.insert(name.clone(), (lever_pos.x, lever_pos.y, lever_pos.z));
    }

    // 每個閘輸出的訊號來源：輸出火把再往外一格的紅石粉。
    let mut gate_source: HashMap<&str, Position> = HashMap::new();
    for (gate_index, gate) in netlist.gates.iter().enumerate() {
        let origin = gate_origin[gate_index];
        let cell = &gate_cell[gate_index];
        let torch_pos = Position::new(
            origin.x + cell.output_offset.0,
            origin.y + cell.output_offset.1,
            origin.z + cell.output_offset.2,
        );
        let pin = torch_pos.offset(OUTPUT_DIRECTION);
        ensure_floor(&mut world, pin);
        world.set(pin.x, pin.y, pin.z, dust());
        gate_source.insert(gate.output.as_str(), pin);
    }

    // 依拓樸順序逐一把每個閘的輸入接上它的驅動訊號，這樣繞線通道的
    // lane_index 也會照固定順序分配，結果可重現。
    let mut lane_index = 0i32;
    for &gate_index in &order {
        let gate = &netlist.gates[gate_index];
        let origin = gate_origin[gate_index];
        for (input_index, input_name) in gate.inputs.iter().enumerate() {
            let source_pin = primary_source
                .get(input_name.as_str())
                .or_else(|| gate_source.get(input_name.as_str()))
                .copied()
                .expect("驅動來源已經在編譯開頭驗證過，這裡一定找得到");

            let direction = INPUT_DIRECTIONS[input_index];
            let (dx, dy, dz) = gate_cell[gate_index].input_offsets[input_index];
            let socket = Position::new(origin.x + dx, origin.y + dy, origin.z + dz);

            route_to_input(&mut world, source_pin, socket, direction, lane_index);
            lane_index += 1;
        }
    }

    let mut output_positions: BTreeMap<String, (i32, i32, i32)> = BTreeMap::new();
    for output_name in &netlist.outputs {
        let gate_index = netlist.gates.iter().position(|g| &g.output == output_name).unwrap();
        let origin = gate_origin[gate_index];
        let cell = &gate_cell[gate_index];
        let torch_pos = Position::new(
            origin.x + cell.output_offset.0,
            origin.y + cell.output_offset.1,
            origin.z + cell.output_offset.2,
        );
        output_positions.insert(output_name.clone(), (torch_pos.x, torch_pos.y, torch_pos.z));
    }

    Ok(CompiledCircuit { world, input_positions, output_positions })
}

//! 火把：這個專案唯一的閘元件（反相器）。
//!
//! 規則很簡單：**附著的方塊被充能就熄滅，否則就亮**。這個反相就是火把
//! 存在的全部理由 —— 少了它，這個 cell library 裡沒有任何東西能做反相。
//!
//! 狀態改變不是瞬間的：排在 2 個 game tick（= 1 個 redstone tick）之後。
//!
//! 火把也會 burnout：60 game tick 內被迫改變超過 8 次就燒毀，燒毀時強制
//! 熄滅、停止回應輸入。但**燒毀會恢復** —— 舊的改變紀錄滑出視窗後就不再
//! 計入，計數降回門檻以下燒毀就解除。把 burnout 做成永久失效會逼著電路
//! 收斂，讓模擬器在該回報「這個電路在振盪」的地方假裝穩定，那是錯的。

use crate::redstone::rules::taxonomy::{power_emitted_toward, BlockPower};
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::{block_power_at, block_signal_at};
use crate::redstone::simulator::schedule::TickPriority;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};
use crate::redstone::world::storage::World;

/// 火把從被迫改變到真正翻轉之間的延遲：2 game tick（1 redstone tick）。
pub const TORCH_DELAY_GAME_TICKS: u64 = 2;

/// burnout 判定的滑動視窗。
pub const BURNOUT_WINDOW_GAME_TICKS: u64 = 60;

/// 視窗內超過這個改變次數就燒毀。
pub const BURNOUT_CHANGE_LIMIT: usize = 8;

/// 火把附著在哪一格。
///
/// 立式火把附著在正下方；牆上火把附著在 `facing` 的**反方向**
/// —— `facing` 記錄的是火把頭朝外的方向，所以頭朝東的火把掛在西邊。
///
/// 不是火把的方塊回傳 `None`；缺少 `facing` 的牆上火把（不該發生，但
/// 讀檔可能給出不完整的資料）也回傳 `None`，而不是憑空猜一個方向。
pub fn torch_support_position(state: &BlockState, pos: Position) -> Option<Position> {
    match state.kind {
        BlockKind::Torch => Some(pos.down()),
        BlockKind::WallTorch => state.facing.map(|facing| pos.offset(facing.opposite())),
        _ => None,
    }
}

/// 火把在下一刻**應該**是什麼狀態。
///
/// 附著的方塊被充能（不論強弱）→ 熄滅；否則 → 亮。這個反相就是火把的
/// 全部用途。
///
/// 對不是火把的方塊，或缺少附著資訊的牆上火把，回傳目前的 `lit` —— 讓
/// 呼叫端不會誤把「這個位置根本不適用」當成「需要改變」。
pub fn torch_should_be_lit(world: &World, pos: Position) -> bool {
    let state = world.get(pos.x, pos.y, pos.z);
    match torch_support_position(state, pos) {
        Some(support) => block_power_at(world, support) == BlockPower::None,
        None => state.lit,
    }
}

/// 這個火把是不是燒毀了。
///
/// 60 game tick 內被迫改變超過 8 次即燒毀。**會恢復**——早於視窗起點的
/// 改變紀錄不再計入；計數一旦降回門檻以下，燒毀就解除。把燒毀做成永久
/// 狀態是先前一個專案的錯誤，那會讓模擬器對振盪電路給出錯誤的收斂結果。
pub fn is_burned_out(changes: &[u64], now: u64) -> bool {
    let window_start = now.saturating_sub(BURNOUT_WINDOW_GAME_TICKS);
    let recent_changes = changes.iter().filter(|&&at| at > window_start).count();
    recent_changes > BURNOUT_CHANGE_LIMIT
}

/// 這個 kind 是不是「二極體」（中繼器或比較器）—— 兩者共用鎖存規則。
fn is_diode(kind: BlockKind) -> bool {
    matches!(kind, BlockKind::Repeater | BlockKind::Comparator)
}

/// 給定朝向，回傳跟它垂直的兩個水平方向 —— 就是中繼器兩側的方向。
///
/// 中繼器只會朝水平四個方向之一；`Up`/`Down` 不是合法的中繼器朝向，這裡
/// 仍然回傳一組方向以避免窮舉時留下不可能的分支，但不會有呼叫端用得到
/// 這個結果 —— 讀檔給出的中繼器一定是水平朝向。
fn horizontal_side_directions(facing: Facing) -> [Facing; 2] {
    match facing {
        Facing::North | Facing::South => [Facing::East, Facing::West],
        Facing::East | Facing::West => [Facing::North, Facing::South],
        Facing::Up | Facing::Down => [Facing::East, Facing::West],
    }
}

/// 中繼器的輸入位置（正後方）。
///
/// 中繼器只從它朝向的反方向讀取輸入 —— 這是二極體的定義，正前方與側面
/// 都不算輸入。缺少 `facing` 的資料回傳 `None`，不猜方向。
pub fn repeater_input_position(state: &BlockState, pos: Position) -> Option<Position> {
    state.facing.map(|facing| pos.offset(facing.opposite()))
}

/// 中繼器兩側的位置，用來判斷鎖存。
///
/// 缺少 `facing` 的資料回傳 `[None, None]`。
pub fn repeater_side_positions(state: &BlockState, pos: Position) -> [Option<Position>; 2] {
    match state.facing {
        None => [None, None],
        Some(facing) => {
            let [a, b] = horizontal_side_directions(facing);
            [Some(pos.offset(a)), Some(pos.offset(b))]
        }
    }
}

/// 這個中繼器現在被鎖住了嗎。
///
/// 側面有**已充能的**中繼器或比較器正對著它就會鎖住。鎖存是**立即**生效的，
/// 不排程 —— 這是它與其他所有中繼器行為的差別。
pub fn repeater_is_locked(world: &World, pos: Position) -> bool {
    let state = world.get(pos.x, pos.y, pos.z);
    let Some(facing) = state.facing else {
        return false;
    };

    for side_facing in horizontal_side_directions(facing) {
        let side_pos = pos.offset(side_facing);
        let side_state = world.get(side_pos.x, side_pos.y, side_pos.z);
        let faces_into_us = side_state.facing == Some(side_facing.opposite());
        if is_diode(side_state.kind) && side_state.lit && faces_into_us {
            return true;
        }
    }
    false
}

/// 中繼器的輸入現在有訊號嗎。
///
/// 跟紅石粉一樣，中繼器可以被兩種東西驅動：正後方的元件直接送出的訊號，
/// 或是正後方那格方塊本身被（其他來源）強充能、再把訊號轉送過來。方向
/// 只看正後方一格 —— 這正是它不會把訊號往回傳的原因。
pub fn repeater_input_is_powered(world: &World, pos: Position) -> bool {
    let state = world.get(pos.x, pos.y, pos.z);
    let Some(facing) = state.facing else {
        return false;
    };
    let Some(input_pos) = repeater_input_position(state, pos) else {
        return false;
    };

    let input_state = world.get(input_pos.x, input_pos.y, input_pos.z);
    if power_emitted_toward(input_state, facing).drives_dust {
        return true;
    }

    block_signal_at(world, input_pos).0 == BlockPower::Strong
}

/// 這個中繼器該用哪個優先權排程。
///
/// 輸出正對著另一個中繼器或比較器的背面或側面 —— 最高優先權，確保它的
/// 鎖存判斷在同一個 game tick 裡先於其他排程解析。其餘情況下，正在關閉
/// 的比正在開啟的優先權高 —— 這兩級優先權存在的唯一理由，就是讓相鄰
/// 中繼器誰先動不再是實作細節。
pub fn repeater_priority(world: &World, pos: Position, turning_off: bool) -> TickPriority {
    let state = world.get(pos.x, pos.y, pos.z);
    let Some(facing) = state.facing else {
        return TickPriority::High;
    };

    let front = pos.offset(facing);
    let front_state = world.get(front.x, front.y, front.z);
    let faces_back_or_side_of_diode = is_diode(front_state.kind)
        && front_state
            .facing
            .is_some_and(|target_facing| target_facing != facing.opposite());

    if faces_back_or_side_of_diode {
        TickPriority::Highest
    } else if turning_off {
        TickPriority::Higher
    } else {
        TickPriority::High
    }
}

/// 中繼器的延遲換算成 game tick。
///
/// `delay` 應該是 1..=4；0 視為 1，因為讀檔可能給出不完整的資料。
/// 1 個紅石刻 = 2 個 game tick。
pub fn repeater_delay_game_ticks(state: &BlockState) -> u64 {
    let redstone_ticks = if state.delay == 0 { 1 } else { state.delay };
    redstone_ticks as u64 * 2
}

/// 比較器的後方輸入位置（`facing` 的反方向）。
///
/// 跟中繼器一樣是二極體的定義：主訊號只從正後方讀，正前方、側面都不算。
/// 缺少 `facing` 的資料回傳 `None`，不猜方向。
pub fn comparator_rear_position(state: &BlockState, pos: Position) -> Option<Position> {
    state.facing.map(|facing| pos.offset(facing.opposite()))
}

/// 比較器兩側的輸入位置，用來讀比較訊號。
///
/// 缺少 `facing` 的資料回傳 `[None, None]`。
pub fn comparator_side_positions(state: &BlockState, pos: Position) -> [Option<Position>; 2] {
    match state.facing {
        None => [None, None],
        Some(facing) => {
            let [a, b] = horizontal_side_directions(facing);
            [Some(pos.offset(a)), Some(pos.offset(b))]
        }
    }
}

/// 這個比較器是不是減法模式。
///
/// `mode` 屬性缺席一律視為比較模式 —— 這是原版的預設值，也是 A1 階段
/// 保留未建模屬性的直接後果：讀檔給什麼字串就照什麼字串判斷，缺席不猜。
pub fn comparator_is_subtract_mode(state: &BlockState) -> bool {
    state.extra_properties.get("mode").is_some_and(|mode| mode == "subtract")
}

/// 比較器後方讀進來的主訊號強度。
///
/// 跟中繼器的輸入一樣可以被兩種東西驅動：正後方元件直接送出的訊號，或是
/// 正後方那格方塊本身被（其他來源）強充能。兩者都回傳實際的類比強度，
/// 不是布林值 —— 硬編成「有訊號就 15」等於把比較器變成中繼器。
fn comparator_rear_input(world: &World, pos: Position) -> u8 {
    let state = world.get(pos.x, pos.y, pos.z);
    let Some(facing) = state.facing else {
        return 0;
    };
    let Some(rear_pos) = comparator_rear_position(state, pos) else {
        return 0;
    };

    let rear_state = world.get(rear_pos.x, rear_pos.y, rear_pos.z);
    let direct = power_emitted_toward(rear_state, facing);
    if direct.drives_dust {
        return direct.strength;
    }

    let (kind, strength) = block_signal_at(world, rear_pos);
    if kind == BlockPower::Strong {
        strength
    } else {
        0
    }
}

/// 從某個位置讀進來的訊號強度，給比較器的側面輸入用。
///
/// **側面只接受強充能** —— 弱充能的方塊餵不進比較器。搞錯這條會產生
/// 「模擬會動、遊戲裡不會動」的電路：紅石粉墊在側面方塊底下是很常見的
/// 接法，在真實遊戲裡完全餵不進比較器，模擬器若照單全收就會跟遊戲行為
/// 分歧。
pub fn comparator_side_input(world: &World, pos: Position) -> u8 {
    let (kind, strength) = block_signal_at(world, pos);
    if kind == BlockPower::Strong {
        strength
    } else {
        0
    }
}

/// 比較器現在**應該**輸出多少（0..=15）。
///
/// 比較模式：任一側比後方強就輸出 0，否則原樣輸出後方的強度。
/// 減法模式：輸出後方減掉兩側較強的那一個，下限是 0（`saturating_sub`，
/// 不會繞回成一個很大的正數）。
pub fn comparator_output(world: &World, pos: Position) -> u8 {
    let state = world.get(pos.x, pos.y, pos.z).clone();
    let rear = comparator_rear_input(world, pos);
    let side = comparator_side_positions(&state, pos)
        .into_iter()
        .flatten()
        .map(|side_pos| comparator_side_input(world, side_pos))
        .max()
        .unwrap_or(0);

    if comparator_is_subtract_mode(&state) {
        rear.saturating_sub(side)
    } else if side > rear {
        0
    } else {
        rear
    }
}

/// 這個比較器該用哪個優先權排程。
///
/// 面對著另一個中繼器或比較器的背面或側面 —— 高優先權，確保它先於一般
/// 元件解析，跟中繼器鏈的道理一樣。其餘情況用一般優先權。比較器沒有
/// 鎖存，不像中繼器需要區分開啟／關閉兩級。
pub fn comparator_priority(world: &World, pos: Position) -> TickPriority {
    let state = world.get(pos.x, pos.y, pos.z);
    let Some(facing) = state.facing else {
        return TickPriority::Normal;
    };

    let front = pos.offset(facing);
    let front_state = world.get(front.x, front.y, front.z);
    let faces_back_or_side_of_diode = is_diode(front_state.kind)
        && front_state
            .facing
            .is_some_and(|target_facing| target_facing != facing.opposite());

    if faces_back_or_side_of_diode {
        TickPriority::High
    } else {
        TickPriority::Normal
    }
}

/// 比較器從被迫改變到真正輸出之間的延遲：固定 2 game tick（1 redstone
/// tick），不像中繼器可以調整。
pub const COMPARATOR_DELAY_GAME_TICKS: u64 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, kind: BlockKind) -> BlockState {
        let mut state = BlockState::air();
        state.kind = kind;
        state.name = name.to_string();
        state
    }

    fn stone() -> BlockState {
        named("minecraft:stone", BlockKind::Solid)
    }

    fn torch() -> BlockState {
        named("minecraft:redstone_torch", BlockKind::Torch)
    }

    fn wall_torch(facing: Facing) -> BlockState {
        let mut state = named("minecraft:redstone_wall_torch", BlockKind::WallTorch);
        state.facing = Some(facing);
        state
    }

    fn lever() -> BlockState {
        named("minecraft:lever", BlockKind::Lever)
    }

    fn repeater(facing: Facing, delay: u8, lit: bool) -> BlockState {
        let mut state = named("minecraft:repeater", BlockKind::Repeater);
        state.facing = Some(facing);
        state.delay = delay;
        state.lit = lit;
        state
    }

    fn comparator(facing: Facing, mode: Option<&str>, power: u8, lit: bool) -> BlockState {
        let mut state = named("minecraft:comparator", BlockKind::Comparator);
        state.facing = Some(facing);
        state.power = power;
        state.lit = lit;
        if let Some(mode) = mode {
            state.extra_properties.insert("mode".to_string(), mode.to_string());
        }
        state
    }

    #[test]
    fn a_standing_torch_is_attached_to_the_block_below() {
        let pos = Position::new(3, 4, 5);
        assert_eq!(torch_support_position(&torch(), pos), Some(Position::new(3, 3, 5)));
    }

    #[test]
    fn a_wall_torch_is_attached_opposite_its_facing() {
        // facing 記錄頭朝哪 -- 朝東的火把掛在西邊的方塊上
        let pos = Position::new(5, 5, 5);
        assert_eq!(
            torch_support_position(&wall_torch(Facing::East), pos),
            Some(Position::new(4, 5, 5)),
            "a torch facing east must hang on the block to its west"
        );
    }

    #[test]
    fn a_torch_on_an_unpowered_block_wants_to_be_lit() {
        let mut world = World::new(5, 5, 5);
        world.set(0, 0, 0, stone());
        world.set(0, 1, 0, torch());

        assert!(torch_should_be_lit(&world, Position::new(0, 1, 0)));
    }

    #[test]
    fn a_torch_on_a_powered_block_wants_to_be_off() {
        // 這就是反相
        let mut world = World::new(5, 5, 5);
        world.set(0, 0, 0, stone());
        world.set(0, 1, 0, torch());

        let mut on_lever = lever();
        on_lever.lit = true;
        world.set(1, 0, 0, on_lever);

        assert!(!torch_should_be_lit(&world, Position::new(0, 1, 0)));
    }

    #[test]
    fn burnout_triggers_past_the_limit_within_the_window() {
        let changes: Vec<u64> = (1..=9).collect(); // 9 次改變，全部在視窗內
        assert!(!is_burned_out(&changes[..8], 9), "剛好 8 次不算燒毀");
        assert!(is_burned_out(&changes, 9), "第 9 次改變就超過門檻，燒毀");
    }

    #[test]
    fn burnout_recovers_once_old_changes_leave_the_window() {
        // 讓火把燒毀來換取收斂是錯的做法 -- 它必須會恢復
        let changes: Vec<u64> = (1..=9).collect();
        assert!(is_burned_out(&changes, 60), "第一筆改變（tick 1）還在視窗內");
        assert!(
            !is_burned_out(&changes, 61),
            "第一筆改變滑出視窗後只剩 8 次，燒毀應該解除"
        );
    }

    #[test]
    fn a_repeater_takes_its_input_from_directly_behind() {
        // 朝東的中繼器，輸入在它西邊
        let pos = Position::new(5, 5, 5);
        assert_eq!(
            repeater_input_position(&repeater(Facing::East, 1, false), pos),
            Some(Position::new(4, 5, 5)),
            "an east-facing repeater must read its input from the block to its west"
        );
    }

    #[test]
    fn a_repeater_delay_is_two_game_ticks_per_redstone_tick() {
        assert_eq!(
            repeater_delay_game_ticks(&repeater(Facing::East, 1, false)),
            2,
            "delay=1 redstone tick = 2 game ticks"
        );
        assert_eq!(
            repeater_delay_game_ticks(&repeater(Facing::East, 4, false)),
            8,
            "delay=4 redstone ticks = 8 game ticks"
        );
    }

    #[test]
    fn a_zero_delay_is_treated_as_one() {
        // 檔案裡可能有任何東西
        assert_eq!(repeater_delay_game_ticks(&repeater(Facing::East, 0, false)), 2);
    }

    #[test]
    fn a_powered_repeater_facing_a_diode_gets_the_highest_priority() {
        let mut world = World::new(5, 5, 5);
        let pos = Position::new(2, 0, 2);
        world.set(pos.x, pos.y, pos.z, repeater(Facing::East, 1, true));
        // 前方是另一個朝同方向的中繼器 -- 正對著它的背面（正常的中繼器鏈）
        world.set(3, 0, 2, repeater(Facing::East, 1, false));

        assert_eq!(
            repeater_priority(&world, pos, false),
            TickPriority::Highest,
            "feeding into another diode's back must use the highest priority"
        );
        assert_eq!(
            repeater_priority(&world, pos, true),
            TickPriority::Highest,
            "facing a diode outranks the turning-off rule too"
        );
    }

    #[test]
    fn a_repeater_turning_off_gets_higher_priority_than_one_turning_on() {
        let mut world = World::new(5, 5, 5);
        let pos = Position::new(2, 0, 2);
        world.set(pos.x, pos.y, pos.z, repeater(Facing::East, 1, true));
        // 前方只是石頭，不是二極體，所以不會落入 Highest 那一支
        world.set(3, 0, 2, stone());

        assert_eq!(repeater_priority(&world, pos, true), TickPriority::Higher);
        assert_eq!(repeater_priority(&world, pos, false), TickPriority::High);
        assert!(
            TickPriority::Higher < TickPriority::High,
            "Higher must run before High within the same game tick"
        );
    }

    #[test]
    fn a_powered_repeater_at_the_side_locks_this_one() {
        let mut world = World::new(5, 5, 5);
        let pos = Position::new(2, 0, 2);
        world.set(pos.x, pos.y, pos.z, repeater(Facing::East, 1, false));
        // 北側 (z-1) 放一個已充能、朝南（正對著它）的中繼器
        world.set(2, 0, 1, repeater(Facing::South, 1, true));

        assert!(
            repeater_is_locked(&world, pos),
            "a powered diode facing into the side must lock this repeater"
        );
    }

    #[test]
    fn an_unpowered_repeater_at_the_side_does_not_lock() {
        let mut world = World::new(5, 5, 5);
        let pos = Position::new(2, 0, 2);
        world.set(pos.x, pos.y, pos.z, repeater(Facing::East, 1, false));
        world.set(2, 0, 1, repeater(Facing::South, 1, false));

        assert!(
            !repeater_is_locked(&world, pos),
            "an unpowered diode at the side must not lock anything"
        );
    }

    #[test]
    fn a_comparator_reads_its_main_signal_from_behind() {
        // 朝東的比較器，後方（主訊號）在它西邊
        let pos = Position::new(5, 5, 5);
        assert_eq!(
            comparator_rear_position(&comparator(Facing::East, None, 0, false), pos),
            Some(Position::new(4, 5, 5)),
            "an east-facing comparator must read its main signal from the block to its west"
        );
    }

    #[test]
    fn mode_defaults_to_compare_when_the_property_is_absent() {
        assert!(
            !comparator_is_subtract_mode(&comparator(Facing::East, None, 0, false)),
            "no mode property must mean compare mode, not subtract"
        );
    }

    #[test]
    fn subtract_mode_is_read_from_the_mode_property() {
        assert!(comparator_is_subtract_mode(&comparator(
            Facing::East,
            Some("subtract"),
            0,
            false
        )));
        assert!(
            !comparator_is_subtract_mode(&comparator(Facing::East, Some("compare"), 0, false)),
            "an explicit compare property must not be misread as subtract"
        );
    }

    #[test]
    fn compare_mode_passes_the_rear_signal_when_no_side_is_stronger() {
        let mut world = World::new(5, 5, 5);
        let pos = Position::new(2, 0, 2);
        world.set(pos.x, pos.y, pos.z, comparator(Facing::East, None, 0, false));

        // 後方是另一個朝同方向的比較器，直接送出類比值 9（不是固定的 15，
        // 才能抓到「硬編成開關」的錯誤實作）
        world.set(1, 0, 2, comparator(Facing::East, None, 9, true));

        // 兩側都是沒有被充能的石頭
        world.set(2, 0, 1, stone());
        world.set(2, 0, 3, stone());

        assert_eq!(
            comparator_output(&world, pos),
            9,
            "compare mode must pass the rear signal through unchanged"
        );
    }

    #[test]
    fn compare_mode_outputs_zero_when_a_side_is_stronger() {
        let mut world = World::new(5, 5, 5);
        let pos = Position::new(2, 0, 2);
        world.set(pos.x, pos.y, pos.z, comparator(Facing::East, None, 0, false));
        world.set(1, 0, 2, comparator(Facing::East, None, 9, true));

        // 北側石頭被另一個比較器強充能到 12，比後方的 9 強
        world.set(2, 0, 1, stone());
        world.set(2, 0, 0, comparator(Facing::South, None, 12, true));
        world.set(2, 0, 3, stone());

        assert_eq!(
            comparator_output(&world, pos),
            0,
            "a side stronger than the rear must zero the output in compare mode"
        );
    }

    #[test]
    fn subtract_mode_subtracts_the_strongest_side() {
        // rear 12, side 5 -> 7
        let mut world = World::new(5, 5, 5);
        let pos = Position::new(2, 0, 2);
        world.set(pos.x, pos.y, pos.z, comparator(Facing::East, Some("subtract"), 0, false));
        world.set(1, 0, 2, comparator(Facing::East, None, 12, true));

        world.set(2, 0, 1, stone());
        world.set(2, 0, 0, comparator(Facing::South, None, 5, true));
        world.set(2, 0, 3, stone());

        assert_eq!(comparator_output(&world, pos), 7);
    }

    #[test]
    fn subtract_mode_clamps_at_zero() {
        // rear 3, side 9 -> 0，不是負數繞回
        let mut world = World::new(5, 5, 5);
        let pos = Position::new(2, 0, 2);
        world.set(pos.x, pos.y, pos.z, comparator(Facing::East, Some("subtract"), 0, false));
        world.set(1, 0, 2, comparator(Facing::East, None, 3, true));

        world.set(2, 0, 1, stone());
        world.set(2, 0, 0, comparator(Facing::South, None, 9, true));
        world.set(2, 0, 3, stone());

        assert_eq!(comparator_output(&world, pos), 0);
    }

    #[test]
    fn a_weakly_powered_side_does_not_feed_the_comparator() {
        // 這條錯了會產生「模擬會動、遊戲裡不會動」的電路
        let mut world = World::new(5, 5, 5);
        let side_pos = Position::new(2, 0, 1);
        world.set(side_pos.x, side_pos.y, side_pos.z, stone());

        // 粉墊在側面石頭上方 -- 只弱充能它
        let mut dust = named("minecraft:redstone_wire", BlockKind::RedstoneWire);
        dust.power = 15;
        world.set(2, 1, 1, dust);

        assert_eq!(
            comparator_side_input(&world, side_pos),
            0,
            "a weakly powered block at the side must not feed the comparator"
        );
    }
}

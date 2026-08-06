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

use crate::redstone::rules::taxonomy::BlockPower;
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::block_power_at;
use crate::redstone::world::block::{BlockKind, BlockState};
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::Facing;

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
}

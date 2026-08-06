//! 紅石模擬器。
//!
//! 逐 game tick 推進，訊號傳播依功率流方向做 BFS（非 locational）。
//!
//! **本階段不支援** quasi-connectivity、活塞、觀察者。碰到這些元件會明確
//! 報錯，不會靜默忽略。

pub mod component;
pub mod connectivity;
pub mod position;
pub mod propagate;
pub mod schedule;

use std::collections::HashMap;

use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

use position::Position;
use schedule::{TickPriority, TickQueue};

/// 紅石模擬器。
///
/// 逐 game tick 推進。1 redstone tick = 2 game ticks。
pub struct Simulator {
    world: World,
    queue: schedule::TickQueue,
    /// 每個火把最近的狀態改變時間，用來判斷 burnout
    torch_changes: std::collections::HashMap<position::Position, Vec<u64>>,
    /// 這次模擬總共處理了幾筆排程，用來偵測發散
    work_done: u64,
}

/// 模擬過程的錯誤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulationError {
    /// 電路在上限內沒有穩定 —— 通常是回授迴路在振盪
    Diverged { game_ticks: u64, pending: usize },
    /// 碰到本階段不支援的元件
    UnsupportedComponent { position: Position, name: String },
}

/// 這個 kind 是不是本階段明確不支援、必須回報而非靜默忽略的元件。
///
/// 這份清單刻意不含中繼器、比較器 —— 它們的功率規則已經由 `taxonomy`
/// 完整處理，只是本階段的排程迴圈還不會主動觸發它們的延遲行為。
fn is_unsupported_kind(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Piston
            | BlockKind::Observer
            | BlockKind::Button
            | BlockKind::PressurePlate
            | BlockKind::WeightedPressurePlate
            | BlockKind::Target
            | BlockKind::DaylightDetector
    )
}

/// 把扁平索引（YZX 順序，見 `World`）還原成座標。
fn decode_flat_index(flat: usize, size_x: i32, size_z: i32) -> Position {
    let layer = (size_x as usize) * (size_z as usize);
    let y = (flat / layer) as i32;
    let rem = flat % layer;
    let z = (rem / size_x as usize) as i32;
    let x = (rem % size_x as usize) as i32;
    Position::new(x, y, z)
}

/// 世界裡第一個不支援的元件，連同它的原始方塊 ID。
///
/// 只掃實際被放置的格子（透過 `cells()`），不是整個 palette —— palette
/// 裡可能留著已經沒有任何格子在用的舊項目。
fn find_unsupported_component(world: &World) -> Option<(Position, String)> {
    let (size_x, _size_y, size_z) = world.size();
    let unsupported_by_index: Vec<bool> = world
        .palette()
        .entries()
        .iter()
        .map(|state| is_unsupported_kind(state.kind))
        .collect();

    for (flat, &palette_index) in world.cells().iter().enumerate() {
        if !unsupported_by_index[palette_index as usize] {
            continue;
        }
        let name = world
            .palette()
            .get(palette_index)
            .expect("palette index referenced by a cell must exist")
            .name
            .clone();
        return Some((decode_flat_index(flat, size_x, size_z), name));
    }
    None
}

/// 世界裡所有火把（立式與牆上）的位置。
fn torch_positions(world: &World) -> Vec<Position> {
    let (size_x, _size_y, size_z) = world.size();
    let is_torch_by_index: Vec<bool> = world
        .palette()
        .entries()
        .iter()
        .map(|state| matches!(state.kind, BlockKind::Torch | BlockKind::WallTorch))
        .collect();

    world
        .cells()
        .iter()
        .enumerate()
        .filter(|&(_, &palette_index)| is_torch_by_index[palette_index as usize])
        .map(|(flat, _)| decode_flat_index(flat, size_x, size_z))
        .collect()
}

impl Simulator {
    /// 從一個世界建立模擬器，並讓它先穩定下來。
    ///
    /// 「先穩定下來」指的是重算一次紅石粉強度，讓它反映世界載入當下各
    /// 元件（假定已經自洽）的狀態 —— 不會追溯性地改動任何火把的 `lit`；
    /// 那是 `lit` 本來就該和它現在的支撐塊一致的假設，跟真正的遊戲讀檔
    /// 一樣。
    ///
    /// 如果世界裡有本階段不支援的元件，這裡只會印出警告 —— 真正擋下模擬
    /// 的是 `run_until_stable`，因為 `new` 沒有 `Result` 可以回報錯誤。
    pub fn new(world: World) -> Self {
        let mut simulator = Simulator {
            world,
            queue: TickQueue::new(),
            torch_changes: HashMap::new(),
            work_done: 0,
        };

        if let Some((position, name)) = find_unsupported_component(&simulator.world) {
            eprintln!(
                "reda: 世界在 {position:?} 有本階段不支援的元件 `{name}` —— \
                 不會被模擬，呼叫 run_until_stable 時會回報 UnsupportedComponent"
            );
        }

        propagate::recompute_dust_strengths(&mut simulator.world);
        simulator
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    pub fn current_tick(&self) -> u64 {
        self.queue.current_tick()
    }

    /// 推進一個 game tick。回傳這一刻有多少格的狀態改變了。
    pub fn step(&mut self) -> usize {
        self.schedule_mismatched_torches();
        self.advance_one_tick()
    }

    /// 一直跑到沒有待處理的排程，或達到 `max_game_ticks`。
    ///
    /// 回傳 `Err(SimulationError::Diverged)` 表示電路在上限內沒有穩定 ——
    /// **振盪是一種要回報的結果，不是要消除的現象**。呼叫端據此判定電路
    /// 有問題。
    pub fn run_until_stable(&mut self, max_game_ticks: u64) -> Result<u64, SimulationError> {
        if let Some((position, name)) = find_unsupported_component(&self.world) {
            return Err(SimulationError::UnsupportedComponent { position, name });
        }

        let mut game_ticks_run = 0u64;
        loop {
            // 每一輪都先找出目前跟「應該是什麼狀態」不一致的火把並排程 ——
            // 這樣才抓得到透過 `world_mut()` 做的外部修改，也才不會被
            // burnout 期間「凍結」的火把騙成「已經清空佇列」。
            self.schedule_mismatched_torches();

            if self.queue.is_empty() {
                return Ok(game_ticks_run);
            }

            if game_ticks_run >= max_game_ticks {
                eprintln!(
                    "reda: 電路在 {game_ticks_run} 個 game tick 內沒有穩定 \
                     （共處理 {} 筆排程，仍有 {} 筆待處理）—— 回報 Diverged",
                    self.work_done,
                    self.queue.pending_count()
                );
                return Err(SimulationError::Diverged {
                    game_ticks: game_ticks_run,
                    pending: self.queue.pending_count(),
                });
            }

            self.advance_one_tick();
            game_ticks_run += 1;
        }
    }

    /// 找出目前狀態與「應該是什麼狀態」不一致的火把，把它們排入佇列。
    ///
    /// 這一步同時處理三種來源的不一致：其他元件這一刻造成的漣漪效應、
    /// 透過 `world_mut()` 直接做的外部修改，以及正在 burnout 中、狀態被
    /// 強制凍結的火把 —— 只要它的輸入還跟目前狀態不同，這裡就會不斷把它
    /// 排回佇列，直到 burnout 解除為止。這正是振盪電路不會被 burnout
    /// 假裝收斂掉的原因：佇列不會真的清空。
    fn schedule_mismatched_torches(&mut self) {
        for position in torch_positions(&self.world) {
            let currently_lit = self.world.get(position.x, position.y, position.z).lit;
            if currently_lit != component::torch_should_be_lit(&self.world, position) {
                self.queue.schedule(
                    position,
                    component::TORCH_DELAY_GAME_TICKS,
                    TickPriority::Normal,
                );
            }
        }
    }

    /// 推進佇列一個 game tick：套用到期的排程，然後重算紅石粉強度。
    ///
    /// 不含「找出新的不一致並排程」—— 那是 `schedule_mismatched_torches`
    /// 的責任，由呼叫端（`step` 與 `run_until_stable`）各自決定何時呼叫。
    fn advance_one_tick(&mut self) -> usize {
        let due = self.queue.advance();
        let now = self.queue.current_tick();
        let mut changed = 0usize;

        for tick in &due {
            self.work_done += 1;
            if self.apply_torch_tick(tick.position, now) {
                changed += 1;
            }
        }

        changed += propagate::recompute_dust_strengths(&mut self.world).len();
        changed
    }

    /// 套用一個到期的火把排程：翻轉狀態，除非它燒毀了。
    ///
    /// 燒毀時強制熄滅，且**不**把這次強制熄滅記錄成一次改變 —— 那不是
    /// 輸入造成的真實翻轉，計入的話燒毀視窗就永遠滑不出去，變成永久失效。
    fn apply_torch_tick(&mut self, position: Position, now: u64) -> bool {
        let state = self.world.get(position.x, position.y, position.z).clone();
        if !matches!(state.kind, BlockKind::Torch | BlockKind::WallTorch) {
            return false;
        }

        let desired = component::torch_should_be_lit(&self.world, position);
        if desired == state.lit {
            return false;
        }

        let burned_out = self
            .torch_changes
            .get(&position)
            .is_some_and(|changes| component::is_burned_out(changes, now));

        if burned_out {
            if !state.lit {
                return false;
            }
            let mut updated = state;
            updated.lit = false;
            self.world.set(position.x, position.y, position.z, updated);
            return true;
        }

        let mut updated = state;
        updated.lit = desired;
        self.world.set(position.x, position.y, position.z, updated);
        self.torch_changes.entry(position).or_default().push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState, Facing};

    fn named(name: &str, kind: BlockKind) -> BlockState {
        let mut state = BlockState::air();
        state.kind = kind;
        state.name = name.to_string();
        state
    }

    fn stone() -> BlockState {
        named("minecraft:stone", BlockKind::Solid)
    }

    fn dust() -> BlockState {
        named("minecraft:redstone_wire", BlockKind::RedstoneWire)
    }

    fn lever() -> BlockState {
        named("minecraft:lever", BlockKind::Lever)
    }

    fn torch() -> BlockState {
        named("minecraft:redstone_torch", BlockKind::Torch)
    }

    fn wall_torch(facing: Facing) -> BlockState {
        let mut state = named("minecraft:redstone_wall_torch", BlockKind::WallTorch);
        state.facing = Some(facing);
        state
    }

    #[test]
    fn a_torch_inverts_its_input_after_one_redstone_tick() {
        // 拉桿 -> 方塊 -> 火把。拉桿開之前火把亮；開了之後 2 game tick 內熄滅。
        let mut world = World::new(5, 5, 5);
        world.set(0, 0, 0, stone());
        world.set(1, 0, 0, lever()); // 拉桿一開始是關的 (lit=false)
        let mut lit_torch = torch();
        lit_torch.lit = true; // 支撐塊目前沒充能，這是自洽的初始狀態
        world.set(0, 1, 0, lit_torch);

        let mut simulator = Simulator::new(world);
        assert!(
            simulator.world().get(0, 1, 0).lit,
            "拉桿開之前火把應該是亮的"
        );

        let mut on_lever = lever();
        on_lever.lit = true;
        simulator.world_mut().set(1, 0, 0, on_lever);

        simulator.step();
        assert!(
            simulator.world().get(0, 1, 0).lit,
            "第 1 個 game tick 火把還不該變 -- 延遲是 2 個 game tick"
        );

        simulator.step();
        assert!(
            !simulator.world().get(0, 1, 0).lit,
            "第 2 個 game tick 火把應該已經熄滅"
        );
    }

    #[test]
    fn a_lit_torch_lights_adjacent_dust() {
        let mut world = World::new(5, 5, 5);
        world.set(0, 0, 0, stone());
        let mut lit_torch = torch();
        lit_torch.lit = true;
        world.set(0, 1, 0, lit_torch);
        world.set(1, 0, 0, stone());
        world.set(1, 1, 0, dust());

        let simulator = Simulator::new(world);

        assert_eq!(
            simulator.world().get(1, 1, 0).power,
            15,
            "new() 必須先讓世界穩定，火把的訊號在建構當下就該傳到相鄰的粉"
        );
    }

    #[test]
    fn an_unsupported_component_is_reported_not_ignored() {
        // 世界裡有活塞 -> run_until_stable 回 UnsupportedComponent，
        // 而不是把它當空氣
        let mut world = World::new(5, 5, 5);
        world.set(0, 0, 0, stone());
        world.set(1, 0, 0, named("minecraft:piston", BlockKind::Piston));

        let mut simulator = Simulator::new(world);
        let result = simulator.run_until_stable(50);

        assert_eq!(
            result,
            Err(SimulationError::UnsupportedComponent {
                position: Position::new(1, 0, 0),
                name: "minecraft:piston".to_string(),
            })
        );
    }

    #[test]
    fn a_stable_circuit_settles_and_reports_its_tick_count() {
        let mut world = World::new(5, 5, 5);
        world.set(0, 0, 0, stone());
        world.set(1, 0, 0, lever());
        let mut lit_torch = torch();
        lit_torch.lit = true;
        world.set(0, 1, 0, lit_torch);

        let mut simulator = Simulator::new(world);

        let mut on_lever = lever();
        on_lever.lit = true;
        simulator.world_mut().set(1, 0, 0, on_lever);

        let settled_after = simulator
            .run_until_stable(50)
            .expect("a single inverter driven by a lever must settle");

        assert_eq!(settled_after, 2, "火把的延遲是 2 個 game tick");
        assert!(!simulator.world().get(0, 1, 0).lit, "火把應該已經熄滅");
        assert_eq!(
            simulator.run_until_stable(50),
            Ok(0),
            "已經穩定的電路再跑一次不該有任何新工作"
        );
    }

    #[test]
    fn an_oscillator_is_reported_as_diverged() {
        // 火把接到自己的輸出 -- 必須回 Diverged，不能靠讓火把燒毀來假裝收斂。
        //
        // 三個牆上火把接成環：每個火把附著在一個支撐塊上，同時緊鄰著下一個
        // 支撐塊、對它輸出訊號。三個反相器串成一個環，數學上不存在一組
        // 亮/暗組合能同時滿足全部三條「跟前一個相反」的關係（NOT^3 沒有
        // 不動點），所以它永遠振盪 —— 就算某個火把中途燒毀、暫時卡在暗，
        // 只要它的輸入還跟現狀不同，`schedule_mismatched_torches` 就會不斷
        // 把它排回佇列，佇列就不會真的清空。
        //
        // 三個支撐塊放在單位立方體裡兩兩成面對角線（距離 sqrt(2)）的角落，
        // 火把放在它們之間唯一的共用鄰格上：這樣每個火把恰好只碰得到
        // 「它附著的支撐塊」跟「它要驅動的下一個支撐塊」，不會像排成一列
        // 那樣意外疊到其他格子上頭去多送一條訊號路徑出去。
        let mut world = World::new(5, 5, 5);
        let support_a = Position::new(0, 0, 0);
        let support_b = Position::new(1, 1, 0);
        let support_c = Position::new(0, 1, 1);

        world.set(support_a.x, support_a.y, support_a.z, stone());
        world.set(support_b.x, support_b.y, support_b.z, stone());
        world.set(support_c.x, support_c.y, support_c.z, stone());

        // torch_a：位於 (1,0,0)，附著在西邊的 support_a（頭朝東），
        // 往上緊鄰 support_b、對它輸出。
        let mut torch_a = wall_torch(Facing::East);
        torch_a.lit = true;
        world.set(1, 0, 0, torch_a);

        // torch_b：位於 (1,1,1)，附著在北邊的 support_b（頭朝南），
        // 往西緊鄰 support_c、對它輸出。
        let mut torch_b = wall_torch(Facing::South);
        torch_b.lit = false;
        world.set(1, 1, 1, torch_b);

        // torch_c：位於 (0,0,1)，附著在上方的 support_c（頭朝下），
        // 往北緊鄰 support_a、對它輸出，閉合這個環。
        let mut torch_c = wall_torch(Facing::Down);
        torch_c.lit = true;
        world.set(0, 0, 1, torch_c);

        let mut simulator = Simulator::new(world);
        let result = simulator.run_until_stable(50);

        assert!(
            matches!(result, Err(SimulationError::Diverged { .. })),
            "a self-feeding ring of torches must never report Ok, got {result:?}"
        );
    }
}

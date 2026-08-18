//! 紅石模擬器。
//!
//! 逐 game tick 推進，訊號傳播依功率流方向做 BFS（非 locational）。
//!
//! **本階段不支援** quasi-connectivity、活塞、觀察者。碰到這些元件會明確
//! 報錯，不會靜默忽略。

pub mod component;
pub mod connectivity;
pub mod differential;
pub mod observer;
pub mod position;
pub mod propagate;
pub mod schedule;

use std::collections::HashMap;

use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

use observer::{Observation, Observer};
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
    /// Optional dynamic-timing-analysis observer. `None` unless
    /// `attach_observer` was called -- and when it is `None`, `step` and
    /// `run_until_stable` take exactly the path they always have (see
    /// `advance_one_tick`, the only place this is consulted).
    observer: Option<Observer>,
}

/// 模擬過程的錯誤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulationError {
    /// 電路在上限內沒有穩定 —— 通常是回授迴路在振盪
    Diverged { game_ticks: u64, pending: usize },
    /// 碰到本階段不支援的元件
    UnsupportedComponent { position: Position, name: String },
}

/// 本階段明確不支援、必須回報而非靜默忽略的元件種類，`find_unsupported_component`
/// 掃描用。
///
/// 這份清單刻意不含中繼器、比較器 —— 它們的功率規則已經由 `taxonomy`
/// 完整處理；中繼器與比較器的延遲、鎖存（中繼器獨有）與排程優先權都已經
/// 接上 `step`。
const UNSUPPORTED_KINDS: [BlockKind; 7] = [
    BlockKind::Piston,
    BlockKind::Observer,
    BlockKind::Button,
    BlockKind::PressurePlate,
    BlockKind::WeightedPressurePlate,
    BlockKind::Target,
    BlockKind::DaylightDetector,
];

/// 世界裡第一個不支援的元件，連同它的原始方塊 ID。
///
/// 用 `World::positions_of`（`World::set` 增量維護的稀疏索引）取代掃過
/// 整個 `cells()`——不支援的元件種類就那幾種，成本只跟「擺了幾個」成
/// 正比。取扁平索引最小的那個，跟舊版線性掃描「回傳第一個碰到的」是
/// 同一個順序（`positions_of` 依扁平索引遞增，也就是 YZX 掃描順序）。
fn find_unsupported_component(world: &World) -> Option<(Position, String)> {
    let flat = UNSUPPORTED_KINDS
        .iter()
        .flat_map(|&kind| world.positions_of(kind))
        .min()?;
    let (x, y, z) = world.decode(flat);
    let name = world.get(x, y, z).name.clone();
    Some((Position::new(x, y, z), name))
}

/// 世界裡所有火把（立式與牆上）的位置。
fn torch_positions(world: &World) -> Vec<Position> {
    world
        .positions_of(BlockKind::Torch)
        .chain(world.positions_of(BlockKind::WallTorch))
        .map(|flat| {
            let (x, y, z) = world.decode(flat);
            Position::new(x, y, z)
        })
        .collect()
}

/// 世界裡所有中繼器的位置。
fn repeater_positions(world: &World) -> Vec<Position> {
    world
        .positions_of(BlockKind::Repeater)
        .map(|flat| {
            let (x, y, z) = world.decode(flat);
            Position::new(x, y, z)
        })
        .collect()
}

/// 世界裡所有比較器的位置。
fn comparator_positions(world: &World) -> Vec<Position> {
    world
        .positions_of(BlockKind::Comparator)
        .map(|flat| {
            let (x, y, z) = world.decode(flat);
            Position::new(x, y, z)
        })
        .collect()
}

/// Every redstone lamp's position in the world.
fn lamp_positions(world: &World) -> Vec<Position> {
    world
        .positions_of(BlockKind::Lamp)
        .map(|flat| {
            let (x, y, z) = world.decode(flat);
            Position::new(x, y, z)
        })
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
            observer: None,
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

    /// Attach a dynamic-timing-analysis observer watching exactly these
    /// positions, each carrying a human-readable label (typically a netlist
    /// signal name). Replaces any previously attached observer.
    ///
    /// The observer is baselined against the world's current state
    /// immediately, so nothing already true when it is attached shows up as
    /// an observation -- only changes from this point on do.
    pub fn attach_observer(&mut self, watched: impl IntoIterator<Item = (Position, String)>) {
        let mut observer = Observer::new(watched);
        observer.reset(&self.world);
        self.observer = Some(observer);
    }

    /// Re-baseline the attached observer (if any) against the world's
    /// current state and clear its log.
    ///
    /// Call this right before the input change you want to measure, so the
    /// observations that follow hold only the changes caused by that
    /// change. A no-op if no observer is attached.
    pub fn reset_observer(&mut self) {
        if let Some(observer) = self.observer.as_mut() {
            observer.reset(&self.world);
        }
    }

    /// The attached observer's log since its last reset, or an empty slice
    /// if no observer is attached.
    pub fn observations(&self) -> &[Observation] {
        self.observer.as_ref().map(Observer::log).unwrap_or(&[])
    }

    /// 推進一個 game tick。回傳這一刻有多少格的狀態改變了。
    pub fn step(&mut self) -> usize {
        self.settle_from_current_state();
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
            // 每一輪都先從目前的世界狀態重新安定下來：這代表「從呼叫端
            // 現在讓世界處於的狀態安定下來」，而不是「處理佇列裡剛好排到
            // 的東西」。少了紅石粉重算這一步，透過 `world_mut()` 做的外部
            // 修改（例如翻轉拉桿）在電路裡沒有任何主動元件直接偵測得到
            // 時 —— 純紅石粉線路就是這樣 —— 佇列會一直是空的，這裡就會
            // 對著過期的粉強度回報「已經穩定」。
            self.settle_from_current_state();

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

    /// 從目前的世界狀態安定下來：先重算紅石粉強度，再排程任何因此變得
    /// 跟輸入不一致的元件。
    ///
    /// 這是 `step` 與 `run_until_stable` 共用的一段 —— 兩者的差別只在於
    /// 拿到這個結果之後要不要真的推進一個 game tick。**無條件重算**紅石
    /// 粉強度是關鍵：呼叫端可能透過 `world_mut()` 直接改了拉桿之類的
    /// 輸入，而電路裡不一定有火把、中繼器或比較器直接碰得到那個輸入 ——
    /// 純紅石粉線路正是這樣。少了這一步，這些外部修改就只能等佇列裡剛好
    /// 有別的排程「順便」觸發重算才會被看見；佇列若一直是空的，粉的
    /// 強度就會一直停留在過期的值，而呼叫端永遠不會被告知。
    fn settle_from_current_state(&mut self) {
        propagate::recompute_dust_strengths(&mut self.world);
        self.schedule_mismatched_torches();
        self.schedule_mismatched_repeaters();
        self.schedule_mismatched_comparators();
        self.schedule_mismatched_lamps();
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
            if self.apply_scheduled_tick(tick.position, now) {
                changed += 1;
            }
        }

        changed += propagate::recompute_dust_strengths(&mut self.world).len();

        if let Some(observer) = self.observer.as_mut() {
            observer.sample(&self.world, now);
        }

        changed
    }

    /// 套用一筆到期的排程，依方塊種類分派給對應的元件邏輯。
    fn apply_scheduled_tick(&mut self, position: Position, now: u64) -> bool {
        match self.world.get(position.x, position.y, position.z).kind {
            BlockKind::Torch | BlockKind::WallTorch => self.apply_torch_tick(position, now),
            BlockKind::Repeater => self.apply_repeater_tick(position),
            BlockKind::Comparator => self.apply_comparator_tick(position),
            BlockKind::Lamp => self.apply_lamp_tick(position),
            _ => false,
        }
    }

    /// Find every lamp whose `lit` state disagrees with what its current
    /// power says it should be, and schedule it.
    ///
    /// Same shape as `schedule_mismatched_torches`: this catches both ripple
    /// effects from other components settling this tick, and external edits
    /// made through `world_mut()`. A lamp has no latching and no burnout, so
    /// unlike repeaters there is nothing else to check before scheduling.
    ///
    /// The delay is not symmetric: turning on is immediate
    /// (`LAMP_TURN_ON_DELAY_GAME_TICKS` = 0) but turning off is delayed by one
    /// redstone tick (`LAMP_TURN_OFF_DELAY_GAME_TICKS` = 4), matching real
    /// Minecraft -- see both constants' doc comments in `component.rs`.
    fn schedule_mismatched_lamps(&mut self) {
        for position in lamp_positions(&self.world) {
            let currently_lit = self.world.get(position.x, position.y, position.z).lit;
            let desired_lit = component::lamp_should_be_lit(&self.world, position);
            if currently_lit != desired_lit {
                let delay = if desired_lit {
                    component::LAMP_TURN_ON_DELAY_GAME_TICKS
                } else {
                    component::LAMP_TURN_OFF_DELAY_GAME_TICKS
                };
                self.queue.schedule(position, delay, TickPriority::Normal);
            }
        }
    }

    /// Apply one due lamp tick: flip `lit` to match its current power.
    fn apply_lamp_tick(&mut self, position: Position) -> bool {
        let state = self.world.get(position.x, position.y, position.z).clone();
        if state.kind != BlockKind::Lamp {
            return false;
        }

        let desired = component::lamp_should_be_lit(&self.world, position);
        if desired == state.lit {
            return false;
        }

        let mut updated = state;
        updated.lit = desired;
        self.world.set(position.x, position.y, position.z, updated);
        true
    }

    /// 找出輸出跟輸入不一致、既沒被鎖住也還沒排程的中繼器，依規則排入佇列。
    ///
    /// 跟火把一樣同時處理外部修改與漣漪效應；跟火把不一樣的地方是鎖存 ——
    /// 被鎖住的中繼器就算輸入不一致也絕對不排程，因為鎖存本身是立即生效、
    /// 不經過排程的。
    fn schedule_mismatched_repeaters(&mut self) {
        for position in repeater_positions(&self.world) {
            if self.queue.is_scheduled(position) {
                continue;
            }
            if component::repeater_is_locked(&self.world, position) {
                continue;
            }

            let state = self.world.get(position.x, position.y, position.z);
            let currently_lit = state.lit;
            let desired = component::repeater_input_is_powered(&self.world, position);
            if currently_lit == desired {
                continue;
            }

            let turning_off = !desired;
            let delay = component::repeater_delay_game_ticks(state);
            let priority = component::repeater_priority(&self.world, position, turning_off);
            self.queue.schedule(position, delay, priority);
        }
    }

    /// 套用一個到期的中繼器排程。
    ///
    /// 開啟一律無條件套用 —— 這保證任何開脈衝至少會被拉長到一個完整延遲
    /// 的長度；若那時輸入其實已經又斷電了，下一次
    /// `schedule_mismatched_repeaters` 掃描會自然重新排一次關閉，這裡不用
    /// 手動處理。關閉則在套用前重新檢查輸入 —— 輸入這時已經恢復供電的話
    /// 就直接吞掉這次改變、維持現狀，這正是短於延遲的關脈衝會被吞掉的
    /// 原因。兩條路徑都不是「特例邏輯」，只是老實問一次「這個方向的改變
    /// 現在還成立嗎」。
    fn apply_repeater_tick(&mut self, position: Position) -> bool {
        let state = self.world.get(position.x, position.y, position.z).clone();
        if state.kind != BlockKind::Repeater {
            return false;
        }
        if component::repeater_is_locked(&self.world, position) {
            return false;
        }
        if state.lit && component::repeater_input_is_powered(&self.world, position) {
            return false;
        }

        let mut updated = state;
        updated.lit = !updated.lit;
        self.world.set(position.x, position.y, position.z, updated);
        true
    }

    /// 找出輸出跟「應該輸出多少」不一致、還沒排程的比較器，排入佇列。
    ///
    /// 跟中繼器一樣同時處理外部修改與漣漪效應；比較器沒有鎖存，所以少了
    /// 中繼器那個「被鎖住就跳過」的檢查。
    fn schedule_mismatched_comparators(&mut self) {
        for position in comparator_positions(&self.world) {
            if self.queue.is_scheduled(position) {
                continue;
            }

            let state = self.world.get(position.x, position.y, position.z);
            let desired = component::comparator_output(&self.world, position);
            if state.power == desired {
                continue;
            }

            let priority = component::comparator_priority(&self.world, position);
            self.queue
                .schedule(position, component::COMPARATOR_DELAY_GAME_TICKS, priority);
        }
    }

    /// 套用一個到期的比較器排程：在套用當下重新計算應該輸出多少，寫回
    /// `power` 與跟它一致的 `lit`（`power == 0` 就必須 `lit == false`）。
    ///
    /// 在套用當下（而不是排程當下）重新計算，理由跟中繼器的關閉路徑一樣：
    /// 世界可能在排程期間又變了，這裡要老實反映「現在」該是什麼值。
    fn apply_comparator_tick(&mut self, position: Position) -> bool {
        let state = self.world.get(position.x, position.y, position.z).clone();
        if state.kind != BlockKind::Comparator {
            return false;
        }

        let desired = component::comparator_output(&self.world, position);
        if desired == state.power {
            return false;
        }

        let mut updated = state;
        updated.power = desired;
        updated.lit = desired > 0;
        self.world.set(position.x, position.y, position.z, updated);
        true
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

    fn lamp() -> BlockState {
        named("minecraft:redstone_lamp", BlockKind::Lamp)
    }

    fn repeater(facing: Facing, delay: u8, lit: bool) -> BlockState {
        let mut state = named("minecraft:repeater", BlockKind::Repeater);
        state.facing = Some(facing);
        state.delay = delay;
        state.lit = lit;
        state
    }

    fn comparator(facing: Facing, power: u8, lit: bool) -> BlockState {
        let mut state = named("minecraft:comparator", BlockKind::Comparator);
        state.facing = Some(facing);
        state.power = power;
        state.lit = lit;
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
    fn a_lamp_above_a_torch_mirrors_the_torch_end_to_end() {
        // This is `compile()`'s output stage in miniature: lever -> support
        // block -> torch (inverter) -> lamp sitting directly on top of the
        // torch. The lamp must track the torch, which itself inverts the
        // lever -- so the lamp ends up lit exactly when the lever is off.
        let mut world = World::new(5, 5, 5);
        world.set(0, 0, 0, stone()); // the torch's support block
        world.set(1, 0, 0, lever()); // starts off (lit = false)
        let mut lit_torch = torch();
        lit_torch.lit = true; // self-consistent: support is unpowered while the lever is off
        world.set(0, 1, 0, lit_torch);
        world.set(0, 2, 0, lamp()); // starts dark; the first settle corrects it

        let mut simulator = Simulator::new(world);
        simulator
            .run_until_stable(50)
            .expect("a lever -> torch -> lamp chain must settle");
        assert!(
            simulator.world().get(0, 1, 0).lit,
            "sanity check: torch should still be lit with the lever off"
        );
        assert!(
            simulator.world().get(0, 2, 0).lit,
            "lever off -> support unpowered -> torch lit -> lamp above it lit"
        );

        let mut on_lever = lever();
        on_lever.lit = true;
        simulator.world_mut().set(1, 0, 0, on_lever);
        simulator
            .run_until_stable(50)
            .expect("the chain must settle again after the lever flips");
        assert!(
            !simulator.world().get(0, 1, 0).lit,
            "sanity check: torch should now be off with the lever on"
        );
        assert!(
            !simulator.world().get(0, 2, 0).lit,
            "lever on -> support powered -> torch off -> lamp above it dark"
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

    #[test]
    fn a_repeater_delays_its_output_by_its_setting() {
        // delay=2 的中繼器：輸入變化後 4 game tick 輸出才變
        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, repeater(Facing::West, 2, false));

        let mut simulator = Simulator::new(world);
        assert!(!simulator.world().get(2, 0, 2).lit);

        let mut on_lever = lever();
        on_lever.lit = true;
        simulator.world_mut().set(1, 0, 2, on_lever); // 中繼器西邊 -- 就是它的輸入

        for tick in 1..=3 {
            simulator.step();
            assert!(
                !simulator.world().get(2, 0, 2).lit,
                "game tick {tick}: delay 是 2 redstone tick = 4 game tick，還不該變"
            );
        }

        simulator.step();
        assert!(
            simulator.world().get(2, 0, 2).lit,
            "第 4 個 game tick 之後應該已經開啟"
        );
    }

    #[test]
    fn a_repeater_does_not_pass_signal_backwards() {
        // 訊號從前方進不去 -- 這是二極體的定義
        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, repeater(Facing::West, 1, false));

        // 前方（輸出端）放一個一直充能的紅石塊 -- 如果中繼器誤把它當輸入，
        // 就會被觸發開啟
        world.set(3, 0, 2, named("minecraft:redstone_block", BlockKind::RedstoneBlock));

        let mut simulator = Simulator::new(world);
        for _ in 0..10 {
            simulator.step();
        }

        assert!(
            !simulator.world().get(2, 0, 2).lit,
            "power in front of a repeater must never be read as its input"
        );
    }

    #[test]
    fn a_locked_repeater_holds_its_output() {
        // 鎖住之後輸入怎麼變輸出都不動
        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, repeater(Facing::West, 1, true));

        // 北側放一個已充能、輸出朝南（正對這個中繼器）的中繼器 -- 鎖住它。
        // facing=North -> output south。它自己的輸入也持續供電，這樣它
        // 自己不會被排程關閉，鎖存才會全程有效，不會半途因為鎖它的那個
        // 中繼器關掉而解除。
        world.set(2, 0, 1, repeater(Facing::North, 1, true));
        let mut lock_input = lever();
        lock_input.lit = true;
        world.set(2, 0, 0, lock_input);

        let mut simulator = Simulator::new(world);
        assert!(simulator.world().get(2, 0, 2).lit);

        // 這個中繼器的輸入（西邊）從頭到尾都沒有訊號 -- 若沒被鎖住，
        // 它會在一個 delay 之後關閉
        for _ in 0..10 {
            simulator.step();
            assert!(
                simulator.world().get(2, 0, 2).lit,
                "a locked repeater must hold its output no matter what its input does"
            );
            assert!(
                simulator.world().get(2, 0, 1).lit,
                "the locking repeater's own input must stay powered throughout the test"
            );
        }
    }

    #[test]
    fn a_repeater_swallows_a_pulse_shorter_than_its_delay() {
        // 這應該是「排程中不重排」自然的結果，不是特例邏輯
        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, repeater(Facing::West, 4, true)); // delay = 8 game tick

        let mut on_lever = lever();
        on_lever.lit = true;
        world.set(1, 0, 2, on_lever.clone()); // 輸入一開始就穩定供電，跟輸出一致

        let mut simulator = Simulator::new(world);
        simulator.step(); // 沒有 mismatch，不會排程
        assert!(simulator.world().get(2, 0, 2).lit);

        // 輸入短暫斷電 1 個 game tick -- 遠短於 8 game tick 的延遲
        let mut off_lever = lever();
        off_lever.lit = false;
        simulator.world_mut().set(1, 0, 2, off_lever);
        simulator.step(); // 偵測到 mismatch，排程「關閉」在 8 game tick 之後

        // 輸入立刻恢復，遠早於排定的關閉會觸發
        simulator.world_mut().set(1, 0, 2, on_lever);

        for _ in 0..10 {
            simulator.step();
            assert!(
                simulator.world().get(2, 0, 2).lit,
                "an off-pulse shorter than the delay must be swallowed entirely"
            );
        }
    }

    #[test]
    fn a_comparator_output_settles_after_one_redstone_tick() {
        // 拉桿 -> 比較器：延遲跟中繼器、火把一樣是 2 個 game tick
        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, comparator(Facing::West, 0, false));
        world.set(1, 0, 2, lever()); // 比較器西邊 -- 就是它的後方，拉桿一開始是關的

        let mut simulator = Simulator::new(world);
        assert_eq!(simulator.world().get(2, 0, 2).power, 0);

        let mut on_lever = lever();
        on_lever.lit = true;
        simulator.world_mut().set(1, 0, 2, on_lever);

        simulator.step();
        assert_eq!(
            simulator.world().get(2, 0, 2).power,
            0,
            "第 1 個 game tick 還不該變 -- 延遲是 2 個 game tick"
        );

        simulator.step();
        assert_eq!(
            simulator.world().get(2, 0, 2).power,
            15,
            "第 2 個 game tick 之後輸出應該已經到位"
        );
        assert!(simulator.world().get(2, 0, 2).lit);
    }

    #[test]
    fn a_comparator_carries_an_analog_level_not_just_on_off() {
        // rear 給 9，輸出必須是 9，不是 15 -- 硬編成開關就是把比較器做成中繼器
        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, comparator(Facing::West, 0, false));

        // 用 target 當靜態的類比訊號源：它不會被排程改變，純粹是測試用的
        // 訊號源，跟其他測試用拉桿當固定 15 的訊號源是同一個道理。
        let mut source = named("minecraft:target", BlockKind::Target);
        source.power = 9;
        world.set(1, 0, 2, source);

        let mut simulator = Simulator::new(world);
        simulator.step();
        assert_eq!(simulator.world().get(2, 0, 2).power, 0, "還在延遲中");

        simulator.step();
        assert_eq!(
            simulator.world().get(2, 0, 2).power,
            9,
            "類比訊號必須原樣傳遞，不能被鎖死成 15"
        );
        assert!(simulator.world().get(2, 0, 2).lit);
    }

    #[test]
    fn dust_laid_behind_a_repeater_feeds_it() {
        // 拉桿 -> 粉 -> 中繼器 -> 粉。這是最常見的接法 —— 少了它模擬器
        // 對任何真實電路都沒用。
        //
        // 拉桿在建構世界時就是開的：`Simulator::new` 會先做一次穩定計算，
        // 這樣不必再靠 `run_until_stable` 去追一次外部改動造成的粉強度
        // 快取落後 —— 那不是這裡要測的東西。
        let mut world = World::new(6, 3, 3);
        let mut on_lever = lever();
        on_lever.lit = true;
        world.set(0, 1, 0, on_lever);
        world.set(1, 0, 0, stone());
        world.set(1, 1, 0, dust()); // 中繼器正後方，跟中繼器同一層
        world.set(2, 1, 0, repeater(Facing::West, 1, false));
        world.set(3, 0, 0, stone());
        world.set(3, 1, 0, dust()); // 中繼器的輸出端

        let mut simulator = Simulator::new(world);

        simulator
            .run_until_stable(50)
            .expect("a lever feeding dust into a repeater must settle");

        assert!(
            simulator.world().get(2, 1, 0).lit,
            "dust laid directly behind a repeater, on the same level, must feed it"
        );
    }

    #[test]
    fn dust_behind_a_comparator_feeds_its_rear_input() {
        // 粉的強度必須原樣進到比較器的後方輸入 —— 不能被壓成固定值。
        let mut world = World::new(6, 3, 3);
        let mut on_lever = lever();
        on_lever.lit = true;
        world.set(0, 1, 0, on_lever);
        world.set(1, 0, 0, stone());
        world.set(1, 1, 0, dust()); // 貼著拉桿，滿強度 15
        world.set(2, 0, 0, stone());
        world.set(2, 1, 0, dust()); // 再走一格，衰減成 14，緊貼比較器後方
        world.set(3, 1, 0, comparator(Facing::West, 0, false));

        let mut simulator = Simulator::new(world);

        simulator
            .run_until_stable(50)
            .expect("a lever feeding dust into a comparator's rear must settle");

        assert_eq!(
            simulator.world().get(3, 1, 0).power,
            14,
            "the comparator's rear input must receive the dust's actual decayed \
             strength, not a fixed constant"
        );
    }

    #[test]
    fn a_weak_signal_still_reaches_a_repeater() {
        // 走了 10 格衰減到 5 的粉，中繼器仍然該被觸發 ——
        // 中繼器只看有沒有訊號，不看強度。
        let mut world = World::new(15, 3, 3);
        let mut on_lever = lever();
        on_lever.lit = true;
        world.set(0, 1, 0, on_lever);
        for x in 1..=11 {
            world.set(x, 0, 0, stone());
            world.set(x, 1, 0, dust());
        }
        world.set(12, 1, 0, repeater(Facing::West, 1, false));

        let mut simulator = Simulator::new(world);

        simulator
            .run_until_stable(50)
            .expect("a weak but present signal must still settle the repeater");

        assert_eq!(
            simulator.world().get(11, 1, 0).power,
            5,
            "sanity check: by the eleventh block the wire should have decayed to strength 5"
        );
        assert!(
            simulator.world().get(12, 1, 0).lit,
            "a repeater only cares whether its input has any signal, not how strong"
        );
    }

    // -----------------------------------------------------------------
    // Convention pin: these two tests exist so a future change cannot
    // silently invert `facing` back to the wrong direction the way the
    // original implementation did. Every other test in this file already
    // exercises the *current* convention, but every one of them also passed
    // when the convention was inverted (input/output simply swapped ends
    // together with the setup) -- see the commit that fixed this bug. What
    // pins the convention here is deriving the exact compass directions from
    // an independent source instead of from the code under test: both the
    // wiki's wording and a real server's actual behaviour.
    //
    // Verified against a real vanilla 1.20.1 server over RCON (not just
    // read off the wiki), with the mirrored pair of `/setblock` experiments
    // this test also encodes:
    //   /setblock <p> minecraft:repeater[facing=north]
    //   /setblock <p+north> minecraft:redstone_block
    //   /setblock <p+south> minecraft:redstone_lamp
    //   -> repeater powered=true, lamp lit=true
    // and the mirror (redstone block to the south, lamp to the north of the
    // same repeater) -> repeater powered=false, lamp unlit. Repeaters and
    // lamps are not block entities, so `/data get block` cannot read their
    // state -- use `/execute if block <pos> minecraft:redstone_lamp[lit=true]`
    // ("Test passed"/"Test failed") instead.
    // -----------------------------------------------------------------

    #[test]
    fn a_north_facing_repeater_matches_the_wiki_convention() {
        // minecraft.wiki/w/Redstone_Repeater, blockstate table, `facing`:
        // "The direction from the output side to the input side of a
        // repeater. The opposite from the direction the player faces while
        // placing the repeater."
        //
        // So `facing = North` means the input is to the north and the
        // output is to the south -- the opposite of treating `facing` as
        // "the direction the repeater points its output". Confirmed live
        // (see the block comment above this test) on a real 1.20.1 server:
        // facing=north + redstone block to the north + lamp to the south
        // lights the lamp; swapping the block and lamp to the opposite
        // sides does not.
        let mut world = World::new(5, 5, 5);
        let repeater_pos = Position::new(2, 0, 2);
        world.set(repeater_pos.x, repeater_pos.y, repeater_pos.z, repeater(Facing::North, 1, false));

        let mut on_lever = lever();
        on_lever.lit = true;
        world.set(2, 0, 1, on_lever); // north of the repeater -- must be its input

        world.set(2, 0, 3, lamp()); // south of the repeater -- must be its output

        let mut simulator = Simulator::new(world);
        simulator
            .run_until_stable(50)
            .expect("a lever feeding a repeater's input must settle");

        assert!(
            simulator.world().get(repeater_pos.x, repeater_pos.y, repeater_pos.z).lit,
            "facing=North must read its input from the north, where the lit lever sits"
        );
        assert!(
            simulator.world().get(2, 0, 3).lit,
            "facing=North must drive its output south, powering the lamp there \
             (this is the exact case that was inverted: a repeater's output pointed \
             at its own input side and never reached the far side)"
        );
    }

    #[test]
    fn a_north_facing_comparator_matches_the_wiki_convention() {
        // minecraft.wiki/w/Redstone_Comparator, blockstate table, `facing`:
        // "The direction from the output side to the input side of the
        // comparator, or the opposite from the direction the player faces
        // while placing the comparator." -- stated as identical to a
        // repeater's, and independently confirmed on the same real 1.20.1
        // server rather than assumed from the wiki text alone: the same
        // mirrored `/setblock` pair described above this test's sibling,
        // with `minecraft:comparator[facing=north]` in place of the
        // repeater, gave comparator powered=true/lamp lit=true with the
        // redstone block to the north, and powered=false/unlit with it to
        // the south.
        let mut world = World::new(5, 5, 5);
        let comparator_pos = Position::new(2, 0, 2);
        world.set(comparator_pos.x, comparator_pos.y, comparator_pos.z, comparator(Facing::North, 0, false));

        let mut on_lever = lever();
        on_lever.lit = true;
        world.set(2, 0, 1, on_lever); // north of the comparator -- must be its rear input

        world.set(2, 0, 3, lamp()); // south of the comparator -- must be its output

        let mut simulator = Simulator::new(world);
        simulator
            .run_until_stable(50)
            .expect("a lever feeding a comparator's rear input must settle");

        assert!(
            simulator.world().get(comparator_pos.x, comparator_pos.y, comparator_pos.z).power > 0,
            "facing=North must read its main signal from the north, where the lit lever sits"
        );
        assert!(
            simulator.world().get(2, 0, 3).lit,
            "facing=North must drive its output south, powering the lamp there"
        );
    }

    #[test]
    fn changing_an_input_takes_effect_without_a_manual_step() {
        // 呼叫端改了輸入之後直接 run_until_stable，就該看到新的結果。
        // 先前這裡會靜靜地回報「已穩定」而電路還停在舊狀態 ——
        // 一個回報穩定卻停在舊狀態的模擬器，比會報錯的更糟。
        //
        // 建一個「拉桿 -> 石頭 -> 粉」的最小電路，跑到穩定，
        // 然後翻轉拉桿、直接 run_until_stable，確認粉跟著變。
        let mut world = World::new(5, 5, 5);
        world.set(0, 0, 0, lever()); // 拉桿一開始是關的
        world.set(1, 0, 0, stone());
        world.set(1, 1, 0, dust());

        let mut simulator = Simulator::new(world);
        simulator
            .run_until_stable(50)
            .expect("an already-off circuit is trivially stable");
        assert_eq!(
            simulator.world().get(1, 1, 0).power,
            0,
            "lever is off -- dust should carry no signal yet"
        );

        let mut on_lever = lever();
        on_lever.lit = true;
        simulator.world_mut().set(0, 0, 0, on_lever);

        // 沒有中繼器、火把或比較器能靠排程佇列偵測到這次外部修改 --
        // 佇列從頭到尾都是空的。直接呼叫 run_until_stable，不手動 step()。
        simulator
            .run_until_stable(50)
            .expect("circuit must settle after the lever is flipped");

        assert_eq!(
            simulator.world().get(1, 1, 0).power,
            15,
            "flipping the lever must reach the dust without a manual step() in between"
        );
    }

    #[test]
    fn lit_and_power_stay_consistent() {
        // power == 0 必須 lit == false，反之亦然
        let mut world = World::new(5, 5, 5);
        world.set(2, 0, 2, comparator(Facing::West, 0, false));
        world.set(1, 0, 2, lever());

        let mut simulator = Simulator::new(world);
        assert_eq!(simulator.world().get(2, 0, 2).power, 0);
        assert!(!simulator.world().get(2, 0, 2).lit);

        let mut on_lever = lever();
        on_lever.lit = true;
        simulator.world_mut().set(1, 0, 2, on_lever);
        for _ in 0..4 {
            simulator.step();
        }
        let powered = simulator.world().get(2, 0, 2);
        assert!(powered.power > 0, "後方有訊號，power 應該非 0");
        assert!(powered.lit, "power 非 0 就必須 lit == true");

        let mut off_lever = lever();
        off_lever.lit = false;
        simulator.world_mut().set(1, 0, 2, off_lever);
        for _ in 0..4 {
            simulator.step();
        }
        let unpowered = simulator.world().get(2, 0, 2);
        assert_eq!(unpowered.power, 0, "後方訊號消失，power 應該歸零");
        assert!(!unpowered.lit, "power == 0 就必須 lit == false");
    }
}

//! Scheduled tick 佇列：決定每個元件「何時」動作。
//!
//! 每個紅石元件的行為都是「排程一次未來的狀態改變」——中繼器設定 2 刻，
//! 就是排在 4 個 game tick 之後；紅石火把排在 2 個 game tick 之後。
//! 這個模組就是管理這些排程的佇列。
//!
//! 原版用 game tick 計數，1 個紅石刻 = 2 個 game tick。

use std::collections::BTreeMap;
use std::collections::HashSet;

use crate::redstone::simulator::position::Position;

/// Minecraft 的四級 scheduled tick 優先權。
///
/// 數值越小越先執行。這些優先權的存在是為了讓 diode 鏈的行為確定 ——
/// 少了它們，兩個相鄰中繼器誰先動就變成實作細節。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TickPriority {
    Highest,
    Higher,
    High,
    Normal,
}

/// The queue's own floor on how soon a scheduled change can actually apply:
/// one game tick, no matter what delay the caller asks for.
///
/// `advance` always moves `current_tick` forward *before* looking at that
/// tick's bucket (see its own doc comment), so by the time anything calls
/// `schedule`, "this tick" has already been consumed for this round -- there
/// is no way to land a change in a bucket that has already been read.
/// Requesting delay 0 therefore cannot mean "apply now"; the earliest any
/// scheduled change can land is the *next* `advance`, exactly like delay 1.
///
/// This is not specific to any one component: it is a property of the queue
/// itself, and applies identically to every caller. It only ever changes
/// behaviour for a component whose real delay is `0` game ticks --
/// `LAMP_TURN_ON_DELAY_GAME_TICKS` (see `component.rs`) is the only such
/// constant in this codebase, and `crate::timing`'s settle-time model has to
/// account for this same floor to predict a lamp turning on correctly (see
/// `critical_path_settle_model_game_ticks`).
pub const MIN_SCHEDULE_DELAY_GAME_TICKS: u64 = 1;

/// 一筆排程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledTick {
    pub position: Position,
    pub at_game_tick: u64,
    pub priority: TickPriority,
}

/// scheduled tick 的佇列。
///
/// 用 `BTreeMap<u64, Vec<ScheduledTick>>` 依 game tick 分桶，桶內的
/// `Vec` 依插入順序累積，`advance` 時再依 `(priority, 插入順序)` 排序 ——
/// 這樣時間順序、優先權順序、插入順序三者都是確定的，不會有 `HashMap`
/// 走訪順序不穩定的問題。另外用一個 `HashSet<Position>` 追蹤「這個位置
/// 目前有沒有待處理的排程」，讓 `is_scheduled` 是 O(1)。
pub struct TickQueue {
    current_tick: u64,
    pending: BTreeMap<u64, Vec<ScheduledTick>>,
    scheduled_positions: HashSet<Position>,
    pending_count: usize,
}

impl TickQueue {
    pub fn new() -> Self {
        TickQueue {
            current_tick: 0,
            pending: BTreeMap::new(),
            scheduled_positions: HashSet::new(),
            pending_count: 0,
        }
    }

    /// 目前的 game tick。
    pub fn current_tick(&self) -> u64 {
        self.current_tick
    }

    /// 排程一筆未來的狀態改變。
    ///
    /// `delay_game_ticks` 是從現在算起的 game tick 數。
    ///
    /// **同一個位置同時只能有一筆排程** —— 若該位置已有待處理的排程，
    /// 這次呼叫沒有作用並回傳 `false`。原版就是這樣，少了這條會讓中繼器
    /// 觸發兩次。
    pub fn schedule(
        &mut self,
        position: Position,
        delay_game_ticks: u64,
        priority: TickPriority,
    ) -> bool {
        if !self.scheduled_positions.insert(position) {
            return false;
        }

        // `advance` 一律先把 current_tick 往前推一格才檢查該格的排程，
        // 所以「這一格」的排程桶在呼叫 `schedule` 的當下必然已經被清空、
        // 不會再被檢查到。因此 delay 0（「就這個 tick」）實務上等同於
        // 「下一次 advance 就會發生」，跟 delay 1 落在同一格 -- see
        // `MIN_SCHEDULE_DELAY_GAME_TICKS`.
        let at_game_tick = self.current_tick + delay_game_ticks.max(MIN_SCHEDULE_DELAY_GAME_TICKS);
        self.pending.entry(at_game_tick).or_default().push(ScheduledTick {
            position,
            at_game_tick,
            priority,
        });
        self.pending_count += 1;
        true
    }

    /// 該位置有沒有待處理的排程。
    pub fn is_scheduled(&self, position: Position) -> bool {
        self.scheduled_positions.contains(&position)
    }

    /// 推進一個 game tick，回傳這一刻要執行的排程，已依優先權與插入順序排好。
    pub fn advance(&mut self) -> Vec<ScheduledTick> {
        self.current_tick += 1;

        let Some(mut due) = self.pending.remove(&self.current_tick) else {
            return Vec::new();
        };

        // Vec::sort 是穩定排序，同優先權的元素維持原本（插入）順序。
        due.sort_by_key(|tick| tick.priority);

        for tick in &due {
            self.scheduled_positions.remove(&tick.position);
        }
        self.pending_count -= due.len();

        due
    }

    /// 還有沒有待處理的排程。
    pub fn is_empty(&self) -> bool {
        self.pending_count == 0
    }

    /// 待處理的排程總數。給發散偵測用。
    pub fn pending_count(&self) -> usize {
        self.pending_count
    }
}

impl Default for TickQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32, y: i32, z: i32) -> Position {
        Position::new(x, y, z)
    }

    #[test]
    fn a_new_queue_is_empty_and_starts_at_tick_zero() {
        let queue = TickQueue::new();
        assert_eq!(queue.current_tick(), 0);
        assert!(queue.is_empty());
        assert_eq!(queue.pending_count(), 0);
    }

    #[test]
    fn advancing_past_a_scheduled_tick_yields_it_once() {
        let mut queue = TickQueue::new();
        queue.schedule(pos(1, 2, 3), 2, TickPriority::Normal);

        let first = queue.advance();
        assert!(first.is_empty(), "tick 1 之後不應該有東西");

        let second = queue.advance();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].position, pos(1, 2, 3));
        assert_eq!(second[0].at_game_tick, 2);

        let third = queue.advance();
        assert!(third.is_empty(), "同一筆排程不能吐出第二次");
    }

    #[test]
    fn zero_delay_still_comes_out() {
        let mut queue = TickQueue::new();
        queue.schedule(pos(0, 0, 0), 0, TickPriority::Normal);

        let due = queue.advance();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].position, pos(0, 0, 0));
        assert_eq!(due[0].at_game_tick, 1);
    }

    #[test]
    fn scheduling_a_position_twice_is_a_no_op() {
        let mut queue = TickQueue::new();
        let first = queue.schedule(pos(5, 5, 5), 3, TickPriority::Normal);
        let second = queue.schedule(pos(5, 5, 5), 1, TickPriority::High);

        assert!(first);
        assert!(!second);

        for _ in 0..2 {
            assert!(queue.advance().is_empty());
        }
        let due = queue.advance();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].position, pos(5, 5, 5));
        assert_eq!(due[0].priority, TickPriority::Normal);
    }

    #[test]
    fn a_position_can_be_rescheduled_after_its_tick_fires() {
        let mut queue = TickQueue::new();
        queue.schedule(pos(1, 1, 1), 1, TickPriority::Normal);
        queue.advance();
        assert!(!queue.is_scheduled(pos(1, 1, 1)));

        let rescheduled = queue.schedule(pos(1, 1, 1), 1, TickPriority::Normal);
        assert!(rescheduled);
        let due = queue.advance();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].position, pos(1, 1, 1));
    }

    #[test]
    fn higher_priority_runs_first_within_the_same_tick() {
        let mut queue = TickQueue::new();
        // 刻意以優先權相反的順序插入
        queue.schedule(pos(0, 0, 0), 1, TickPriority::Normal);
        queue.schedule(pos(0, 0, 1), 1, TickPriority::High);
        queue.schedule(pos(0, 0, 2), 1, TickPriority::Higher);
        queue.schedule(pos(0, 0, 3), 1, TickPriority::Highest);

        let due = queue.advance();
        assert_eq!(due.len(), 4);
        assert_eq!(due[0].priority, TickPriority::Highest);
        assert_eq!(due[1].priority, TickPriority::Higher);
        assert_eq!(due[2].priority, TickPriority::High);
        assert_eq!(due[3].priority, TickPriority::Normal);
    }

    #[test]
    fn same_priority_preserves_insertion_order() {
        let mut queue = TickQueue::new();
        queue.schedule(pos(1, 0, 0), 1, TickPriority::Normal);
        queue.schedule(pos(2, 0, 0), 1, TickPriority::Normal);
        queue.schedule(pos(3, 0, 0), 1, TickPriority::Normal);

        let due = queue.advance();
        assert_eq!(
            due.iter().map(|t| t.position).collect::<Vec<_>>(),
            vec![pos(1, 0, 0), pos(2, 0, 0), pos(3, 0, 0)]
        );
    }

    #[test]
    fn ticks_come_out_in_time_order() {
        let mut queue = TickQueue::new();
        queue.schedule(pos(5, 0, 0), 5, TickPriority::Normal);
        queue.schedule(pos(1, 0, 0), 1, TickPriority::Normal);
        queue.schedule(pos(3, 0, 0), 3, TickPriority::Normal);

        let tick1 = queue.advance();
        assert_eq!(tick1.len(), 1);
        assert_eq!(tick1[0].position, pos(1, 0, 0));

        assert!(queue.advance().is_empty());

        let tick3 = queue.advance();
        assert_eq!(tick3.len(), 1);
        assert_eq!(tick3[0].position, pos(3, 0, 0));

        assert!(queue.advance().is_empty());

        let tick5 = queue.advance();
        assert_eq!(tick5.len(), 1);
        assert_eq!(tick5[0].position, pos(5, 0, 0));
    }

    #[test]
    fn pending_count_tracks_outstanding_work() {
        let mut queue = TickQueue::new();
        assert_eq!(queue.pending_count(), 0);

        queue.schedule(pos(0, 0, 0), 1, TickPriority::Normal);
        assert_eq!(queue.pending_count(), 1);

        queue.schedule(pos(1, 0, 0), 2, TickPriority::Normal);
        assert_eq!(queue.pending_count(), 2);

        // 重複排程同一個位置不會增加計數
        queue.schedule(pos(0, 0, 0), 5, TickPriority::Normal);
        assert_eq!(queue.pending_count(), 2);

        queue.advance();
        assert_eq!(queue.pending_count(), 1);

        queue.advance();
        assert_eq!(queue.pending_count(), 0);
        assert!(queue.is_empty());
    }
}

//! 紅石模擬器。
//!
//! 逐 game tick 推進，訊號傳播依功率流方向做 BFS（非 locational）。
//!
//! **本階段不支援** quasi-connectivity、活塞、觀察者。碰到這些元件會明確
//! 報錯，不會靜默忽略。

pub mod connectivity;
pub mod position;
pub mod propagate;
pub mod schedule;

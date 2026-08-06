//! 網表產生器：只用 NOR，fan-in 硬性上限 3。
//!
//! Shared by every reference circuit under `circuits/` -- the seven-segment
//! decoder, and the smaller circuits alongside it -- so the NOR-tree
//! expansion logic (`not`, `and_reduce`, `or_reduce`) lives in exactly one
//! place instead of being copied per circuit.

use std::collections::HashMap;

use crate::compile::Gate;

/// 一步一步把 NOR 閘疊起來的產生器。
///
/// - `not`：對同一個訊號重複呼叫回傳同一個共用的反相閘（用 `not_cache`
///   記住），這就是「四個輸入的反相閘只建一次，之後每個 minterm 共用」
///   的機制。
/// - `nor`：最原始的操作，建一個新的 NOR 閘，最多 3 個輸入 —— 對應
///   `place_nor_gate` 的硬體限制。
/// - `and_reduce` / `or_reduce`：把任意長度的訊號清單摺成一棵 fan-in <= 3
///   的樹，分別算出它們的 AND / OR。
pub(crate) struct NetlistBuilder {
    pub(crate) gates: Vec<Gate>,
    not_cache: HashMap<String, String>,
    counter: usize,
}

impl NetlistBuilder {
    pub(crate) fn new() -> Self {
        NetlistBuilder { gates: Vec::new(), not_cache: HashMap::new(), counter: 0 }
    }

    fn fresh_name(&mut self) -> String {
        let name = format!("g{}", self.counter);
        self.counter += 1;
        name
    }

    /// 建一個新的 NOR 閘，`inputs.len()` 必須在 1..=3 之間。
    pub(crate) fn nor(&mut self, inputs: &[String]) -> String {
        assert!(
            !inputs.is_empty() && inputs.len() <= 3,
            "place_nor_gate 最多 3 個輸入，收到 {}",
            inputs.len()
        );
        let output = self.fresh_name();
        self.gates.push(Gate {
            name: output.clone(),
            inputs: inputs.to_vec(),
            output: output.clone(),
        });
        output
    }

    /// `NOT x`，同一個 `x` 只會建一次閘，之後都回傳快取的輸出名稱。
    pub(crate) fn not(&mut self, x: &str) -> String {
        if let Some(cached) = self.not_cache.get(x) {
            return cached.clone();
        }
        let output = self.nor(&[x.to_string()]);
        self.not_cache.insert(x.to_string(), output.clone());
        output
    }

    /// 任意長度訊號清單的 AND，摺成 fan-in <= 3 的樹。
    ///
    /// 每一層把訊號三個三個分組：組裡的每個訊號先取 `NOT`（如果是原始
    /// 輸入或別的 minterm 已經算過的反相，直接命中快取，不新建閘），
    /// 再用一個 NOR 閘算這一組的 AND（De Morgan：
    /// `AND(a,b,c) = NOR(NOT a, NOT b, NOT c)`）。落單的訊號直接晉級到
    /// 下一層，不建新閘。
    pub(crate) fn and_reduce(&mut self, signals: Vec<String>) -> String {
        let mut level = signals;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(3));
            for chunk in level.chunks(3) {
                if chunk.len() == 1 {
                    next.push(chunk[0].clone());
                } else {
                    let nots: Vec<String> = chunk.iter().map(|s| self.not(s)).collect();
                    next.push(self.nor(&nots));
                }
            }
            level = next;
        }
        level.into_iter().next().expect("and_reduce called with an empty signal list")
    }

    /// 任意長度訊號清單的 OR，摺成 fan-in <= 3 的樹。
    ///
    /// `OR(a,b,c) = NOT(NOR(a,b,c))`：每組先算 NOR，再反相一次拿到真正
    /// 的 OR 值，這樣才能繼續往上一層跟別組的 OR 值再取 OR。落單的訊號
    /// 直接晉級,不建新閘。
    pub(crate) fn or_reduce(&mut self, signals: Vec<String>) -> String {
        let mut level = signals;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(3));
            for chunk in level.chunks(3) {
                if chunk.len() == 1 {
                    next.push(chunk[0].clone());
                } else {
                    let nor_out = self.nor(chunk);
                    next.push(self.not(&nor_out));
                }
            }
            level = next;
        }
        level.into_iter().next().expect("or_reduce called with an empty signal list")
    }
}

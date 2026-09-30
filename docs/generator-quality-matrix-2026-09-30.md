# 擴大電路形狀觀察（2026-09-30）

基準 `83de6af`，生成器無修改。新增 `tests/generator_quality_matrix.rs`，以獨立布林公式檢查原生與 fresh Yosys 的 XOR、2 選 1 選擇器、多數決電路，兩個前端各跑全部輸入狀態間轉換。

| 案例 | 前端 | lowered gates | 方塊 | 中繼器 | 最慢 ticks |
|---|---|---:|---:|---:|---:|
| xor2 | 原生／Yosys | 5 | 193 | 7 | 14 |
| mux2 | 原生／Yosys | 6 | 174 | 3 | 12 |
| majority3 | 原生 | 8 | 588 | 15 | 18 |
| majority3 | Yosys | 8 | 792 | 23 | 26 |

XOR 兩前端產物的完整指紋相同，選擇器亦相同。XOR／選擇器走 Unified3d，多數決走 channel emitter。六個產物總共 288 次獨立真值轉換檢查通過。測試只建立此輪固定觀察基準，未以案例名稱修改生成規則。

追查多數決 lowered 產物：原生是三個兩輸入 AND 的合併；Yosys 是 `(a&b) | (c&(a|b))` 的因式分解。Yosys 產物包含 `g1 = merge(b,a)` 後立刻接 `n4 = nor(g1)`，再和 `not(c)` 形成下一個 NOR。這個 Merge → Nor1 可以用一個 Nor2 表達其反相邏輯，值得追查現有 lowering／builder 是否已能消除此類中間段。閘數相同未代表拓撲、時序與物理成本相同。

此輪沒有生成器優化提交。原始來源、量測、指紋與物理 metrics 保存在相鄰 JSON；後續變更將以這些來源與獨立真值公式對照，不修改這份基準來使實驗通過。

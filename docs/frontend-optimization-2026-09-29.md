# 原生前端最佳化（2026-09-29）

本次變更以 `d8f3af0` 為基底，接續 [前端品質重評](frontend-quality-2026-09-29.md)。Yosys 正式入口、baked netlist、lowering、佈局、routing 與 baseline 均未修改。

## 品質結果

| seven_segment 指標 | 原生前端修改前 | 原生前端修改後 | 同次 fresh Yosys | 目標 |
|---|---:|---:|---:|---:|
| lowered gates | 94 | 30 | 47 | 僅報告 |
| 非空氣方塊 | 17,814 | 8,980 | 10,088 | ≤10,088 |
| occupied 體積 | 360,036 | 100,408 | 213,816 | ≤213,816 |
| 最差全世界穩定時間（game ticks） | 133 | 52 | 84 | ≤84 |

相對 Yosys，方塊減少約 11.0%、體積減少 53.0%、最差穩定時間減少 38.1%。原生 and4 保持 228 blocks、3,528 volume、28 ticks，world fingerprint 與修改前相同。

七段顯示器的最終電路為 24 個 NOR（4×NOR1、11×NOR2、9×NOR3），以及 6 個合流（1×merge2、5×merge3）。兩個前端的七段顯示器均由 `compile()` 的 Legacy 路徑產生。

## 實作

1. 保留已驗證的互斥 case 標籤。常數分支、常數 default、selector 寬度 ≤6 時，建立完整 truth table；沒有把非法 BCD 值當成 don't-care。
2. 合併只差一個 literal 的 implicants，生成 prime implicants，再以必要 implicants、未覆蓋 minterms 數、literal 數及固定鍵挑選 cover，最後移除冗餘項。這是有界且決定性的貪婪 cover，不宣稱全域最小。
3. 同時建立正、反相 cover，映射到已有的 NOR／merge 元件，最大 fan-in 為 3。優先選擇有主動驅動的輸出，再比較新增加的邏輯元件數，讓不同輸出共用已選中的邏輯。
4. 大於 6 位元、帶動態分支或非 literal 常數的 case 保持通用路徑；零 default 可利用互斥性改成獨立 gating 後合流。沒有辨識模組名、七段顯示器真值表或某組固定 pins。
5. 每個最小化輸出仍連回原始 case 標籤、值與 default 的 source spans。HashSet 只供 membership 和計數使用，不以其迭代順序做選擇。

為何不只壓低 gate 數：單純因式分解曾把前端 gates 降至 46，卻產生 16,082 blocks。只按共用節點數選正反相的候選通過了邏輯測試，但實體模擬失敗，因此未採用。最終版本的選擇規則明確優先主動輸出，並以獨立的實體轉換測試驗證。

## 驗證與重跑

品質 gate 是 `tests/frontend_quality_report.rs`，現為普通測試。它使用與重評相同的 `lower_optimised → compile` 路徑，每個產物測全部 256 個有序狀態轉換、同時設定四個輸入、檢查全部輸出並量到全世界穩定。品質門檻只加在原生前端，沒有改動既有 acceptance。

```bash
source tools/env.sh
cargo test --locked --release --test frontend_quality_report -- --nocapture
cargo test --locked --release --lib frontend::
cargo test --locked --release --test systemverilog_compiler --test systemverilog_seven_segment --test systemverilog_dff --test systemverilog_fuzz --test logical_evaluator --test m4_shadow_validation --test netlist_fingerprint_independence --test m5_and4_bridge --test m5_tier_c_bridge --test m3_m4_dff_yosys_differential --test verilog_frontend
cd viewer
cargo test --locked --release --test frontend_compiler
wasm-pack test --node --release -- --locked --test frontend_compiler
```

cover 單元測試遍歷所有 256 個三輸入布林函數的正反相；整合測試另檢查 1／3／6／7 位元隨機完整真值表、非零與動態 default、反向 case 順序、決定性及標籤／值的雙向來源追蹤。原有 >10,000 expression fuzz、DFF 差分和物理橋接測試繼續保留。

### 驗證紀錄

- 12 個 native integration targets：72 passed，0 failed。
- 前端單元測試：21 passed，0 failed（包含全部 256 個三輸入函數的 cover 檢查）。
- 四個 fixture 的 netlist 與 source-map SHA-256：native / WASM 同一組固定值，parity 測試兩邊各 1 passed。
- 既有 wasm placement tests：6 passed，0 failed；root wasm32 check 通過。
- 三次獨立品質測試：每次 1,024 個狀態轉換全數通過，四列產物 fingerprint 與全部品質值相同。
- lowering 加實體 compile 的三次中位數：seven_segment 原生 196.39 ms、Yosys netlist 407.95 ms；and4 原生 12.73 ms、Yosys netlist 11.45 ms。這些不含 frontend 合成時間，且不作為 acceptance 門檻。
- [三次原始數據與原始碼 SHA-256](frontend-optimization-2026-09-29.json)保留完整 bbox、histogram、worst transition 與 world fingerprint。
- `cargo clippy --lib --tests` 完成；專案既有警告仍在，本次不是全 repo 警告清理。沒有宣稱 `./check.sh` 或整個 repo 測試全綠。

## 範圍與仍未完成的 M5 條件

這是原生前端的小型常數 case 最佳化，不是完整 ABC 替代品，也不新增語言支援。量測是自家 simulator 的結果，不是 Minecraft 實機認證，亦不代表 `compile_fragment_synth` 的品質。

本次品質目標達標後，仍不能自動宣稱正式切換完成：generator 的跨合併穩定性、完整切換版測試與兩次連續 CI、owner 的 revision/fingerprint 記錄、DFF power-on 語意，以及正式 consumers 與 baked artifacts 的切換仍須按 M5 計畫處理。Yosys 依賴保留。

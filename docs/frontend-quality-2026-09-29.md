# 原生前端電路品質重評（2026-09-29）

這是最佳化前的量測；後續實作與驗證見 [原生前端最佳化](frontend-optimization-2026-09-29.md)。

基準 commit：`d8f3af049f2396f29023a4d62cdb15583d3e8035`。本次只新增量測與文件，沒有修改生成器、前端、baseline、baked netlist 或切換正式入口。

## 結論

仍不建議直接以原生前端取代正式 Yosys 路徑。功能測試通過，但七段顯示器的實體方塊數、體積與穩定時間都明顯較差。判斷依據應從「94 vs 47 gates」改為同一後端上的實體成本；也不宜把總 gate 數直接當成火把數。

| 電路 / 前端 | 前端 gates | lowered gates | 非空氣方塊 | occupied bbox | occupied 體積 | 最差 game ticks | 平均 game ticks |
|---|---:|---:|---:|---|---:|---:|---:|
| and4 / REDA | 3 | 7 | 228 | 49×4×18 | 3,528 | 28 | 6.5664 |
| and4 / Yosys | 3 | 7 | 214 | 48×4×15 | 2,880 | 22 | 5.7227 |
| seven_segment / REDA | 82 | 94 | 17,814 | 137×6×438 | 360,036 | 133 | 99.6094 |
| seven_segment / Yosys | 31 | 47 | 10,088 | 151×6×236 | 213,816 | 84 | 65.1211 |

相對本次 Yosys 對照：

- 七段顯示器：方塊 **+76.6%**，體積 **+68.4%**，最差穩定時間 **+58.3%**。
- and4：方塊 **+6.5%**，體積 **+22.5%**，最差穩定時間 **+27.3%**。相同 gate 數與 histogram 並不保證相同佈局；這次沒有隔離命名、排序與拓撲各自造成多少差異。
- 七段顯示器的 94 gates 是 **38 NOR + 56 merges**；Yosys 的 47 是 **30 NOR + 17 merges**。47 個額外 gates 中，39 個是線路合流；NOR 數只增加 26.7%。合流仍需要實體佈線，並不代表零方塊或零延遲。
- 先前計畫文字稱 and4「frontend 與 lowered 都是 3 gates」不正確：本次 `lower_optimised` 的兩邊都是 **3 → 7**。

## 量測方法與限制

- Linux x86_64、Rust 1.98.1、release、Yosys 0.69（yowasp-yosys 0.69.0.0.post1233）。
- REDA 使用既有 `and4.sv`、`seven_segment.sv`；Yosys **現場合成** catalog 中相同函數的 `.v` fixture，沒有把 baked 載入時間當成合成時間。兩份語法不同的輸入都對照獨立真值表。
- 兩邊一律 `lower_optimised → compile`。and4 兩邊都是 `Unified3d`；七段顯示器兩邊都是 `Legacy`。這不是 `compile_fragment_synth` / `--synth` 的品質評估。
- 每個產物測全部 **16×16 = 256** 個有序轉換（含不變狀態）。先把來源狀態設好並等穩定，再同時設定目的狀態的四個輸入，量到整個 simulator 穩定；每次都檢查全部輸出。測試以固定順序沿用同一 simulator，並非每對轉換重建世界。
- 因採用同時切換多輸入、全部狀態對，這裡的 ticks **不可直接與 README 的逐輸入切換 sweep 比較**。量的是全世界穩定時間，並非僅輸出可讀時間；包含內部暫態。
- and4 在這裡也使用最佳化 lowering；原有 `verilog_frontend` 的 and4 測試使用 `lower`，所以其 9 gates / 290 blocks 不能直接混入本表。
- 三次獨立 process 重跑，每次 1,024 個轉換，全數功能正確；每一列的 world fingerprint、大小與 ticks 三次一致。時間樣本與三次中位數存於同名 JSON，時間不參與品質門檻。
- 這些是自家 simulator 的結果，沒有新增真實 Minecraft 認證；兩個組合電路不能代表所有可合成 SystemVerilog。

## 正確性與移除 Yosys 的判斷

本次另重跑 `m4_shadow_validation`（4 tests）、`m5_tier_c_bridge`（4 tests）、`m3_m4_dff_yosys_differential`（1 test），共 9 個全部通過。DFF 的物理測試含 priming edge，不能据此宣稱已解決 power-on 的 `q=0` / `q=1` 差異。本次沒有做 DFF 的新尺寸與時序對照。

M5 的品質條件仍未滿足；本報告沒有替 generator owner 接受品質退步或降低 acceptance gate。建議後續針對七段顯示器先降低 case 解码的合流與佈線成本，再用相同實體量測確認收益。`elaborate::case_assignment` 目前把 case 展成巢狀 mux，`logic` 主要做結構共用與局部簡化；這是值得實驗的優化位置，尚未以消融實驗證明它是全部差距的原因。

切換評估應同時報告正確性、方塊、體積、最差穩定時間和 planner 路徑，不以單一 gate 比率批准切換。實際可接受的退步幅度仍需明確決定；本次不預設一個百分比替代原計畫的批准條件。

## 重跑

```bash
source tools/env.sh
cargo test --locked --release --test frontend_quality_report -- --nocapture
cargo test --locked --release --test m4_shadow_validation --test m5_tier_c_bridge --test m3_m4_dff_yosys_differential -- --nocapture
```

量測工具：`tests/frontend_quality_report.rs`。每次完整通過後輸出 `target/frontend_quality_report.json`。保存的三次樣本：`docs/frontend-quality-2026-09-29.json`。

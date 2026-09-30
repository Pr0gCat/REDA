# 電路生成優化：密集通道壓縮（2026-09-30）

本輪保持前端與 lowered netlist 不變，縮短密集通道的平行軌道間距，降低實體佈線成本。

`compile()` 仍先嘗試 relaxation planner。需要通道布局時，先用 3 格軌道間距生成；候選必須通過原本全部實體驗證，拒絕時才用舊布局。`compile_legacy()` 維持 5 格間距，供獨立比較與既有 baseline 使用。DFF、pinned／fragment synthesis 入口維持原本流程。

平行紅石粉的隔離界線為座標距離 2，已有 `tests/spacing_derivation.rs` 的 simulator 實驗；本輪採距離 3，保留額外間隔。其階梯、分支與終端由完整電路的實體驗證及輸入轉換測試檢查。沒有調高搜尋上限，也沒有略過驗證。

生成時將實際軌道間距記錄在 `LegacyEmission`；`routing_stats` 使用相同間距重建幾何，避免讀錯座標或報錯延遲。新增回歸檢查，確認壓縮布局的佈線中繼器總數與世界中的實際數量一致。

## 量測

release；原生或 fresh Yosys → `lower_optimised` → `compile`。每個案例檢查全部 16×16 輸入轉換，數字為 simulator game ticks。

| 案例 | 方塊：前 → 後 | 非空氣包圍盒體積：前 → 後 | 最慢轉換：前 → 後 |
|---|---:|---:|---:|
| 原生 and4 | 228 → 228 | 3,528 → 3,528 | 28 → 28 |
| Yosys and4 | 214 → 214 | 2,880 → 2,880 | 22 → 22 |
| 原生 seven_segment | 8,980 → 7,408 | 100,408 → 70,416 | 52 → 46 |
| Yosys seven_segment | 10,088 → 8,588 | 213,816 → 164,892 | 84 → 76 |

原生七段顯示器的方塊減少 **17.5%**、體積減少 **29.9%**、最慢延遲減少 **11.5%**；閘數仍是 30。手寫 segment_a 的方塊由 6,416 降至 5,712；手寫 seven_segment 由 16,244 降至 13,872；手寫 and4 與 full_adder 維持 232 與 1,065。

三次重測的方塊、體積、延遲及世界指紋完全一致。兩個 seven_segment 的歷史 channel 世界指紋仍與優化前一致；and4 的預設世界指紋也不變。三次 `lower + compile` 時間中位數：原生 and4 12.71 ms、Yosys and4 10.86 ms、原生 seven_segment 155.80 ms、Yosys seven_segment 333.16 ms。計時只供報告，不參與候選選擇。

原始前測、三次後測、世界指紋與實作 SHA-256 存在 `generator-optimization-2026-09-30.json`。前測時序與實體 metrics 使用同一測試；既有 README 的 hybrid switchover 表格是歷史數字。

## 驗證

```bash
source tools/env.sh
cargo test --release --lib compile::routing_stats::tests
cargo test --release --test frontend_quality_report --test spacing_derivation \
  --test channel_safety --test reference_circuits --test seven_segment \
  --test mc_dump --test dff_physical --test verilog_frontend
cargo test --release --test compile_end_to_end --test frontend_selection
cargo check --lib --target wasm32-unknown-unknown
```

WASM 編譯、原生前端指紋，以及 host／WASM 各 6 個 placement parity 測試通過。

DFF 的上電、enable、存檔讀回與實體軌跡通過；七段顯示器的 CLI 仍輸出 47 個 lowered gates，方塊數更新為 8,588。尺寸回歸測試更新為本輪實際產物，獨立 baseline JSON、baked netlists 與 fragment synthesis producer 未修改。

本輪是預設 channel emitter 的優化，不代表 fragment synthesis 的已知大型案例品質問題或 Minecraft 客戶端認證已完成。

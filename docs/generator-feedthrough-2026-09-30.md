# 電路生成優化：跨列搜尋目標（2026-09-30）

基準 `bdae456`；同一前端與 lowered netlist，閘間距維持 14，軌道間距與共用間隔維持 3／2。

## 觀察與根因

前兩輪未採用的實驗，已記錄在 `generator-rejected-experiments-2026-09-30.md`，包括原始佈線／動態時序對照 `generator-pitch-timing-2026-09-30.json`。平行距離 2 的問題有獨立 simulator 重現；12 格閘距雖縮小體積，卻讓部分跨列幹線變長，因此撤回。

例如 `d1 → g11` 在閘距 14 → 12 的實驗中，橫向 Track 由 88 增至 148 格、Track 中繼器由 7 增至 12 個，縱向部分長度不變；`2→5` 的 `g11` 事件晚 8 ticks。這指向跨列位置的選擇。

`reserve_columns` 原先以當前來源 entry X 為搜尋目標，`reserve_feedthrough` 找最近合法位置。前一段已經占用的終端區間與下一段的終端位置都未參與目標計算。靠近 entry 不一定減少整條跨列幹線的成本。

## 通解

先計算當前 entry 與當前 sinks 的最小 X 區間 I，以及下一段 sinks 的 X 區間 J。將 entry 投影至 J，再投影回 I。兩區間相交時，這是最靠近 entry 的交集位置；不相交時，是 I 中最靠近 J 的端點。下一段没有 sinks 時保持既有目標。

此目標先避免擴大當前段的既有跨度，再向下一段終端靠近。既有 `reserve_feedthrough` 的避開閘體／列與間隔搜尋保持原樣。合法列受其他線限制，實際選中位置仍可能偏離目標，所以這是可量測的布局啟發式，並非對任意 netlist 的全域最優保證。

新規則只用於第一個 compact channel 候選；拒絕時回到 `bdae456` 的來源目標，再回到較寬共用間隔與歷史布局。每個候選仍執行全部原有實體驗證。`compile_legacy` 固定使用原來源目標，不改獨立基準。`LegacyEmission` 記錄實際規則，佈線統計按同一規則重建。

## 對照量測

release；四個來源／前端案例各檢查全部 16×16 輸入轉換。

| 案例 | 方塊：前 → 後 | 包圍盒體積：前 → 後 | 最慢穩定 ticks：前 → 後 |
|---|---:|---:|---:|
| 原生 and4 | 228 → 228 | 3,528 → 3,528 | 28 → 28 |
| Yosys and4 | 214 → 214 | 2,880 → 2,880 | 22 → 22 |
| 原生 seven_segment | 7,270 → 7,136 | 68,460 → 68,460 | 46 → 46 |
| Yosys seven_segment | 8,402 → 8,342 | 159,456 → 160,026 | 72 → 70 |

原生七段方塊减少 1.8%，体積與最慢時序維持；Yosys 七段方塊减少 0.7%、最慢時序改善 2.8%，**包圍盒體積增加 0.36%**。本輪接受此小幅包圍盒取捨，因實體方塊與時序都改善；沒有宣稱全部指標同時降低。手寫完整七段 13,758 → 13,488 格，手寫單段維持 5,664 格，and4／full_adder 維持原樣。

三次物理 metrics、世界指紋與完整時序報告相同。兩個七段的歷史布局指紋仍與 `bdae456` 一致；and4 世界指紋不變。原始前測、三次後測、時間與實作 SHA-256 在 `generator-feedthrough-2026-09-30.json`。計時不參與候選選擇；被拒絕時會增加一次生成／驗證成本。

## 驗證範圍

`source tools/env.sh` 後執行：

- `cargo test --release --lib compile::routing_stats::tests`：7 個通過，包含實際中繼器數量比對。
- `channel_safety`：5 個通過；`dff_physical`：1 個通過。
- `frontend_quality_report`：三次通過，保留原生既有時序門檻並收緊方塊門檻。
- `reference_circuits`、`mc_dump`、`spacing_derivation`、`seven_segment`、`compile_end_to_end`、`frontend_selection`：共 37 個通過。
- root WASM 編譯通過；viewer `frontend_compiler` 原生與 Node/WASM 各 2 個通過，包括實際完整世界指紋。

精確尺寸與世界指紋回歸更新為量測值；獨立 baseline JSON 與 baked netlists 未改。驗證涵蓋上述案例與既有通道安全測試，沒有宣稱整個測試庫或 Minecraft 客戶端認證通過。

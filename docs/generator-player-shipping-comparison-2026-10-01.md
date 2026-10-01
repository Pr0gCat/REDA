# 玩家矩陣與實際 shipping 生成器的完整對照

研究來源承接 [DECODE003 玩家解碼器](https://mattbatwings.github.io/lra/archives/DECODE003-Decoder-horizontal-to-horizontal-2-versions/) 與 [ENCODE003 玩家編碼矩陣](https://mattbatwings.github.io/lra/archives/ENCODE003-Encoder-horizontal-to-horizontal/)。實際 schematic 的座標、取訊號方向與原始波形見 `generator-player-design-analysis-2026-10-01.md`。已認證生成入口及先前與舊 torch-row 比較的結果見 `generator-player-matrix-certification-2026-10-01.md`。

本輪把比較對象改為真正的 `compile(&source)`。前一輪 4088 改善、8 持平的結果，適用於舊 torch-row 控制，不能移用為勝過 shipping 的結論。

## 獨立控制與邊界

`mixed_decoder_tap_probe --shipping-control` 使用 native frontend 編譯相同兩地址、三輸出 SystemVerilog，保留原始三個有序輸出標籤。shipping API 以唯一 signal 編譯，公開別名仍映回實際原始 driver／lamp 座標；候選則以 `compile_player_torch_row` 的有序來源邊界觀測。沒有為被消除的來源 gate 虛構座標。

全部 4096 個四列三位字表，每臂各 16 組 from/to、setup 與 target 共 32 階段。每個世界初始正常穩定，此後保留完整歷史；不加入 hold，不重置 burnout，不修改 simulator。每個世界兩次建構 fingerprint 相同；玩家候選每階段另核對所有實作圖 gate 的實體真值。

控制有 27 個字表至少一階段公開 driver／lamp 穩態錯誤。仍跑完全部階段、保存原始事件；這些控制不評為快速或改善，而獨立統計正確性恢復。其餘 4069 個正確控制，逐表逐項比較十二指標，不以方塊、體積、平均或綜合分數抵銷退步。這不是對一般 ROM／DFF 或 Minecraft 實機的全面結論。

## 已認證玩家矩陣的對照結果

| 項目 | 字表數 |
| --- | ---: |
| 玩家候選全部公開及內部真值正確 | 4096 |
| shipping 控制至少一階段錯誤、玩家候選正確 | 27 |
| 正確控制中改善且十二項無退步 | 2978 |
| 正確控制中全部持平 | 46 |
| 正確控制中至少一項退步 | 1045 |

1045 個退步案例均有總穩定時間增加；其中 356 個最差穩定時間增加、319 個 driver 最後變化時間增加、356 個 lamp 最後變化時間增加。毛刺與錯誤脈衝指標沒有增加。改善毛刺不能補償延遲退步，因此拒絕無條件替換 shipping；既有 `compile()` 選擇不變。

## 根因見證與通解實驗

字表編碼 8、`[0,1,0,0]`，只有 `a & ~b` 的第一輸出有效。shipping 的 target 總時間 71、矩陣 75 ticks；`0→1` 是 5→7、`1→0` 是 8→10。雖然矩陣消除 shipping `0→3` 的公開錯誤脈衝，仍不滿足延遲不退步。

編碼 9、`[1,1,0,0]`，第一輸出只是 `~b`，其他兩輸出固定低。shipping 的 `0↔1` 與 `2↔3` 只改無關輸入 `a`，target 時間全為 0。全地址列矩陣仍切換內部解碼，分別等 6 和 4 ticks。總時間 54→88；最差時間 21→10 是另一項改善，不能抵銷總時間增加。

提出通解：使用現有 Boolean cover 把各輸出合併成共享 cube 資料列，依 care mask 只配置真正相關的地址取訊號點，移除無用地址導體；較寬矩陣用同級兩個實際 buffer 分別供電，完整宣告共用 carrier 與端點。這不是按字表編碼選擇特例。

`~b` 的合併候選成功通過認證與完整階段真值，無關 `a` 切換降為 0；總時間改善至 68，但仍高於 shipping 的 54，不能宣稱此通解已通過全面品質驗收。

初版六列候選被 ExpandedStrict 拒絕：未緩衝的 lever 灰塵經合法階梯連回共用地址 carrier。抬高上游軌道後，編碼 3434 的 `[2,5,5,6]` 通過，但完整家族仍遇到宣告 contributor 無法連到 junction 的拒絕。完整 4096 表中有 12 表被拒絕認證，其餘 4084 表公開與實作圖內部真值正確。相較已認證玩家版，這 4084 表中 328 表改善、3756 表持平、0 表十二項退步；但不能忽略另 12 個認證失敗。因此本輪合併矩陣未取得完整家族認證，所有生產端實驗與驗證器改動已撤回。沒有把 contributor 加入宣告來掩蓋回灌，也沒有降低 checker。

完整拒絕 patch、掃描 raw、逐表品質與錯誤紀錄另存同前綴附件。拒絕 patch 以 `e1a8adf` 為基底，只供獨立 checkout 重現，不應套回正式生成器。失敗候選標為 uncertified，不以不完整階段或零時間評分。摘要 JSON 記錄精確掃描結果與檔案 SHA256。

## 重現

```sh
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --shipping-control > /tmp/reda-player-shipping-replay.jsonl
python3 tools/compare_shipping_matrix.py docs/generator-player-shipping-comparison-2026-10-01-controls.jsonl.gz /tmp/reda-player-shipping-replay.jsonl
```

兩個命令均預期 exit 1：producer 保存 27 個錯誤控制，comparator 拒絕 1045 個退步候選。exit 1 是實驗結果，不是可忽略的驗收錯誤。最終診斷工具重跑完整 4096 表，raw 與本輪凍結 raw 逐 byte 相同；comparison 品質 JSON 重跑也逐 byte 相同。此次最終生產 Rust 原始碼與 `e1a8adf` 相同，只提交診斷／分析工具與證據；前輪已記錄的 integration fixture 失敗未修改。

後續應先處理不需要完整地址解碼的函數：證明共享 cube 載體、分支取訊號與雙 buffer 隔離，再和固定同邊界控制逐項比較。不能按案例名稱排除退步表，也不能在 `CompiledCircuit` 中虛構被消除的來源 gate 觀測。

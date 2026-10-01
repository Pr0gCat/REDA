# 玩家矩陣轉成嚴格認證的實體生成入口

承接 `378923c` 的玩家 DECODE003／ENCODE003 衍生矩陣。
原型完整物理波形已改善，但缺少正式元件、路由及支撐交接描述，仍不能選入正式生成。
本輪新增明確的 `compile_player_torch_row(source, proof_work_limit)` 入口，
同樣只接收兩輸入、三個輸出宣告的組合函數。
它取得來源真值後建構具體實作，再通過來源邊界 rewrite、實作 instance graph 等價、
結構所有權、精確 emission、ExpandedStrict 物理驗證與來源有序邊界映射，才返回 sealed result。
既有 `compile_torch_row` 保留原世界；自動 `compile()` 尚未選用新入口。

## 找到的認證／生成問題與通解

舊轉接器把輸出落點固定在燈的北面，以舊行列座標分配剩餘灰塵。
玩家版輸出在燈的西面，正相 tap 的中繼器還從後方實心支撐上方的灰塵讀取，
並對前方支撐強充能後輸出到其下方灰塵。
先前簡單接法在真實認證中依序被拒絕為 NonConductingRouteCell、WrongRepeaterAxis、
缺少交接支撐及 JunctionContributorDoesNotReach。這些失敗沒有取得 certificate。

通解包含：

- 共用 typed world 轉接器，保留舊布局分支的完整記錄；新分支依實際灰塵連通區分配所有權。
- 新實作圖明確包含地址入口 buffer、正／反相 tap、字線 OR、資料火把、輸出 OR、常數高與實際常數低驅動。
  來源中的消失 gate 沒有虛構觀測座標；所有內部座標屬於 `implemented()`。
- `RepeaterHandover` 以 routed sink、primitive 身分及實際支撐描述入口弱充能與出口強充能。
  嚴格檢查宣告端點、輸入／輸出面、灰塵位置、實心支撐、延遲 owner、路徑首尾與重複宣告。
  支撐本身具有實體 owner 並參與 emission；路徑仍只接受灰塵／中繼器。
- 字線／位線保留完整分布式 junction 區域；資料火把取訊號的灰塵仍屬於該 junction。
  對這類接點，要求來源恰是該 junction、目標恰是實際 wall torch、終端恰在其實心支撐上方。
  所有既有 contributor 閉包、異域連通、耦合與結構驗證繼續執行。
- 無有用資料列時，刪除無邏輯負載的地址導體，保留真實輸入 lever 與支撐。

新增交接資料綁定 candidate fingerprint；空資料省略序列化，保留舊候選 identity。
ExpandedStrict 的 RouteContinuity 語義版本升為 2，LegacyCompatibility 的 descriptor 不變。
中繼器實體只有 primitive owner；max-plus timing 只對 route-owned 元件計路由延遲，
起點 primitive 的延遲仍屬於 primitive arc，沒有額外實體中繼器。

## 獨立完整驗證

凍結控制沿用上一輪 `generator-player-matrix-generation-2026-10-01-certified-controls.jsonl.gz`，
沒有重寫品質 caps、改 simulator、清除物理歷史或插入 hold。

4096 種四列三位字表，每個控制／候選先正常穩定，再遍歷 16 組 from/to 的 setup 和 target。
每臂各 65,536 個 target 及同數 setup，所有公開 driver、lamp 穩態正確。
新候選每個階段另按實作圖獨立計算所有 gate 真值，對照其實際灰塵／火把／中繼器觀測。
每個世界重建 fingerprint 相同，完整家族再跑一次，raw JSON 逐 byte 相同。

| 項目 | 凍結已認證控制 | 新已認證矩陣 |
| --- | ---: | ---: |
| 最差穩定時間，game ticks | 16 | 10 |
| target 穩定時間總和 | 608752 | 419784 |
| driver／lamp 毛刺、單輸入毛刺、錯誤脈衝 | 0 | 0 |

十二項品質逐表獨立比較，4088 種改善、8 個恆定表持平、0 種退步。
2744 個先前合格非恆定表的凍結九項 caps 也直接檢查全數通過。
沒有用體積、方塊數或綜合分數抵銷退步。

全部已認證控制 raw 記錄與原凍結控制完全相同。
相較上一輪未認證原型，新候選所有公開標籤的事件時間／值、穩定時間與最終值相同；
1500 個字表、11822 個階段的 raw 跨標籤事件順序不同，僅為同 tick 事件排序。
沒有把這項差異冒稱完整 raw 候選相同；新候選兩次重跑的完整 raw 確實相同。

## 回歸與明確限制

52 個相關 unit regression 通過：verification 15、candidate 10、realise 14、
新增玩家契約 2、timing graph 7、manifest 4。
新增拒絕測試涵蓋缺少入口／出口契約、錯誤支撐、偽造端點、重複契約、未知 routed sink、
錯誤 junction 取訊號落點；既有石頭路徑、反向／轉彎中繼器、異域 owner 與 contributor 測試通過。

decoder integration 共 18 項通過、1 項既有失敗。
失敗是 `decoded_rows_match_rom_truth_and_record_hazards_against_existing_generator` 第 197 行：
舊 one-input shipping fixture 要求最差 8 ticks，實測 10。
將未修改 `378923c` 以 `git archive` 匯出到獨立 `/tmp` checkout，使用獨立 target，
同一測試重現完全相同失敗；原斷言保留。兩份完整 log 已保存，不能宣稱整套全綠。
新增入口的 native／fresh Yosys 六組來源、alias 輸出、任意命名與反轉宣告順序、
錯誤來源／缺少輸出／不合法訊號／DFF／零 proof budget 拒絕檢查皆通過。
舊兩個完整 typed family 回歸也通過。

raw 家族、逐表品質、摘要 SHA256 與測試記錄保存為本文件同前綴附件。
下一輪須與目前 shipping `compile()` 的獨立實體品質對照，處理選擇與受支持邊界，
再接入自動生成。此處結果只適用兩輸入三輸出組合家族；尚未宣稱更大 ROM、一般 mux、
DFF 或 Minecraft 實機全面改善。

```sh
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --player-typed > /tmp/reda-player-typed-replay.jsonl
python3 tools/compare_matrix_probe.py docs/generator-player-matrix-generation-2026-10-01-certified-controls.jsonl.gz /tmp/reda-player-typed-replay.jsonl
RUST_MIN_STACK=33554432 cargo test --release --lib compile::verification::tests
RUST_MIN_STACK=33554432 cargo test --release --lib compile::fragment_synth::torch_row::player::tests
RUST_MIN_STACK=33554432 cargo test --release --test torch_row_decoder player_matrix_certifies_native_and_fresh_yosys_sources
```

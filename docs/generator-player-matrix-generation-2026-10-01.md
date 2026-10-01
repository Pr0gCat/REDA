# 玩家結構轉成通用生成方法：完整家族實驗

本輪由上一輪已下載並實測的 DECODE003 三格入口 stencil，建立兩位地址、三位資料的通用矩陣原型。
輸入是四個三位字，不以模組、gate 或案例名稱決定幾何。
正式生成器、模擬器及既有品質上限未修改；原型仍標示 `certified:false`。
玩家原始接線與來源見 `generator-player-design-analysis-2026-10-01.md`。

## 問題、原因與通解

上一輪的灰塵回灌已由玩家的中繼器入口與上層跨越方式排除。
本輪第一個生成版本在全 4096 字表均得到正確公開輸出，卻有 1950 表品質退步。

字表 `[0,0,0,1]` 的單輸入切換 `0→2` 提供具體見證：
bit 1 的 mismatch 火把 `(11,4,5)` 在 tick 2 熄滅；
bit 0 沒變，但其火把 `(11,4,3)` 到 tick 4 才亮。
中間資料火把 `(12,2,10)` 誤亮，driver 在 tick 4–6 錯高，lamp 在 tick 5–10 錯亮。

幾何原因是 bit 1 的火把 `(11,4,5)` 與 bit 0 的支撐 `(11,4,4)` 相鄰，
會充能到另一支火把的支撐。這不是獨立的兩個 literal tap，會產生額外級聯延遲。
把地址線間距由兩格改成三格，禁止不同 tap 直接充能到彼此支撐，
同一 `0→2` 不再改變 bit 0 的火把，也不再出現公開錯誤脈衝。
這是幾何與完整波形對照，不是修改火把更新順序來消除毛刺。

三格版本仍有 58 表退步。[0,0,0,1] 的另一個 `2→1`，
bit 1 mismatch 火把到 tick 20 才恢復，driver 錯高 18 ticks，lamp 錯亮 21 ticks。
完整內部波形可觀察到部分火把在 60 game ticks 內切換九次，
超過現有 `is_burned_out` 的八次上限；快速切換歷史已成為另一個限制。
此記錄沒有讀取或重置私有火把歷史；觀察到的切換本身已足以驗證超過門檻。

單純移除零資料列、以固定高驅動取代全高輸出，雖改善常數表，
但退步表反而增加到 74。故不能因刪除邏輯或減少方塊就接受。

最後版本保留三格隔離與上述函數化刪除，再為有實際資料列的兩条地址線
各放一個 delay=1 中繼器，對所有 literal 提供相同的入口級。
這是候選世界內真實的延遲元件：每個字表照常跑完整實體模擬，
不插入測試 hold、不重置歷史、不改 burnout 語義。
全常數函數沒有解碼 tap，因此不放無用入口級；全高輸出使用獨立的真實火把驅動。

## 固定對照與結果

第一份對照是上一輪已提交的
`generator-player-design-analysis-2026-10-01-rejected-matrix.jsonl.gz` 中完整的四列 control。
每個版本重建此 arm，逐表比對 fingerprint、32 階段狀態、ticks 與六個公開標籤的完整事件，
均與凍結記錄完全相同；沒有重新產生舊上限或覆寫基準。

所有版本皆完整掃描 4096 字表、16 組 from/to，每組先 setup 再 target。
每個 arm 有 65536 個 target 與同樣數目的 setup，保留同一 simulator 的實體歷史。
driver 與 lamp 的預期值都從原始字表直接計算，不從物理輸出反推。

| 實驗版本 | 穩態正確字表 | 任一品質指標退步 | 改善且無退步 | 決策 |
| --- | ---: | ---: | ---: | --- |
| 玩家入口、地址線間距 2 | 4096 | 1950 | 2146 | 拒絕 |
| 間距 3 | 4096 | 58 | 4038 | 拒絕 |
| 間距 3＋刪除零資料列／全高輸出折疊 | 4096 | 74 | 4022 | 拒絕 |
| 上述結構＋有用解碼入口中繼器 | 4096 | 0 | 4096 | 原型對照通過 |

通過研究用 control 不代表勝過現有合格入口。因此另補測實際
`compile_torch_row`，從 native frontend 的四字輸出函數取得來源 netlist，
保留三個公開輸出宣告，包括 aliases。每次建構使用現有來源等價與完整物理認證入口，
取 sealed 結果的公開輸入、driver 與 lamp 座標；沒有假造被消除的來源中間節點。
此 control 的每表重建兩次，且全部掃描另完整重跑；世界與波形完全一致。

補測保留原本的九項指標定義，並另外逐標籤記錄最長脈衝等十二項完整對照。
首次補測遇到診斷工具誤將所有 driver 都讀成灰塵 `power`：
字表 [1,0,0,0] 的實際燈已亮，火把 driver 被誤讀成 false。
已依方塊型別改讀灰塵 power 或火把／中繼器 lit，完整重跑才取得以下結果。
這是新診斷工具的修正；沒有改動舊 controls、舊測試或模擬器。

新十二項補測的 control 總穩定時間仍為 **608752**，符合舊 production-entry 記錄。
逐表九項 control 值也保持全部 **2744** 個既有非恆定字表的獨立上限。
新增完整原始波形 profile 凍結成單獨檔案，沒有取代舊指標檔。

| 對照已認證 torch-row 入口 | Control | 新原型 |
| --- | ---: | ---: |
| 最差穩定時間，game ticks | 16 | 10 |
| 全家族 target 總穩定時間 | 608752 | 419784 |
| driver／lamp 毛刺 | 0／0 | 0／0 |
| driver／lamp 錯誤脈衝時間 | 0／0 | 0／0 |

**4088** 個非恆定字表至少一項改善且其餘均無退步；八個恆定字表完全持平。
十二指標分別為最差／總穩定時間，driver/lamp 毛刺，driver/lamp 單輸入毛刺，
driver/lamp 最後變化時間，driver/lamp 總及最長錯誤脈衝。
沒有用時間改善抵銷毛刺退步，也沒有方塊數或體積排名。
事件以每個公開標籤計算，不把同位置的別名註冊重複算成毛刺。

## 局部隔離與決定性檢查

最終原型建構另外檢查：

- 兩個原始地址域、每個有用字線與三個輸出位線不得經任意灰塵平面／階梯連通。
  走訪既有 `dust_connections` 全部鄰接結果，不能只檢查路徑標記或 owner 名稱。
- 每支火把在亮起時，不得直接充能到另一支 tap／資料火把的支撐。
- 每個候選重建的完整世界 fingerprint 相同。

加上檢查後再次完整跑已認證 control 與新原型，4096 表全數通過，
完整 JSON 與加入檢查前逐 byte 相同。這些是必要的局部條件，
**不是**完整 primitive、route、延遲 owner、輸出邊界與來源等價的物理 certificate。
新原型的 raw 世界與所有 setup/target 波形，在兩種 control profile 下也完全相同。

## 重現與剩餘工作

```sh
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --player-matrix
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --player-isolated
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --player-pruned
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --player-restored
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --certified-control > /tmp/reda-player-matrix-replay.jsonl
python3 tools/compare_matrix_probe.py docs/generator-player-matrix-generation-2026-10-01-certified-controls.jsonl.gz /tmp/reda-player-matrix-replay.jsonl
```

指定 `512 513 --internal` 可重現上述內部切換見證。
原型程式的 exit 0 只表示穩態檢查完成；`compare_matrix_probe.py` 才逐項判斷品質，
任一字表錯誤或任一指標退步會 exit 1，不把失敗版本重新分類成通過。
五份完整原始家族記錄、逐表十二項比較及內部波形均已保存，summary 含原始 SHA256。
舊單點 probe 與上一輪回灌見證重跑一致；格式與 diff 檢查通過。

下一步是將此函數化結構接成正式 typed candidate：
完整描述入口 buffer、正相／反相 tap、字線 junction、資料火把及各輸出驅動，
尤其不能把目前零輸出的未充能位線冒充已認證的常數驅動。
必須通過現有來源邊界等價與 ExpandedStrict 物理認證，並保留同一凍結控制與十二項品質。
再完成不同來源表示／命名／宣告順序與適當正式生成回歸，才可接入正式選擇。
本輪不宣稱 stock compile()、一般 mux、更大 ROM 或 Minecraft 實機已改善。

# 玩家矩陣的函數資料列合併：完整認證與品質改善

承接 `ac0a727` 的固定 shipping／玩家候選對照。上一輪合併原型有 12 表被認證器拒絕，所有生產端實驗撤回。本輪從這些具體失敗修正生成規則，更新既有 `compile_player_torch_row`；邊界仍為兩輸入、三個有序輸出宣告的組合函數，不增加另一路公開編譯 API。

## 觀察、根因、通解

上一版按完整地址生成資料列。輸出不依賴某地址位時，切換該位仍啟動內部解碼，增加實際穩定時間。通解重用 `frontend::cover::cover`：每個輸出求正相 cube cover，依 `(bits, care_mask)` 合併相同 cube 的資料遮罩。只保留真正相關的地址取訊號點與地址導體；單 literal 列直接使用取訊號 primitive，多 literal 列才建立 OR。恆定高／低仍有實際火把驅動。沒有按字表編碼、名稱或來源 gate 排除案例。

最多六列的覆蓋可能比原四地址列更寬。六列情形以同級兩個真正的 buffer 驅動共享地址 carrier，保留上游灰塵隔離的跨層幾何。額外 buffer 具有實作圖節點、primitive owner 與實際觀測；沒有把物理延遲藏在沒有 owner 的導體。輸出從位線中部取樣並接實際 lamp，避免較寬位線的訊號衰減。

12 個前次認證失敗的根因是觀測點。當最後一列需要正相 repeater tap，跨線灰塵的支撐把 `(18,5,z)` 覆蓋成實心方塊；實作圖卻把這裡當成共用 OR 的訊號觀測。因此 contributor 無法到達宣告的 junction。修正為最後跨線前的真實灰塵 `(source_x-5,5,z)`；較寬家族全部 48 種都逐一檢查觀測方塊和完整認證。沒有將實心方塊偽裝成導體或擴大可接受 block 分類。

另一個認證問題是共用字線中的取訊號終端：emission 的 terminal role 覆蓋 junction role，反向 contributor 遍歷原先會在 outbound tap 截斷。現在只在明確屬於 carrier 的 tap 繼續依真正有向導電關係遍歷，外部支線仍截斷。carrier tap 必須檢查所有實體前驅，包括未宣告的外來驅動；新增直接接上 foreign lever 的拒絕回歸。incoming landing 可穿過宣告 carrier，仍不沿上游 route 跨出契約。所有 route continuity、junction closure、跨 owner 連通、耦合、精確 emission、source rewrite／instance graph 等價維持必須通過。

ExpandedStrict Connectivity 語義版本 3→4；LegacyCompatibility descriptor 不變。共用 typed world 轉接器依實際目標選 primary lever 的直接 repeater 或灰塵分支，並區分 repeater 的直接灰塵出口與強充能支撐出口；lamp 從實際相鄰灰塵接線。舊 `compile_torch_row` 的布局分支保留。

## 固定對照實驗

控制直接讀取上一輪凍結 `generator-player-shipping-comparison-2026-10-01-family.jsonl.gz`，以其中已認證玩家 arm 作為品質控制；原 shipping arm 同時逐紀錄核對完全相同。沒有重建品質 caps 或控制波形。

4096 表，每臂每表 16 組 from/to，各 setup+target 共 32 階段；初始正常穩定後保留全部物理歷史，不加入 hold 或重置 burnout。所有公開 driver、lamp 與每個 implemented gate 的實體真值都正確；每個世界兩次生成 fingerprint 相同。每個新候選均由既有入口取得 sealed rewrite，通過來源邊界、實作圖與 ExpandedStrict 實體認證。

| 與已認證玩家版比較 | 結果 |
| --- | ---: |
| 完整家族正確／認證 | 4096 / 4096 |
| 十二指標改善且無退步 | 328 表 |
| 十二指標持平 | 3768 表 |
| 任一指標退步 | 0 表 |
| 最差穩定時間 | 10 → 10 game ticks |
| target 總穩定時間 | 419784 → 416536 game ticks |

逐表十二指標仍獨立比較；公開 driver／lamp 毛刺與錯誤脈衝均為零。另對既有 `generator-torch-row-prior-nonconstant-caps-2026-09-30.json` 的 2744 表九項 caps 直接檢查，全部保持。沒有以方塊數、體積或綜合評分驗收。

`[1,1,0,0]`、只有第一輸出 `~b` 的見證：無關 `a` 切換的 target 時間由 6／4 降為 0，該表總時間 88→68。與真正 shipping 的 54 比仍較慢，因此此改善不代表能全面替換 shipping。shipping 的錯誤控制仍獨立記錄，不能把不正確控制評為快速。

## 回歸、決定性與限制

完整重播、逐表品質 JSON、測試 logs、精確版本與 SHA256 保存同前綴附件及 summary。最終正式入口重播完整家族，raw JSON 逐 byte 相同；與修正觀測點後實驗的完整波形也逐 byte 相同。

53 個相關 unit regression：verification 15、realise 14、candidate 10、timing graph 7、manifest 4、玩家契約 3。新增覆蓋全部 48 個六列 cover，既有交接偽造、漏 contributor、反向 repeater、石頭路徑、跨 owner 與耦合拒絕測試保留。native／fresh Yosys 來源測試加入前次失敗的 `[2,7,7,1]` 及需雙 buffer 的 `[2,5,5,6]`。來源重新命名、反轉宣告、alias、錯誤來源、DFF、不合法來源與 proof budget 拒絕維持驗證。

完整 decoder integration 18 項通過、1 項失敗；保留前輪已於未修改基底重現的 fixture 失敗：`decoded_rows_match_rom_truth_and_record_hazards_against_existing_generator` 第 197 行要求最差 8 ticks，實際 10。斷言未改；不宣稱整套全綠。

與實際 shipping 的固定對照：27 表恢復正確、2979 表改善且無退步、46 表持平、1044 表退步。自動 `compile()` 未選用此入口，受支持家族未擴大。本輪合併法改善既有玩家式生成，但仍須繼續處理其相對 shipping 的延遲退步及自動選擇的來源觀測契約。

```sh
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --shipping-control > /tmp/reda-player-cover-replay.jsonl
python3 tools/compare_shipping_matrix.py docs/generator-player-shipping-comparison-2026-10-01-family.jsonl.gz /tmp/reda-player-cover-replay.jsonl --candidate-control
python3 tools/compare_shipping_matrix.py docs/generator-player-shipping-comparison-2026-10-01-controls.jsonl.gz /tmp/reda-player-cover-replay.jsonl
RUST_MIN_STACK=33554432 cargo test --release --lib compile::fragment_synth::torch_row::player::tests
RUST_MIN_STACK=33554432 cargo test --release --lib compile::verification::tests
RUST_MIN_STACK=33554432 cargo test --release --test torch_row_decoder
```

producer 的 exit 1 是因原 shipping 控制的 27 表錯誤，候選全數正確；`--candidate-control` 品質驗收須 exit 0。shipping 品質比較與完整 decoder suite 仍預期 exit 1，精確結果在 summary 記錄。

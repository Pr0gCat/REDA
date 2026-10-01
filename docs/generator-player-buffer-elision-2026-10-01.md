# 入口 buffer 的結構化省略：先拒絕，再驗證通解

基準為 `143a40c` 的已認證函數資料列矩陣，控制直接讀取 `generator-player-cover-certification-2026-10-01-family.jsonl.gz` 的 candidate arm。原 shipping arm 仍逐紀錄比對，不重建控制或修改品質 caps。自動 `compile()` 尚未選用玩家入口。

## 觀察與第一次實驗：拒絕

短矩陣的地址入口 buffer、取訊號 primitive、資料火把構成多級路徑。先測試：未緩衝訊號仍可到達全部實際取訊號點時，刪除兩個入口 buffer，實作圖同時改接真實 primary input。較寬矩陣維持 buffer。

兩個見證 `[0,1,0,0]` 與 `[1,1,0,0]` 的總穩定時間分別 75→51、68→52 ticks。但完整 4096 表雖公開與實作圖真值全正確，有 155 表至少一項品質退步。3573 表改善不能抵銷這些退步，第一版全部撤回，原 patch 與完整拒絕 raw／品質保存。

`[1,1,1,0]` 的 `2→1` target，預期第一輸出持續為高：入口刪除後，兩個取訊號 repeater 都在 tick 2 改變，第一資料火把在 tick 4 熄滅，另一資料火把直到 tick 6 才亮，driver 產生 2-tick 錯低脈衝；lamp 雖沒有錯誤，仍不能通過。原 buffer 版本兩個資料火把都在 tick 6 改變，driver 沒有脈衝。

完整內部觀測定位了此例的歷史根因。以初始化完成為時間 0，另一資料火把先前在 `[12,16,20,28,41,45,49,62,66]` 改變；target 起點 66，正常應於 70 亮起。既有 burnout 判定 `count(at > now - 60) > 8` 在 70 得到 9，阻止亮起；72 時得到 8，才恢復，因此實際亮起延後 2 ticks。依 `component.rs` 及 `apply_torch_tick` 原有判定與完整事件重建；沒有清除歷史、插入等待、修改排程或火把語義。不能只按組合邏輯深度決定 buffer 是否多餘。

另一例 `[0,0,0,1]` 刪入口後最差 10→26 ticks，driver 最長錯誤脈衝 18、lamp 21 ticks。該例的全部內部歷史未逐一作因果診斷，沒有把所有 155 個反例冒稱同一個已確認原因。

## 第二次實驗：按分支結構保護時序

`needs_input_buffer` 直接檢查生成圖的 cube literal 與資料遮罩：只要有反相火把取訊號，或同一輸出有多個資料火把合流，就保留原入口 buffer。只有 repeater-only 控制、每個輸出單一資料火把的路徑才省略。

這種兩輸入三輸出家族最多三個共享資料列，既有輸入位置仍能供電至全部實際 tap；不用按案例編碼或名字挑選，也不按方塊數排名。省略後沒有虛構 Buf 節點或 gate 座標，primary input 的實際灰塵與 primitive input 路由由既有轉接器、端點契約及 ExpandedStrict 驗證。

完整家族仍是 4096 表；全部通過來源邊界 rewrite、instance graph 等價、結構所有權、精確 emission 與嚴格實體認證。每表 16 組 from/to，各 setup+target 共 32 階段，保留歷史；每階段核對全部公開 driver／lamp 與 implemented gate 真值。每個世界兩次生成 fingerprint 相同。

| 與凍結玩家版比較 | 結果 |
| --- | ---: |
| 公開與實作圖真值正確 | 4096 / 4096 |
| 十二項改善且無退步 | 117 表 |
| 十二項全部持平 | 3979 表 |
| 任一項退步 | 0 表 |
| target 穩定時間總和 | 416536 → 414032 game ticks |
| 家族最差穩定時間 | 10 → 10 game ticks |

所有公開毛刺與錯誤脈衝仍為 0。3979 個未改善候選的完整 raw 記錄亦與基準相同，非僅指標持平。另對 2744 表原始九項 frozen caps 直接檢查，全部維持；沒有重生 caps 或綜合評分。正式入口完整重播與首次完整掃描逐 byte 相同。另按實際前一階段地址檢查 setup 的十二項波形指標，全 4096 表亦無退步、driver／lamp 毛刺為零；這是額外檢查，沒有改寫原 target 驗收或 caps。

與真正 shipping 的同邊界固定對照：27 表恢復正確、3023 表改善且無退步、27 表持平、1019 表仍至少一項退步。這版沒有全面替換 shipping。

## 回歸與重現

本輪 4 個玩家契約 unit regression 通過，包括新增保留 torch／合流時序保護、真實 primary input 路由；所有 48 個較寬矩陣仍認證通過。fresh native／Yosys 來源 integration 加入單一路徑及三個共享 repeater-only 資料列，全部 10 組來源通過；重新命名、反轉宣告、alias、DFF／不合法來源／proof budget 的既有 recognition integration 亦通過。

本輪針對 private 玩家生成變更驗證上述回歸，沒有再次跑完整 decoder suite。前輪已獨立重現的 fixture 失敗仍保持：第 197 行要求 shipping 最差 8 ticks，實際 10。沒有修改該斷言；不宣稱全套全綠。

```sh
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --shipping-control > /tmp/reda-player-buffer-elision.jsonl
python3 tools/compare_shipping_matrix.py docs/generator-player-cover-certification-2026-10-01-family.jsonl.gz /tmp/reda-player-buffer-elision.jsonl --candidate-control
RUST_MIN_STACK=33554432 cargo test --release --lib compile::fragment_synth::torch_row::player::tests
RUST_MIN_STACK=33554432 cargo test --release --test torch_row_decoder player_matrix_certifies_native_and_fresh_yosys_sources
RUST_MIN_STACK=33554432 cargo test --release --test torch_row_decoder torch_row_recognition_uses_functions_and_refuses_invalid_or_unqualified_sources
```

producer 因原 shipping 控制的 27 表錯誤預期 exit 1；玩家品質驗收 exit 0。失敗 patch 以 `143a40c` 為基底，只供獨立 checkout 重現；原內部 witness 比較命令附 `73 74 --internal`。原始掃描、品質、內部事件、burnout 重建、測試與 SHA256 另存同前綴附件。

後續仍需降低正相函數的兩級反相，以及處理其他共享資料路徑的歷史時序；不能全面移除 buffer，也不能掩蓋被燈延遲濾掉的 driver 脈衝。

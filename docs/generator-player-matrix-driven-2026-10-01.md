# 玩家矩陣：常數零輸出的實際驅動

承接 `4d8d8f3` 的玩家 DECODE003／ENCODE003 結構研究與全家族對照。
原型零輸出原本只有未充能灰塵，不能作為正式 typed candidate 的常數驅動。
新增獨立 `--player-driven` 實驗模式，其他凍結模式不變；正式生成器與模擬器未修改。

對每個在四個資料字皆為零的輸出位，生成 NOR0 → NOR1 實際火把對。
以該位線 z 座標表示：高火把 `(15,4,z)` 朝 north，支撐 `(15,4,z+1)`；
下方灰塵 `(15,3,z)` 充能支撐 `(15,2,z)`，低火把 `(16,2,z)` 朝 east，
其下方既有輸出灰塵 `(16,1,z)` 接燈 `(17,1,z)`。
這是既有常數零驅動幾何的旋轉，啟用條件僅由資料語義決定。

## 完整實驗

- 全部 4096 個兩輸入三輸出函數，每個世界重建 fingerprint 相同。
- 每個候選／控制各執行 16 組 from/to 的 setup、target，共 32 階段；保留實體歷史。
- 所有 driver、lamp 穩態皆與獨立字表預期相同。
- 721 個候選世界實際改變，但全部 4096 個候選的 32 階段公開事件、值、穩定時間
  與上一輪 `certified.jsonl.gz` 候選完全相同。
- 每個已認證控制完整 raw 記錄與獨立凍結控制相同。
- 對控制的十二項品質逐項比較：4088 個改善、8 個持平、0 個退步。
  因波形完全相同，前輪最差穩定時間 16→10 game ticks、總 target 穩定時間
  608752→419784 的結果不變；不採方塊數、體積或抵銷指標。
- 灰塵域連通與直接火把異域支撐充能檢查，全家族通過。

原始全家族波形及逐表品質保存為同前綴 `.jsonl.gz`、`-quality.json.gz`，
摘要含原始 SHA256。候選仍為 `certified:false`，局部隔離與模擬不能替代完整證明。
下一步仍需建立實際元件／junction 圖、入口弱充能路由、輸出落點與延遲 owner，
通過既有來源等價及 ExpandedStrict 認證，再考慮正式生成選擇。

```sh
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --player-driven > /tmp/reda-player-driven.jsonl
python3 tools/compare_matrix_probe.py docs/generator-player-matrix-generation-2026-10-01-certified-controls.jsonl.gz /tmp/reda-player-driven.jsonl
```

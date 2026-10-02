# 大型 decoder 的平直刷新認證修復

本輪先解決 seven segment 無法沿完整 manifest 嚴格認證的問題。新 `compile_flat_refresh_candidate()` 是可認證候選入口，尚未改變預設 `compile()` 的選擇策略。品質持平，不宣稱延遲或毛刺改善；八項目標仍未完成。

## 觀察、原因、通解

舊生成中的路徑包含內部 repeater 直接接斜上 dust。Native 原始例為 `(76,1,68)` dust → `(76,1,67)` repeater → `(76,2,66)` dust，三個分支共用同一個 relay。舊電路透過 repeater 前方支撐方塊強供電運作，但 manifest 的嚴格 RouteContinuity 不允許以此當作普通內部平直刷新。沒有修改 verifier 或 simulator。

通解依路徑幾何與 ownership：將階梯前的內部 repeater 移到前一個同路由 dust 位置，使 repeater 前後都平直，原 repeater 位置改為 dust，之後才爬樓梯。只有前兩格同層、固定 facing 能沿前格平直穿過、兩位置非觀測點、且每個分支包含兩格的情況才嘗試移動。保留所有路徑、分支、terminal refresh counts、gate observations 及原始世界 bounds；候選仍須重新完成來源等價、結構、ownership、完整電氣認證。

`flatten_refreshes()` 只修改未認證 draft；`compile_flat_refresh_candidate()` 使用原編譯器保留的實際 manifest，沿相同既有流程重新認證。它不依函數名稱／編號，也不以方塊數或體積選擇。若候選仍不合格，返回錯誤。它沒有聲稱所有結構都能靠一次前移修好。

## 實驗與獨立驗證

Native 與 fresh Yosys 的完整 seven segment 都生成成功並通過嚴格認證。每個 run 從同一世界保留 simulator 歷史，執行 16×16 組 setup/target，共 512 階段；只清除 observer 記錄。每階段驗證所有實作 gate 和全部 7 個 driver/lamp。

- Native：最差 target 44 ticks、target 總8823、setup 總8786、lamp 毛刺切換85/256，全部 setup/target 指標與固定控制相同。
- fresh Yosys：全部 setup/target 指標與固定控制相同。
- 更強的核對：兩個 frontend 的完整階段記錄（含 ticks、全部事件、實際內部訊號及 driver/lamp）均與各自固定控制逐字相同。
- 各 frontend 三次完整候選生成 bytes 相同，包含完整 BlockState／world bounds 指紋。
- `tools/compare_cover_probe.py` 以記錄的 source/implementation graphs 獨立重新計算每階段真值、重新計算 setup/target 各12項核心品質與2項群組毛刺數。它拒絕錯誤 protocol、internal/lamp 值、事件及缺失 certification；不是只接受 producer 的 correct 標籤。
- 實際 manifest 的 regression test 驗證前移一次、再次不再改動、observations/branch paths/counts 不變、嚴格認證、原始 bounds、sealed view、equivalence、world partition；1 項通過。physical rewrite views 6、selection 3 通過。

原始世界的 frozen 控制只有功能／波形證據，本身未取得此新嚴格認證；控制與候選的狀態在資料中明確區分。控制 Native／fresh Yosys 的原始 world fingerprints 分別仍是 `7c6c51d831ca72e249d590fd11b7306bdddd1c1f8f76c73bac13ada0e6cb39d4`／`9839637a3ae76d2c45fc50def2b4d21d984ff88fc85beca5f4593ebcb7dbce63`，與不可變 checkpoint 一致。沒有以新候選重製控制。

## 拒絕的共享 cover

同一 probe 依真值表／既有 consensus cover 建立跨輸出共用 cube，嘗試 polarity mask 0、1。平直刷新後兩候選雖通過嚴格認證與內部真值，但品質退步：mask 0 最差46 ticks、lamp 毛刺117/256；mask 1 最差66 ticks、lamp 毛刺120/256。它們未加入預設選擇。只減少邏輯重複或通過真值不足以驗收；下一輪需處理物理到達時序與 fanout 路徑。

同名 `-native-control`、`-native-candidates`、`-yosys-control`、`-yosys-candidate` 的 `.jsonl.gz` 保留全部圖、完整階段及實際內部值；`-summary.json` 記錄所有逐項比較與 SHA-256。上一輪首次嚴格拒絕的細節在 `generator-large-cover-rejected-2026-10-02.json`。

## 重現

```sh
source tools/env.sh
export RUST_MIN_STACK=134217728
cargo run --release --example rom_cover_search -- tests/fixtures/seven_segment.sv seven_segment 0 --control > /tmp/control.jsonl
cargo run --release --example rom_cover_search -- tests/fixtures/seven_segment.sv seven_segment 2 --flat-refresh > /tmp/candidate.jsonl
python3 tools/compare_cover_probe.py /tmp/control.jsonl /tmp/candidate.jsonl
cargo test --release --test flat_refresh --test physical_rewrite_views
```

加入 `--yosys` 可取 fresh Yosys oracle；原生入口不需要它。固定控制重現只供核對，不得覆寫 frozen control。probe 使用最多6個輸入的完整真值表，不是一般大型電路的可擴展搜尋方案；通用組合、預設入口、較大 ROM/Mux 和其他 HDL／循序功能仍待後續完成。沒有 Minecraft server 驗證結果。

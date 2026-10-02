# REDA 全部剩餘項目完成目標

使用者於 2026-10-02 要求完成上輪列出的全部八項。目標保持 active，任何單一輪或小型家族通過都不能代替全部完成。每輪沿「觀察實際電路／波形 → 根因 → 通解 → 獨立實驗 → 通過則提交」。提交但不推送，保留使用者其他未提交修改。

| 項目 | 完成條件 | 起始狀態 |
|---|---|---|
| 大型生成 | 完整 seven segment 及較大 ROM/Mux 使用結構通解生成、嚴格認證，整合預設入口並保持來源／實作消費端契約 | 未完成；新 matrix 限兩輸入／三輸出 |
| 大型品質 | 固定來源與獨立世界控制，連續 setup/target 每項延遲、driver/lamp 毛刺及錯值脈衝不退步且有實測改善，完整轉換、內部訊號與公開輸出真值通過 | 原生 seven segment 44 ticks、85/256 輸出毛刺切換；尚待新改善 |
| 通用候選 | 依結構生成／組合與選擇候選，沒有名稱／函數編號白名單；搜尋預算有實際作用，重複生成決定性 | 預設 bounded selector 有兩候選；fragment API 尚無 proposal loop |
| 循序支援 | reset、多時鐘、負緣、回授 FSM/計數器具明確來源語意、實體生成、power-on/edge/hold/reset 狀態軌跡驗證 | 已有單時鐘正緣 feed-forward DFF/enable；其他限制仍在 parser/elaborator |
| 原生 HDL | parameters、模組實例、inout、legacy reg/always 等缺口具可綜合子集語意、lowering、實體生成與獨立差分驗證；不可把明確拒絕當支援 | 尚未支援 |
| Yosys 可移除依賴 | 上述功能及既有支援能原生執行；Yosys 不存在的環境驗收通過，僅保留可選 oracle | 部分 unsupported features 仍依 fallback |
| 真實遊戲驗證 | 真實目標 Minecraft Java server 上取得新方法及代表組合／循序電路可重現 conformance；記錄版本、完整結果與限制 | 最近測試為 simulator/strict proof/mock RCON，尚無新方法 server 結果 |
| 全專案收斂 | root/viewer、品質門檻、clippy、WASM build/runtime、conformance 與相關工具實際通過；README 與契約同步 | 未宣稱全綠；需重新全量盤點 |

## 不可降低的驗收

不以閘數、方塊數或體積作品質接受條件。不修改 simulator／物理驗證規則或清除歷史以通過，不新增等待階段掩蓋問題，不刪測試、增加 skip 或改寫獨立控制以接納退步。保留兩輸入／三輸出全部 4096 函數及 2744 個既有上限；擴展家族另建不可變 controls。每輪完整波形與完整世界指紋重複三次一致。遊戲結果不得以 mock 或 simulator 代替。

## 執行順序

先建立全量測試與 seven segment 的固定對照，研究多條件共享解碼與匯合時序，取得第一個大型通解。同步盤點循序／HDL、工具及遊戲環境；依相依性逐項完成，不能因遊戲環境缺少而停止其他可獨立工作。各輪記錄原因、實作、拒絕實驗、指標、回歸、提交和限制。

## 已確認的起始證據

- HEAD `387a646`：bounded default selector，4096 正確、27 改善／4069 持平／0 退步，三次全量相同。
- 當前 seven segment 品質 probe 重跑：Native 32 lowered gates、Legacy planner，最差44、平均34.46484375 ticks，輸出毛刺85／256、單輸入毛刺0；fresh Yosys 最差64、平均48.05078125、輸出毛刺141／256、單輸入毛刺17。世界指紋與固定 checkpoint 相同。
- `examples/rom_family_physical_probe.rs` 的現有未提交修改不屬於本輪，保留並排除提交。

## 第一輪盤點與拒絕實驗

全量 baseline 已終止：root lib 1165 passed、15 failed、71 ignored，exit 101；cargo 未繼續 integration tests。因此這次不能宣稱完整 root suite 已跑完。具體失敗名稱保存在 `reda-completion-baseline-2026-10-02.json`，包含 route parity、keep-out、供電診斷及 resettle 等；不能把舊測試的失敗一律當成過時而放寬。

大型 cover 實驗使用輸入真值表及既有 consensus cover，跨輸出共享相同 cube，嘗試正向及混合輸出極性。原始 Native seven segment 與第一個兩候選皆在嚴格 RouteContinuity 拒絕：原始例為 RouteId(2)、sink ordinal 3，(76,1,67) → (76,2,66)。既有功能測試不能代替此認證；沒有將任何未認證候選整合預設，也沒有更改 verifier/simulator。下一步追 source endpoint 的 repeater／支撐交接，依認證允許的幾何生成，而不是修改認證接受條件。

實驗另外暴露一般入口的錯誤：大於三個輸入的 Or 被 admitted 為 realisable，兩個 placer 失敗後 channel emitter 在 `place_merge_gate` assert panic。已在所有入口共同的 `checked_topological_order` 增加三面 cell 的硬體上限，返回既有 NotRealisable。此修復只是拒絕不合法單 cell，不宣稱已新增寬 OR 自動 lowering；ordinary `lower()` 目前仍把此類 Or 原樣保留，完整 HDL／lowering 支援需後續補上。回歸覆蓋 4、5、7 fan-in 的 default、independent、legacy 三入口。

## 平直刷新實驗進展

已提出依幾何／ownership 前移內部階梯刷新元件的通解，並提供 `compile_flat_refresh_candidate()`。Native／fresh Yosys 完整 seven segment 均嚴格認證、所有512階段內部與公開真值通過；完整階段與原始控制相同，三次完整生成相同。共享 cover mask 0/1 雖認證成功但品質退步，因此不採用。見 [平直刷新認證修復](generator-flat-refresh-certification-2026-10-02.md)。

此步完成的是大型 candidate 認證的前置缺口，沒有宣告大型延遲／毛刺改善或預設整合完成；下一步建立適用更廣邊界的逐項 runtime qualification/selection，並改進 cover 的物理路徑時序。其餘七項狀態保持未完成，root baseline 15 failures 尚未收斂。

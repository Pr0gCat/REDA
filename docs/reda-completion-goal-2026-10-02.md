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

## 較大邊界預設整合進展

`compile()` 現在對一至六個組合輸入、任意非空公開輸出邊界，嘗試平直刷新候選；嚴格認證、完整 retained-history 真值及 setup/target 共24項不退步才選用。Native／fresh Yosys 完整 seven segment 的實際預設結果與前輪認證候選完全一致。固定2/3家族仍保留原規則。詳見 [較大邊界預設整合](generator-default-flat-refresh-selection-2026-10-02.md)。

這只推進第一項的預設認證整合，沒有把平直刷新當成完整共享解碼生成，也沒有大型延遲／毛刺改善。較大 ROM/Mux、可擴展通用組合／搜尋、循序／HDL、真實 Minecraft 與全專案收斂仍未完成。回歸另定位 reference size 舊快照失敗：and4原始／預設同為238，full_adder同為1017；沒有改寫其232／1065歷史期望，需後續追根因。路徑測試保留原始 Legacy decoder，另驗收預設 FragmentSynth 嚴格認證。

## 大型內部波形及拒絕實驗

新增 opt-in gate event trace，完整 retained-history 各閘事件由獨立 Python 重建狀態。觀察到原始／共享 cover 的早期條件暫態及後續傳播，並定位 cover builder 的多餘 OR/NOR、singleton merge、未使用常數閘。測試末級 NOR 融合與 output-cone pruning，全部來源等價、嚴格認證及512階段真值通過，三次全 bytes 一致；但最差與毛刺仍退步於44 ticks／85切換的固定原始控制，四個新 cover 全拒絕，未改預設。下一步需量測 landing operands 的物理到達時間後處理配平。見 [大型內部波形實驗](generator-cover-internal-timing-2026-10-02.md)。其餘驗收仍保持未完成。

## 路由到達時間與配平實驗

觀測全部 gate-input terminal 與最後 interior repeater；原始／共享 cover 分別覆蓋74／100／109個 operand pair，各512階段。實測 cube 的d2於tick6、d0於tick8到達，3／4個刷新元件形成2-tick暫態。僅調整branch-exclusive既有repeater延遲，可使兩者同於tick8並消除此cube脈衝，但原始Native總穩定時間8823→8829、setup8786→8792，故拒絕。末端dust改repeater另造成mixed cover內部錯值，也拒絕並保存錯誤紀錄。反向input placement同樣退步。四組實驗各三次bytes一致，未改預設、控制、simulator或verifier。詳見 [路由到達時間實驗](generator-route-arrival-experiment-2026-10-02.md)。下一步縮短較慢路徑，再評估配平，八項仍未完成。

## 有效刷新縮減

首次取得完整 Native seven segment 的實際預設改善：總穩定時間8823→8671、輸出毛刺切換85→83、錯值脈衝1425→1385；較大61-gate 16×7 ROM亦改善15745→15731、157→153、4531→4467。setup/target全部28項不退步，512階段內部與公开真值通過，ROM另核對獨立字表。每組大電路三次bytes相同，4096小家族三次保持原成果。依穩定順序逐一提案、認證、完整qualification、接受後組合；default預算256，無名稱或座標白名單。199單次提案中192雖嚴格認證但內部真值失敗，均拒絕，證明不能省略runtime驗證。詳見[有效刷新縮減](generator-qualified-refresh-reduction-2026-10-02.md)。

推進第1/2/3項但未全部完成：一般API budget、共享decoder、更多ROM/Mux與更大輸入規模仍需處理，其餘循序／HDL／無Yosys／真實Minecraft／全專案收斂繼續保持未完成。

## Legacy HDL 原生支援與路由根因

原生支援 `reg`、wildcard legacy `always`、現有正緣／enable clocked forms，以及連續賦值的串接／巢狀／slice targets。標準 `seven_segment.v` 現在可在 native/auto 且 Python 不存在時完成實體生成；三次完整512階段波形與已封存有效優化結果完全相同，未退步。25項compiler、12項DFF來源語意、24項frontend unit、2項DFF實體、6項CLI selection通過，WASM library檢查通過。詳見[原生legacy HDL](native-legacy-hdl-2026-10-02.md)。第5/6項有實際進展但尚未完成，parameter／instance／inout與更廣循序範圍仍待實作。

第8項另重現路由根因：typed request 的arrival balancing runway使一般／unary末端與frozen reference不同，ring charge及pinned fixture前置幾何因此失配。原始期待、simulator與verifier均保留，已封存[路由證據](reda-completion-routing-audit-2026-10-02.json)。需分開legacy compatibility與經完整qualification的balancing候選後再驗收，不以更新快照宣告修好。八項總目標繼續active。

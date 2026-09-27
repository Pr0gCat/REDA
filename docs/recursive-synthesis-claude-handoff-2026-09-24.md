# REDA recursive synthesis 工作交接

更新日期：2026-09-24，Asia/Taipei。交接對象：Claude Desktop。

## 接手摘要

請繼續 `/Users/seith/Desktop/REDA` 的 `codex/fragment-synth-performance`。**目標未完成，尚不能合併。** 最新阻塞已不是 input fanout，也不是搜尋高度，而是 **g19 的連續上升階梯沒有合法中繼器位置，訊號在到達平台前耗盡**。

下一步先使用下方固定路徑 replay 建立快速回歸，再修既有 router／parent egress 的階梯可實現性。不要再直接提高 A* 預算、調 grain 或繞過物理認證。

本檔記錄已觀察的結果，不保證任何後續修改仍具有相同結果。接手時以磁碟與測試為準。

## 完整目標與工作限制

完成 deterministic recursive contract synthesis：自動 logical graph partition、穩定 ChunkId、region/halo/portal/corridor、parent-owned trunks、遞迴並行 child synthesis、typed refusal 與有限 repair、stable composition、authoritative root certification；接入 `compile_fragment_synth` 正式入口；證明 1/N workers fingerprints 一致；通過六個 budget-zero acceptance cases、pinned glyph 與既有品質限制；證明替代成功後移除舊 whole-circuit fallback 與專用診斷。

- 不新增第二套 leaf router，不以 whole-circuit fallback 隱藏失敗。
- 不建立 PR。使用者原意是完成後直接準備 merge；目前不可宣稱已可 merge。
- 不改 baseline、降低 acceptance 標準或藉調 partition/grain 避開問題。
- 小範圍測試優先，單次實際測試約三分鐘上限；編譯時間分開計。不要執行 `check.sh`、完整 lib/frontend/viewer suites 作為每次迭代。
- 使用者偏好由 agents 實作與 review，簡潔中文回報；Claude 用 Fable/Opus，high 以上，不用 Sonnet。這次使用者轉到 Claude Desktop 是為了接替 Codex，而非要求再由 Codex 啟動 Claude MCP 工作。
- `.codegraph/` 存在。理解／定位程式碼先使用 CodeGraph，遵守 AGENTS.md。Ponytail 原則：重用現有機制，最小根因修正。

## 工作樹安全與交接狀態

目前大量 dirty/untracked 修改，包含使用者另一條線的 native SystemVerilog、DFF、evaluator、viewer。**不要 reset、整檔覆蓋或把所有 dirty changes 當作本任務內容。** 本次沒有 commit／PR。

`packed_node.rs`、`packed_recursive.rs`、`packing.rs`、`recursive.rs`、`attribution.rs` 等重要實作仍是 untracked，不能只看 `git diff`，也不能只從 HEAD 建新 worktree 就假定有完整工作。先保留當前工作樹；並行 writers 必須隔離且帶齊正確基底，避免遺失未提交依賴。

交接時所有本任務 Codex agents 已停寫／完成，終端查不到 cargo、rustc 或 acceptance process。Claude MCP 舊 writer 已在先前切換時停止；本次未派新 Claude 任務。

最新 g19 診斷的 TEMP tracing 已移除，agent 以逐 byte 比對確認 `routing.rs`、`parent.rs` 回到診斷前版本；正式修正仍保留。已有舊 trace binary 可能留在 target，接手應重新 cargo build，不把 binary 當最新 source 的證明。

## 最近完成且保留的修改

### 1. Demand-driven repeater refresh

檔案：`src/compile/routing.rs`。

- Strict/seed 路徑改用 `ReservePolicy::LatestLegalCell`，不再以全路徑最長不適合中繼段預扣每一輪 refresh 距離。
- 紅石粉先走，到需求點才向前回找最近合法的平直格放 repeater。
- `straight_flat_repeater_fits` 保證進出皆平直；不能在轉彎／階梯位置硬放 repeater。
- 修過 terminal phantom refresh：dust terminal 不能把預算規劃的 repeater 覆寫成 dust 後還算刷新成功。`terminal_hosts_demanded_refresh` 決定末格是否可 refresh；不可時往前放，無合法位置就拒絕。
- 保留 legacy `TotalStairs` 行為以免改掉舊 pinned layouts。
- Fable 實作、Opus review 通過；之後本地 routing tests 仍通過。

量測（同一 segment_a 路徑的近期結果）：

- 原兩葉合計 2753 route cells／313 repeaters；更新後 2771／253，repeaters 少 60。
- Root 從 134 ticks／6567 blocks 到 **108 ticks／6613 blocks**。
- 品質仍未達標：segment_a 要求不超過 **72 ticks／6416 blocks**。
- 最近三個小 acceptance：and4 14／232、verilog_and4 14／290、full_adder 46／1065，皆 pass。這些數字是後續 fanout/height 修改前量測，最後仍需重新驗證。

### 2. Parent-owned recursive input fanout

原 seven_segment 拒絕：node `d43a872b2d644dcb634148bb8b5e235b9e6aad033d00eca548eb57781f05bad5` 的 `g10` input 被兩個 children 讀取，但 packed node 只會把一個 child terminal 當 root port。

保留的修改：

- `parent.rs`：新增 `PackedInputTrunkRequest`；重用原有 trunk router。parent 建 caller 空格、handover repeater、內部 source，分送至帶真實 child IDs 的所有 sinks。
- `packed_node.rs`：多 reader input 才建 parent hardware，單 reader 路徑不变。`PackedRootPort.interface` 為 optional child owner，另保存明確 contract，parent-owned 介面不冒充 child。
- Child root input 改作內部 sink 後，原邊界 runway 可能伸到負座標。依實際 sink egress（含最高 lane staircase）推導必要平移，整個 selected packed layout 同量平移，world、halo、interfaces、gate metadata 一致。
- `packing.rs`：`translate_artifact` 只擴大為 `pub(crate)` 供重用。
- `packed_recursive.rs`：把原本期待 fanout refusal 的 test 改為真正 repair 成功；另外補回 early packing `RepairFailed` 的 typed cause/source/lineage 與 1/4-worker failure 一致性測試。

驗證：

- `shared_input_fanout_is_parent_owned_certified_nested_and_worker_invariant` PASS。涵蓋兩 sinks、真正包入外層並認證、1/4 fingerprints、切斷 trunk 後拒絕。
- `a_refused_shared_input_leaf_repairs_with_parent_owned_fanout` PASS，包含 split repair、1/4-worker、shipped truth table。
- packed_node tests 13/13 PASS。
- `an_early_packing_repair_failure_keeps_its_typed_cause_and_lineage` PASS。注意 thiserror 的 source 實際 downcast 到 `Box<PackedRecursiveError>`，不是裸型別；test 已修正。
- Root 曾独立重跑 shared_input 2/2 PASS。

### 3. Hard guidance 搜尋高度修正

檔案：`src/compile/routing.rs`，`search_path` 計算 search bounds 的既有 hard-guidance 區塊。

確定 bug：parent 指定 lane=12，端點均 y=3，但原搜尋上限只 `max(endpoint.y)+6=9`；hard guidance 原本只擴 x/z。

最小修正：Y min/max 也納入 `preferred_y` 和 `access_y`。不更動預算、reservations、soft/unguided 行為。`allows()` 的 hard 約束只管 x/z，Y 仍是成本偏好，因此旧程式不是所有路由都不可能成功，只是根本搜尋不到指定高度。

驗證：

- 新 `a_hard_guidance_height_above_the_endpoint_ceiling_is_reachable` 用擋到 y9 的牆與封閉側向範圍，必須實際越牆；測 preferred_y/access_y，且模擬訊號到達末端。
- 真正移除修正跑 before-fix：NoLocalRoute；補回後 PASS。
- Routing **63/63 PASS**，包括 demand-refresh；Root 也獨立執行過這 63 個 tests。
- Review 無 blocker。不是提高 max expansions/queue。
- Seven 的 rank0 隨後能走過 g10、g16，下一個錯誤變成 g19 PhysicalInvariant。

## 當前阻塞：g19 連續階梯無法 refresh

正式測試：seven_segment，仍 **未 certified**。最近普通 run 88.06s；精確診斷 run 94.02s。

```text
node d43a872b2d644dcb634148bb8b5e235b9e6aad033d00eca548eb57781f05bad5
all 5 packed layouts were refused
rank0: packed connection g19 from None
route RouteId(2) ... PhysicalInvariant
```

已證實的具體原因：

- 第一個 sink ordinal0，shared=0、incoming strength=15、local_reroutes=0。
- `realise_branch_cells(...).carries == false`。不是 floor overlap、terminal axis、ConnectionId 或 queue failure。
- Rank0 path 長 285 cells；queue=239424，未達 262144 上限。
- Source `(104,3,10)` 爬到 lane18，連續 15 級上升。
- Path index2..17 連續 16 格不能合法放 repeater。
- 最後合法 refresh 在 index1 `(103,3,10)`；到 index16 `(93,17,15)` 強度為0；下一合法位置 index18 `(91,18,15)` 已太晚。
- 四個實際進入 routing 的 layouts 都有同類連續爬升問題。

**因此物理拒絕正確，不能刪除 carries check 或讓階梯格直接承載非法 repeater。**

### 下一步建議

1. 先用下方 JSON 固定路徑建立毫秒級 regression，重現這個不可 refresh 的連續階梯。不要每一個假設都重跑整個 seven_segment。
2. 查清 parent 強制 source/sink egress staircase 與既有 `search_path` 的責任邊界。若階梯是 forced runway，單改自由搜尋可能不能插入平台；需讓幾何契約本身提供合法平直 refresh 空間。
3. 在既有 routing/egress 機制中讓路徑具有合法 refresh 平台，或讓既有 search 拒絕不可實現的候選。不要新建第二 router。
4. Repo 有 `strength_aware_astar` 可先了解，但 agent 發現它沒有 `RouterWork`／bounded counters，**不能直接換入**；尚未作完可重用性評估。
5. 最小 fixture 必須證明訊號實際抵達（simulator），再跑 routing、shared input、typed repair tests，最後 single seven acceptance。
6. 若功能已過但品質未過，明確區分，不改 baseline。之後還有 segment_a 品質、pinned glyph 當前版本、全範圍 determinism/cleanup 的收尾。

## 診斷證據與快照

下列 `/tmp` 檔在交接時實際存在，重開機可能清除；需要長期保存時先複製到自己的工作資料夾。

- 最新 g19 完整 trace：`/tmp/reda-g19-invariant-trace.FbBwQD`
- **快速 replay JSON**：`/tmp/reda-g19-invariant.POYiqz/rank0-carry-replay.json`
- g19 診斷前 source snapshots：`/tmp/reda-g19-invariant.POYiqz/`
- Height 修正後正式 seven log：`/tmp/reda-seven-guidance-height.nGOqIR`
- Height 修正前 routing snapshot：`/tmp/reda-guidance-height.nPb5WE/routing.rs`
- Fanout 修改前四檔：`/tmp/reda-input-fanout.VCQlHs/{parent.rs,packed_node.rs,packing.rs,packed_recursive.rs}`
- Height 修正前帶身分 trace：`/tmp/reda-g10-identity-trace.QXYcYr`
- 初始無身分 trace：`/tmp/reda-seven-fanout-trace.0wjCWq`
- Demand refresh 前 snapshot：`/Users/seith/reda-preedit-20260924/routing.rs`
- Leaf metrics：`/tmp/reda-preedit/metrics.log`、`/tmp/reda-after-metrics.log`、`/tmp/reda-baseline-metrics.log`

舊 g10 身分 trace 說明：rank4 g10 可成功，接著 g16 超 queue；rank3 egress conflict；rank0/1/2 g10 超 queue。Rank0 sink0 用157437，sink1再用104708，總262145。RouterWork 是跨 branches/retries 累積，不要誤稱第二 sink 單獨耗掉262k。

舊 source `halo.max.x + 10` 間距曾被提出風險，但對觀測 g10 的所有 source egress，實測 `in_halo=[]`、`occupied=[]`；sink core ownership 正確、foreign egress 為空。**沒有證據支持藉增大 +10 解決目前 g19 問題。**

## 可用測試命令

在 repo root 執行；先 build，長 acceptance 請另以三分鐘監控執行。不要依赖以下示例 target hash，使用 cargo 產出的當前 binary。

```sh
cargo test --release --lib shared_input -- --nocapture
cargo test --release --lib an_early_packing_repair_failure_keeps_its_typed_cause_and_lineage -- --nocapture
cargo test --release --lib a_hard_guidance_height_above_the_endpoint_ceiling_is_reachable -- --nocapture
cargo test --release --lib compile::routing::tests:: -- --nocapture
cargo test --release --test fragment_synth_acceptance --no-run
cargo test --release --test fragment_synth_acceptance budget_zero_seven_segment -- --exact --nocapture
git diff --check
```

六個 exact acceptance names：

- `budget_zero_and4`
- `budget_zero_verilog_and4`
- `budget_zero_full_adder`
- `budget_zero_segment_a`
- `budget_zero_seven_segment`
- `budget_zero_pinned_verilog_seven_segment`

需要葉片 metrics 時有 test-only ignored harness：

```sh
cargo test --release --lib segment_a_recursive_leaf_route_metrics -- --ignored --nocapture
```

## 尚未完成／不能宣稱完成

- Seven_segment 目前功能失敗，不是只有品質不夠。
- Segment_a 最近108/6613，品質不過72/6416。
- Pinned glyph 當前樹尚未重新驗證。較舊版本能 certified，不能當現在的證明。既有 baseline 該案例未 certified，數值改善 predicate 依 evaluator 決定；仍須功能與11個 caller pins 正確，勿自行更改標準。
- 新 fanout／已有三層 fixture 的 1/N 通過，不等於所有六案例、所有路徑 deterministic 完整證明。
- Public compile path 已走 recursive，舊全電路 seed fallback 在 production 入口已移除／test-only；最終 cleanup 仍須依使用者全目標審核，不因局部綠測試宣告完成。

## 已嘗試但不要盲目重做

- Explicit seam-band packing 曾讓 segment_a 變142/7412，已撤回；現有 parent BandPlan 是另一件事，不可整批刪。
- 全域 weighted slot permutation 當時卡精確 pose snapshot tests，已撤回；有些測試只是 snapshot，不全是物理契約，但尚未重新實作。
- Prim sink ordering 幾乎無收益且增加部分階梯，已撤回。
- A* heuristic 改 `max(horizontal L1, abs(dy))` 曾修正啟發式一致性，但量測 repeater 變多，已撤回；不要假定目前已採用。
- Selective post-route rip-up 被否決：缺 branch 級 reservation/refcount/promotion provenance，無法安全釋放共享硬體；不是幾行就能做的優化。
- Strict/seed 的 pre-stair 強制 repeater loop 在目前 eligibility 下無作用；legacy 還需要，不要把 demand refresh 收益歸給刪掉該 loop。

交接後請先讀 replay 與相關 source，提出具體、可驗證的階梯 refresh 修正；不用再從頭重查 fanout 和搜尋高度已解決的問題。

## 已保存到 repo 的持久交接包

關鍵資料已另存到 `docs/handoff-2026-09-24/`，不必依賴 `/tmp`：

- `evidence/rank0-carry-replay.json`：g19 失敗 branch 的固定路徑。
- `evidence/reda-g19-invariant-trace.FbBwQD`：最新精確根因 trace。
- `evidence/reda-seven-guidance-height.nGOqIR`：最新普通 acceptance 輸出。
- `evidence/reda-g10-identity-trace.QXYcYr`：高度修正前各 layout 的 g10 身分紀錄。
- `pre-input-fanout/`：四個源碼檔的 fanout 修改前快照。
- `pre-guidance-height/routing.rs`：搜尋高度修正前快照；已含 demand-driven refresh。

**快照是比較證據，不是可直接回復的版本。** Repo 先前已是 dirty；把快照覆蓋 live source 可能移除後續正確修改。這些檔案沒有加入 Rust module，不是另一份實作，也不應讓 CodeGraph/全文搜尋的快照結果誤導對 live code 的判斷。優先限定 `src/` 查詢。

Replay JSON 的語意：`path` 包含 source 起點（285 個 route cells 的計數不一定包含它）；`previous`、`incoming`、`shared`、`terminal_hosts_refresh` 是 realisation 的上下文；`repeaters` 是該失敗規劃曾選的位置，**不是已通過物理驗證的答案**。它不是完整 world/reservations capture，因此可以快速驗證強度規劃，但單憑 JSON 無法證明重新 A* 的碰撞合法性；新的完整路由仍需真正 routing + simulator/certification。

## 架構導航：從正式入口到物理世界

以下是閱讀順序與責任邊界，不是要求重寫架構：

1. `src/compile/fragment_synth/api.rs`：`compile_fragment_synth` 建立 configs、case fingerprint，走 recursive contract producer。Public path 不應回退全電路 seed search；legacy seed helper 已限制在 tests。
2. `recursive.rs`：`compile`／`compile_with_workers`、root pins 正規化、直接 root leaf、pinned root 分配／組合、unpinned packed recursive adapter，最後組成正式 product。当前 `TERMINAL_GATES=32`、`MAX_RECURSIVE_WORKERS=8`；不是這次修 bug 的調參入口。
3. `partition.rs`：canonical ordering、root/node/chunk IDs、logical split 與 boundary signals。不可讓 worker 完成順序或任意 map 遍歷順序改變 IDs。
4. `packed_recursive.rs`：`PackedGrain::production()` 使用 `TERMINAL_GATES`。建立 children、parallel schedule、遇可 repair leaf refusal 就有限細分；保留 refusal 與後續 error 的 lineage。`PackedRecursiveProduct` 含 node/artifact、leaf diagnostics、depth、實際 peak workers。
5. `leaf.rs`：`synthesise_free_leaf` 等，沿用已有 planner/router 生成 leaf；`FreeLeafArtifact` 對外提供可平移的世界、occupied/halo/access、interfaces、gate metadata。
6. `packing.rs`：deterministic ranked placements、平移、halo/介面相容；不是多次無界随机重試。
7. `packed_node.rs`：組 child worlds、判讀 boundary uses、parent trunks/input fanout、root interface export、certification、`into_parent_connectable`。一個已認證 node 要能作為下一層 child，而不是只在頂層運作。
8. `parent.rs`：parent trunk routing、endpoint guards、egress、可用 lane/band、source/sink reservations，重用 PhysicalRouter。`PackedTrunkRequest` 是 child source；`PackedInputTrunkRequest` 是 parent input source；共同進既有 router。
9. `routing.rs`：`route_guided_with_runways` 對應的 DurablePhysicalRouter 路徑、`route_ordered_attempt`、`search_path`、`realise_branch_cells`、route certification。搜尋到幾何路徑不代表能保證訊號，必須通過 realisation 與物理檢查。
10. `certification.rs`：`certify_root_world` 是權威來源。`recursive::assemble_product` 使用 certificate 的 world fingerprint 與真實 trunks timing fingerprint 組 product，不可用未認證 world 的 metrics 代替。
11. `benchmark.rs`／`tests/fragment_synth_acceptance.rs`：通過 public compile，再用 evaluator 衡量同一 compiled world，最後對 baseline 判斷；`compiled_and_certified` 與 `case.passed()` 不相同。

`attribution.rs` 保留實際 recursive leaf/trunk diagnostics；metrics harness 應讀真正被選中的 children，不要拿全電路 planner 的重建物當成 recursive 葉片證據。

## 必須保留的物理／身分規則

- 每個外部 root input 只公開一個 caller cell。Fanout 在 parent 內分發，不可要求呼叫者驅動多格。
- Caller cell 與內部 source dust 是不同座標，中間有 handover repeater；source strength 必須符合實際供電，不可只在 contract 填15。
- Child sink endpoint 身分仍屬真實 child。Parent-owned source 使用 `PrimaryInput(PortId)`；packed child routing endpoints 轉為 PrimitiveOutput/Landing 命名空間，不可混淆。
- Root caller 和內部 source 可共享 logical endpoint ID，但 root guard 使用獨立 keep-out owner；release endpoint access 不得釋放 root 的外部接入保護區。
- Handover repeater/floor 必須先確實加入 world/reservations；不能以虛構 child ownership 讓 reservation 檢查略過。
- Sink runway 必須留给後續 branch；先前 fanout branch 不可把下一個 sink 的入口堵住。共享 floor 與導線的所有權也不能只靠刪格推斷。
- 高度 lane 與 x/z corridor/band 是不同限制。指定偏好高度必須包含在 search envelope；仍需保留 world shell/reservations，不能為可達性直接放寬所有障礙。
- Root adaptation 會為未 pinned 的 ports 放 lever/lamp；caller 指定的 pin cell 交付時必須維持空格，方向與座標要正確。
- `assert_supplied_pins_honoured` 檢查 role、exact position、caller cell Air；pinned glyph fixture 有11個 placements。不能用重命名 ports 或忽略 pins 過關。
- Counter/queue limits 跨同一路線的 branches、retry 累積。保持 deterministic bounded work，不改為無界 fallback。

## 測試範圍與已知舊問題

最近沒有跑完整測試套件，這是刻意控制迭代成本，不代表整個 repo 全綠。

較早 demand-refresh 工作曾誤跑完整 lib suite，結果1103 passed／6 failed／69 ignored；那六個失敗在 demand-refresh **之前的精確 source snapshot** 也同樣失敗。涉及 planner negotiation、pinned output 同時餵 gate、grown unpinned and4、keep-out footprint、legacy pinned full-adder byte layout、stale dust resettle differential。不要僅因看到这些失敗就回退已驗證的 refresh 改動；也不要宣稱它們永遠無關，最後整合時仍應清楚列出。

舊 log：`/tmp/reda-fulltest.log`、`/tmp/reda-fulltest2.log`；這些未收進持久交接包，非當前修正通過的證據。目前常見 warnings 是 packing 的 `unused_mut`，lib test 還有 `LayoutSearchError::Exhausted.rank_zero` 未讀；不是本次 g19 根因。

曾有精確位置 snapshot 阻擋 placement 實驗：planner repeater facing/origin、packed split operands mouth adjacency。Review 認為其中一些是固定布局偏好而非必要物理契約。若未來真的改 placement，可將 snapshot 改成關係性幾何測試，但必須保留 truth table、pins、trunk separation、cut-trunk拒絕、determinism；不要為了過測試直接刪。

## Case identity 與完成判定

目前 `recursive::producer_revision()` 對字串 `recursive-contract-producer-v1:terminal-gates={TERMINAL_GATES}` 做 fingerprint；case descriptor 會帶 recursive producer revision、library/verifier/simulator/config/manifest 等資訊。最近 refresh 修改未另外 bump router revision；先前 reviewer 判斷 candidate content fingerprint 會反映實際世界，但**最終發布前仍要審核 producer/cache identity 是否足以代表全部行為變更**，不要無理由改 baseline 身分，也不要以 case fingerprint 不變推論 output 不變。

只有以下項目都有當前證據才可宣稱完整完成：

- Public compile 使用新的 recursive contract producer；不靠舊全電路 fallback。
- 父子介面、halos、trunks、fanout、root pins 有跨層實際物理與功能驗證。
- 分割/repair 有限、錯誤 typed 且 lineage 正確。
- 1/N workers、canonical declaration order 的 determinism 覆蓋實際 production 路徑。
- 六個 budget-zero cases 都通过原有 scorer；不是只 compile、只 truth table 或單個 fixture。
- Pinned caller cells、segment_a/seven 品質限制仍符合，沒有改判分標準。
- 舊 fallback 專用程式確定不再使用才移除；保留共享 router/legacy必要功能。
- 檢查 dirty scope，不夾帶使用者 SV/DFF/viewer 工作；再準備使用者要求的直接合併流程。

## 可直接貼給 Claude Desktop 的開場

> 請讀 `/Users/seith/Desktop/REDA/docs/recursive-synthesis-claude-handoff-2026-09-24.md`，從目前 dirty branch `codex/fragment-synth-performance` 接手。所有既有改動先保留。先讀持久交接包的 g19 replay，處理 y3 到 lane18 連續階梯缺合法 refresh 平台的根因；不要加 A* 預算、調 grain、造第二 router 或繞過物理驗證。使用 CodeGraph，最小可驗證修正，再做 targeted tests、seven_segment。完整目標仍是六案例、品質與 determinism 全部通過，不建立 PR。目前 Codex 已停寫，請由你繼續。

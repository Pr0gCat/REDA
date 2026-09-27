# fragment_synth 優化計畫

> 基準：main `fb74453`，與 `cc43241` 是同一棵樹，`git diff` 為空。本文所有 file:line 都以這棵樹為準。`F` 代表 `src/compile/fragment_synth/`。
> 量測條件：release 建置，使用獨立的 `CARGO_TARGET_DIR`，10 核 Mac。每案一個 process，指令為 `--exact --nocapture --test-threads=1`，外包 `/usr/bin/time -l`。每案只跑了一次，不是中位數。
> 原始紀錄放在 scratchpad `/private/tmp/claude-501/-Users-seith-Desktop-REDA--claude-worktrees-caveman-full-3b51d3/afa6a9f0-76f5-41fd-a860-f7be868bf89d/scratchpad/`（`plan-time/`、`density/`、`tick-probe/`、`wall/`、`plan-lib.log`、`plan-clippy.log`、`ign.log`）。這些檔案不在 repo 裡，session 結束後可能會消失。

## 1. 現況摘要

### 1.1 數字（對照 `tests/fixtures/fragment_synth_baseline.json`，legacy 是 `compile_legacy`，見 `F/benchmark.rs:251`）

| 案例 | 現在 ticks / blocks | legacy ticks / blocks | 體積：現在 vs legacy | wall | 出貨 producer | gate |
|---|---|---|---|---|---|---|
| and4 | 14 / 232 | 18 / 472 | — / 9,212 | 0.08 s | planner 直接葉 | 過 |
| verilog:and4 | 14 / 290 | 22 / 480 | — / 8,836 | 0.06 s | planner 直接葉 | 過 |
| full_adder | 54 / 1,094 | 46 / 1,784 | 18,725 / 39,750 | 0.31 s | planner 直接葉 | **ticks 紅** |
| segment_a | 124 / 9,305 | 72 / 6,416 | 391,680（272×18×80）/ 149,604 | 118.4 s | lid fabric `[4,6]` | **紅** |
| seven_segment | 198 / 21,847 | 98 / 16,244 | 未量 / 329,814 | 161.4 s | lid fabric `[6]`（第 4 個嘗試） | **紅** |
| pinned:verilog:seven_segment | 190 / 20,410 | baseline 未認證 | 非空氣 bbox 431×20×119 = 1,025,780 | 36.7 s | pinned fabric `[4,6]` | 過（gate 只要求 certify，見 `F/benchmark.rs:523-548`） |

### 1.2 已經穩固的部分

- 6 個案例全部通過 certify，懸空方塊為 0。檢查點是 `certify_root_world`（`F/certification.rs:773`）裡的 `unsupported_component`（`F/certification.rs:629-652`）。
- and4 的兩個案例 ticks 和 blocks 都贏 legacy。full_adder 的 blocks 比 legacy 少 39%。
- 只有一個 router。A\* 上限是 262,144（`F/config.rs:27-28`）。元件層級的 1 vs N worker 測試很齊全。
- `StripShape::tightest` 很便宜：每步 10–30 ms，每案合計不超過 1.1 s。

### 1.3 弱點（依影響排序）

1. **延遲**
   - seven_segment 是 legacy 的 2.0 倍，segment_a 是 1.7 倍。
   - seven_segment 的 198 ticks 中有 138（74%）花在 3 次跨 leaf。logic depth 本身只要 18–20 ticks。
2. **密度**
   - segment_a 的體積是 legacy 的 2.6 倍，seven_segment 的 blocks 是 1.34 倍。
   - pinned decoder 只有 6.2% 的 blocks 在 pin 矩形內，30 顆 torch 一顆都不在裡面。本體往沒有 pin 的方向一路長出去。
3. **時間**
   - seven_segment 需要 161 s，逼近「單次約三分鐘」的上限（`docs/recursive-synthesis-claude-handoff-2026-09-24.md:20`）。
   - nested packed 一次都沒出貨，卻佔掉約 55% 的時間。每個 producer 都從頭重建葉。
   - 10 核只用到 1.2–1.9 倍。
4. **選擇**
   - 第一個 certify 的就出貨。`QualityKey`（`F/certification.rs:37-43`）從來沒被拿來比較。
   - 相關程式碼：`F/api.rs:94-100` 自己寫著「The budget buys nothing」；proposal loop 只在 test 裡（`F/search.rs:60-67`）。
5. **可信度**
   - 物理行為只經過自家 simulator 驗證。
   - 26.2 探針有 13 項不符，還沒處理。
   - 沒有任何一個 fragment_synth 產物在真實遊戲裡跑過。
   - 預設的 `compile()` 仍然出貨 dust-on-dust。
   - 出貨的 litematic 初始狀態不自洽。
6. **維護**
   - lib 測試：6 個失敗、71 個 ignored。
   - clippy 有 154 個不重複 warning，`check.sh:31` 用 `-D warnings` 所以會失敗。
   - `docs/handoff-2026-09-24/` 有 2.4 MB 的過期檔案。
   - 共用的 target dir 會拿到舊 binary。bake 沒有腳本。
   - viewer 看大世界時取景很差。

## 2. 原則與不變的限制

**硬限制（任何項目都不能違反）**

1. 物理認證不能略過。每個出貨的 root world 都必須經過 `certify_root_world`。
2. A\* 上限不動（`F/config.rs:27-28`；超限的處理在 `src/compile/routing.rs:1187-1193`）。
3. 只有一個 router。fabric 走 `route_planned`（`routing.rs:1280`），它和其他路徑用的是同一個 `search_path`。
4. 決定性：
   - 1 個 worker 和 N 個 worker 的產物必須相同。
   - 排序只用穩定的鍵，不依賴 `HashMap` 的迭代順序。
   - 不用 wall-clock 預算做選擇或截斷。`SynthesisBudget::Time`（型別在 `F/search.rs:15-18`）不能進入 acceptance。
   - 計時只能出現在報告裡。
5. pinned 規則：電路放在 pins 的平面矩形內，可以往上疊，也可以往沒有 pin 的面長出去。不可以在輸入的後面，也不可以在輸出的前面。由 `tests/build_circuit_pins.rs` 的掃描負責檢查。
6. baseline JSON（`baseline_commit` afe577d）不改，acceptance 標準不降。三個紅燈 gate（`F/benchmark.rs:484-489`）保持紅燈，直到品質工作完成。

**工作規則**

- 加速類項目的產物必須逐位元不變。比對方式是 `canonical_world_fingerprint`（`F/benchmark.rs:1190`）。
- 會改變產物的項目：
  - 今天的產物必在候選清單內，而且清單順序等於今天的嘗試順序。
  - 升 `producer_revision`（`F/recursive.rs:446-450`）。
  - 重新烤 `viewer/baked/`。
  - commit 附上 M1 報告的 3 次中位數。
- 新的幾何（z 向 pad、疊層、N/S 面、PITCH 2）要先有 `tests/fabric_kernels.rs` 的 kernel 測試。C3 的真實遊戲環境就緒後，還要附上真實遊戲的結果。
- 每案單次執行不超過 180 s，超過就不合併。

---

## 3. 工作項目

### 3.A 量測、候選選擇與速度

**時間去向（實測）**

| 案例 | 失敗的嘗試 | 可省的葉重建 | 尾段（認證加量測） |
|---|---|---|---|
| segment_a | nested `[4,6]` 0–65.9 s：子葉 seed repair 27 次後兩個 pitch 都被拒，拆半後的 node 兩個 layout 也被拒 | fabric 重建 49.5 s | 2.7 s |
| seven_segment | nested `[4,6]` 65.9 s（root 的 6 個 layout 串行 20.7 s，其中 5 個各約 4 s 撞到 `QueueEntries`）；fabric `[4,6]` 42.4 s（tightest 68 步後 routing 拒）；nested `[6]` 27.6 s（node 認證拒：「RedstoneWire … has nothing to stand or hang on」） | 41 s 加 17.8 s | 7.8 s |
| pinned | pinned packed 唯一的 layout 在 12.5–15.5 s 被拒 | 13.5 s | 6.7 s |

- CPU 取樣：seven_segment 第 83 s 時，`flat_leaves` 只剩一個 worker 在跑，其他 thread 都停在 `semaphore_wait_trap`。
- 該 worker 的 8,299 個樣本中，8,203 個落在 `seed::route_all → DurablePhysicalRouter::route_guided`。其中 `anchor_is_free_for_typed` 佔 38%，其餘主要是 `BTreeMap<Anchor, _>` 的查找。

#### M1. benchmark 報告：體積、密度、計時、producer
- **問題**
  - `FragmentAcceptanceCase`（`F/benchmark.rs:285-302`）已經有 `quality`（292 行），`PhysicalMetrics` 也有 `occupied_min/max/volume`（`src/compile/metrics.rs:33-40`）。
  - 但每案那行只印 ticks 和 blocks（`tests/fragment_synth_acceptance.rs:80-96`），`src/bin/fragment_acceptance.rs:70-75` 只印總結。
  - 沒有計時，也看不到出貨的是哪個 producer。`RecursiveDiagnostics`（`F/attribution.rs:59-62`）只有 `leaves` 和 `root_trunks`。
- **做法**
  - 加入以下欄位：
    - `FragmentAcceptanceCase` 加 `compile_ms` 和 `measure_ms`，用 `Instant` 分別包住 `compile_fragment_synth`（`F/benchmark.rs:411`）和 `evaluate_world`（453 行）。這兩個欄位不進 `passed()`（305-311 行）、不進任何 fingerprint，也不碰 `BenchmarkCase`（32-44 行，`deny_unknown_fields`）。
    - `RecursiveDiagnostics` 加 `producer`（DirectLeaf / NestedPacked / Fabric / PinnedPacked / PinnedFabric / Allocation）和出貨的 pitch rung。
  - `report_case` 每案印一行，內容為：volume、`bbox=XxYxZ`、`density=blocks/volume`、baseline 的 volume 和 density、`compile_ms`、`measure_ms`、producer、`generated_world_fingerprint`。Q1 完成後再加 `candidate=<label> (k/n)`。
  - 不加 volume gate。
- **成功標準**
  - 6 個 `budget_zero_*` 各印一行，包含上面的欄位。
  - baseline JSON 仍能解析，`F/benchmark.rs:1602` 的 schema 測試不變。
  - 所有 fingerprint 不變。
- **風險**：計時是非決定性的，只能出現在報告裡。
- **工作量**：S｜**依賴**：無，第一個做。

#### M2. 剖析 span 與 CPU 取樣
- **問題**：目前只有從 trace 反推的粗時間軸。以下都不知道：每片葉在每個 pitch 各花多少時間、尾段怎麼拆、P1 快取的命中率。
- **做法**
  1. 加 `REDA_TRACE_TIME`，寫法照現有的 `REDA_TRACE_*`（`F/recursive.rs:742`、`F/parent.rs:1255`）。每個 span 印一行 `reda-time <kind> <label> <ms> <ok|err>`，位置如下：
     - 葉的 pitch 迴圈：`F/leaf.rs:207-212`。
     - producer 嘗試：`F/recursive.rs:794-827`、`849-893`。
     - layout build：`F/packed_node.rs:965-1009`，966-976 行已有計時，改掛到新旗標。
     - tightest 的步數與總時間：`F/packed_node.rs:1751-1790`。
     - node 認證：`F/packed_node.rs:1025`。
     - adapter 認證：`F/packed_recursive.rs:700`。
  2. 用 `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` 加獨立的 target dir 建置，然後對 test binary 本身的 pid 跑 `/usr/bin/sample <pid> 10`。
  3. 每個 nested root layout 記錄 `route_count`、`lane_top`（`F/packed_node.rs:568-575`）、canvas 體積和失敗種類，給 P3c 用。
- **成功標準**：
  - 各階段合計至少涵蓋 wall 的 90%。
  - 列出前 3 名熱點。
  - 回答三個問題：
    - node 認證是否 ≥ 3 s？決定 P6 做不做。
    - `route_guided` 是否佔葉時間 ≥ 60%？決定 P5 做不做。
    - nested layout 失敗前能不能看出確定的下界？決定 P3c 做不做。
- **風險**：`REDA_TRACE_FABRIC` 輸出量很大，量時間時要關掉。
- **工作量**：S｜**依賴**：無，和 M1 並行。

#### P1. 單次 compile 內的葉快取
- **問題**
  - `synthesise_free_leaf_at`（`F/leaf.rs:216-260`）是 `(chunk, contract, search, pitch)` 的純函數，卻在很多地方各自重算：
    - nested 的 `LeafBuilder`（`F/packed_recursive.rs:211`、`233`）。
    - fabric 和 pinned 的 `flat_leaves`（332-358 行）。
    - `LEAF_LADDERS` 的第二個 rung（`F/recursive.rs:739`）。
    - pinned grain 減半（`F/packed_recursive.rs:300`、`413`）。
    - `build_leaf_finer`（445-475 行）裡被拒的葉，每次都重新被拒一遍。
  - 兩條路徑建出的葉 `ChunkId` 相同：`partition` 以 boundary 定 id（`F/partition.rs:253-270`），而 `node_chunk_id`（179-187 行）等於 `child.id`。所以快取可以命中。
- **做法**
  - 型別：`LeafCache = Mutex<BTreeMap<(ChunkId, i32), Arc<OnceLock<Result<FreeLeafArtifact, Arc<FreeLeafError>>>>>>`。鎖內只做 get-or-insert，鎖外用 `get_or_init` 計算。
  - 生命週期：由 `compile_with_cutoff`（`F/recursive.rs:747`）建立，只活在一次 compile 內。
  - 傳遞：各函數的 `pitches: &[i32]` 參數改成 `&LeafGrid { pitches, cache }`。
  - `FreeLeafError`（`F/leaf.rs:125-126`）加 `#[error(transparent)] Shared(Arc<FreeLeafError>)`。只有測試會比對它的變體（`F/packed_recursive.rs:1796`、`1981`、`2076`、`2092`），而且那些測試不經過快取。
- **成功標準**
  - 產物逐位元不變。
  - `synthesise_free_leaf_at` 的呼叫次數等於相異的 `(ChunkId, pitch)` 數。
  - wall：segment_a ≤ 72 s，seven_segment ≤ 105 s，pinned ≤ 24 s。
  - `F/recursive.rs:2629` 和 C5 的 sweep 都通過。注意 `F/recursive.rs:3042` 走的是 allocating 的 `solve_subtree`+`compose`，不能證明出貨路徑。
- **風險**：seed 裡若有非純的依賴，快取會把某一次的結果固定下來。靠 C5 的 1 vs N sweep 攔。
- **工作量**：S–M｜**依賴**：M2。

#### P2. 葉層級並行
- **問題**：pitch 4 失敗後才試 6（`F/leaf.rs:207-212`），兩半也是串行建（`F/packed_recursive.rs:454-457`）。
- **做法**
  - 改用 `run_indexed`（`F/certification.rs:716-770`）。它回傳 index 最低的終止結果。job 成功時回 `Err(artifact)`，這樣得到的就是第一個能認證的 pitch，和串行結果一樣。
  - 算出的 pitch 6 結果存進 P1 快取。
  - worker 沿用減半規則（`F/packed_recursive.rs:510`），總數受 `MAX_RECURSIVE_WORKERS = 8`（`F/recursive.rs:77`）限制。
  - 只在 `pitches.len() > 1` 時並跑，是否並跑依靜態的 `CertificationWorkers` 配額決定，不看執行期的閒置狀態，確保 1 vs N 的 sweep 能覆蓋兩種排程。
- **成功標準**：產物不變。seven_segment 的 user/wall ≥ 3。segment_a 在 P1 之後再降 ≥ 10 s。
- **風險**：pitch 4 成功時，pitch 6 等於白算的 CPU。
- **工作量**：S｜**依賴**：P1。

#### P3. 壓低 nested 失敗的成本
- **問題**
  - node 的 layout 一個接一個 route（`F/packed_node.rs:965-1009`），失敗的一路跑到 A\* 上限。
  - 三次失敗的原因各不相同，目前看不出確定的下界。
- **做法**
  - a. layout 迴圈改成「成功即終止」的 `run_indexed`。和 layout 無關的錯誤也當作終止，`rank_zero_error` 依 index 取。結果和串行相同。
  - b. Q1 落地後 nested 和 fabric 並行，nested 失敗只花 CPU，不加到 wall 上。
  - c. 只接受可證明的下界，模仿 `pinned_floors_short`（`F/packed_node.rs:1906-1917`，文件寫明「a `Some` is certain」）。找不到就不寫。
  - 略過 node 級快取，等 M2 顯示 `[6]` rung 常常完全重複 `[4,6]` 時再加。
- **成功標準**：(a) 產物不變，seven_segment root layout 階段從 20.7 s 降到 ≤ 6 s。(c) 交出下界表，或結論「沒有確定下界」。
- **風險**：每個並行 layout 帶一個 world 加 A\* 狀態，要設 worker 上限。
- **工作量**：a、c 各 S｜**依賴**：M2；b 依賴 Q1。

#### Q1. 決定性多候選選擇，取代「第一個認證就出貨」
- **問題**
  - 每一層都是先成功的先出貨：
    - `F/recursive.rs:794-828`（unpinned）、`849-893`（pinned）。
    - layout：`F/packed_node.rs:965-1009`。
    - `tightest`：`F/packed_node.rs:1751-1790`。
    - pitch：`F/leaf.rs:207-212`。
  - `QualityKey` 的 derive `Ord` 正好是 (ticks, blocks, volume) 的字典序。它在 `assemble_product` 用已認證的 world 填好（`F/recursive.rs:659-667`），但 production 從來不比較它。`static_routed_delay` 只是複製 settle（664-666 行）。
  - seven_segment 出貨的是第 4 個嘗試。segment_a 的 nested `[6]` 從來沒被試過；舊量測顯示它約 6,566 blocks、102–108 ticks，兩項都比今天的 fabric 好。
- **做法**
  - 固定的候選清單，不隨 budget、時間或 worker 數變化：
    - 未釘選，gates > `TERMINAL_GATES`（`F/recursive.rs:76`）：`[nested [4,6], fabric [4,6], nested [6], fabric [6]]`。
    - 釘選：`[pinned packed [4,6], pinned fabric [4,6], pinned packed [6], pinned fabric [6]]`。四個都失敗才走 allocation（`F/recursive.rs:896` 起，S1 會處理它的去留）。
    - 直接葉不動。
    - seam 形狀不另列候選：`tightest` 失敗時已經會退回 full strip（`F/packed_node.rs:874-885`）。
  - 執行：`run_indexed(CertificationWorkers::bounded(n), n, |i| Ok::<_, Infallible>(attempt(i)))`，每個候選一路做到 `adapt_packed_root`，分到 `max(1, workers/n)` 個 worker，葉經由 P1 快取共用。
  - 選擇：純函數 `pick`，用 dominance 規則：
    - 基準 = 依清單順序第一個認證成功的候選，也就是今天會出貨的產物。
    - 只在 ticks 和 blocks 都不比基準差的候選之間，依 `(QualityKey, index)` 取最小；沒有這樣的候選就出貨基準。
    - 全部失敗時回傳最後一個候選的錯誤，和 `F/recursive.rs:828` 一致。
  - 報告：`RecursiveDiagnostics` 加 `candidates: Vec<(label, Result<QualityKey, String>)>` 和 `chosen`，不含時間。
  - `producer_revision` 升到 v2。
  - 不設時間預算，認證前也不剪枝。
- **成功標準**
  - 6 案出貨的 ticks 和 blocks 都不比今天差。這由構造保證，因為今天的產物就在清單裡，而且只有 dominance 才換。
  - 沒有任何 `no_tick_regression` 或 `no_block_regression` 由 true 變 false；目前綠燈的 and4、verilog:and4、pinned 保持綠燈。
  - `pick` 有單元測試。
  - 新增 `candidate_selection_is_worker_invariant`。
  - 和 P1–P3a 合起來的 wall：seven_segment ≤ 110 s，segment_a ≤ 75 s，pinned ≤ 30 s。user time 不超過今天的 1.5 倍。
- **風險**
  - dominance 可能讓只在一項上明顯更好的候選出不了貨。這是刻意的保守選擇，見開放問題 1。
  - 沒有 P1 的話，成本是 2–4 倍。
- **工作量**：M｜**依賴**：M1、P1 必要；P2、P3a 建議先做。這項取代「把 fabric 排到 nested 前面」的調順序做法，也取代原本的 D3.0。

#### P4. 不組 world 就先檢查 fabric 衝突（P4a 先做，P4b 低優先）
- **問題**
  - `tightest` 每一步都呼叫 `build`（`F/packed_node.rs:1768`），先 compose 再 plan（`F/parent.rs:873-877`）。但 plan 只需要 `packed`，`lid` 來自 `packed_canvas`（`F/parent.rs:741`）。
  - `plan_clash` 只回傳第一個衝突（`F/fabric.rs:335-374`），每步只加寬 `PITCH = 3`（`F/fabric.rs:40`，`F/packed_node.rs:1785`）。
  - 今天這段不到 wall 的 1%。
- **做法**
  - **P4a**（S）：從 `route_packed_trunks_on` 抽出 `fabric_plan`，提供只 plan 不 route 的入口。D1.1 第 3 步需要它，所以排在 D1.1 之前。
  - **P4b**（M）：
    - 把 `build`（`F/packed_node.rs:579-862`）拆成 `prepare` 和 `route` 兩段。
    - 迴圈內只跑 plan 加 `plan_clash`。
    - 刪掉 `fabric_checked` 這個 `Cell`（578、876-878 行）。
- **成功標準**：產物不變。P4b 之後每個 fabric node 只 compose 1 次，走 full fallback 時 2 次。tightest ≤ 50 ms。
- **風險**：`build` 約 280 行，拆的時候容易漏掉 pinned offset 的細節。
- **工作量**：P4a S，P4b M｜**依賴**：P4a 無；P4b 在 D1.1 和 D1.3 讓步數變多之後才做。

#### P5. router 內迴圈的資料結構（看 M2 結果）
- **問題**
  - `anchor_is_free_for_typed`（`routing.rs:2389-2426`）每次呼叫要查 `BTreeMap<Anchor, PhysicalReservation>`（`routing.rs:277-279`）約 15 次。
  - 其中 12 次來自 `keep_out_typed`，而它每次都配置一個 `Vec`（`routing.rs:2454-2468`）。
  - A\* 的 `previous` 和 `visited` 也是 `BTreeMap`（`routing.rs:2969`、`4011`）。
- **做法**（每一步都必須逐位元不變）
  1. `keep_out_typed` 改回傳 `[Anchor; 12]`。
  2. 對只做查找的 map，加一個密集索引，做法仿照 `endpoint_keep_outs`（`routing.rs:285-304`）。`BTreeMap` 仍是唯一的真相來源。
  3. frontier 維持 `BTreeSet<SearchState>`。
- **成功標準**：world 逐位元不變，expansion 數相同，葉階段 wall 降 ≥ 30%。
- **風險**：`routing.rs` 有 7,321 行，planner 也共用，任何語意偏差都會改變所有輸出。
- **工作量**：L｜**依賴**：M2 顯示 `route_guided` 佔葉時間 ≥ 60% 才做。

#### P6. root world 的重複認證（看 M2 結果）
- **問題**：packed root 在 node 層認證一次（`F/packed_node.rs:1025`），adapter 放上 lever 和 lamp 之後又認證一次（`F/packed_recursive.rs:700`）。Q1 之後每個候選都要付兩次。
- **做法**：只有 node 認證 ≥ 3 s 才做。
  - root 那一層跳過 node 認證，改由 adapter 對真正出貨、帶 harness 的 world 做唯一一次認證。
  - 由型別保證：root 專用入口回傳「未認證的 layout」，只有 `adapt_packed_root` 能把它轉成產品。這樣 `F/leaf.rs:112-122` 的 `certificate` 不變式仍然成立。
- **成功標準**：每個出貨的 root world 恰好認證一次，而且就是出貨的那個 world。尾段降 ≥ 35%。
- **風險**：錯誤訊息的時點會往後移到 adapter。
- **工作量**：M｜**依賴**：M2。

---

### 3.B 密度

**現況**（量 `viewer/baked/*.litematic`）
- segment_a：y12–19 有三層、相距 3，是 lid fabric 的特徵，佔 3,634 blocks（39%）。x114–153 這 40 欄地面層是純 seam。
- decoder：
  - pin 矩形是 x68–112、z24–144，矩形內只有 1,270 blocks。torch 分布在 x193–423。
  - x268–367 這 100 欄是一條沒收窄的 seam，上方有 5,636 blocks（28%）。
  - x128–187 是 feet 和 port pad。
  - litematic 的 EnclosingSize 是 500×21×148，包含空氣。
- 舊的 density report 顯示：
  - seven_segment 的 seam 佔 halo 跨距的 56%，trunk 佔 blocks 的 70%。
  - nested 失敗改走 fabric 時多 43%（9,383 vs 6,565）。

**pad 幾何評估**

根因：
- pad 是沿 x 的直梯（`F/fabric.rs:115-142`），所以 comb 要從 `pad_reach` 之後才能開始（`F/fabric.rs:153-158`、`204-208`）。
- `interleaved_shifts` 只在 `0..2*PITCH` 範圍內找位移，找不到就回 0（`F/packed_node.rs:1818-1843`，1840 行）。
- 兩面的端點列同列時，兩面的 pad 迎面相撞，seam 只能加寬回 full（1780-1786 行）。

| 方案 | 同列相撞 | seam 寬 | 結論 |
|---|---|---|---|
| 今天的 x 向 pad | 可錯開時解決；同列時不行 | 可錯開 17；同列退回 39+3(nA+nB) | 保留為第一候選 |
| z 向 pad（D1.3） | 可以解決 | 約 11+3(nA+nB)，與 climb 無關 | 採用 |
| coil riser（`plan_riser`，`src/compile/routing/riser.rs:210`） | 部分 | 7×7 盒子互相重疊 | 不採用：盒子互相重疊，結果依規劃順序而變。nested 的 `coil_egress` 已經在用它（`F/parent.rs:2032`），D3.1 也建立在它上面 |
| E 層依側分高度 | 不能 | — | 不採用：兩面的高度曲線必然相交 |

#### D1.1 `plan_clash` 只把 seam 造成的衝突算給 seam
- **問題**
  - `tightest` 一遇到 clash 就加寬 `seam_at` 指到的 seam。加寬到 full 仍 clash 時，所有 seam 一起退回 full（`F/packed_node.rs:1782-1784`、`884`）。
  - full strip 本身從不檢查（876-878 行；`F/parent.rs:876` 用 `checked.then`）。
  - 4 葉牆面的 trace：shifts 是 `[0,3,0,3]`，代表列可以錯開。但 seam0 從 18 加寬到 89，連續 25 次都停在同一格 `Anchor{181,23,38}`，最後三個 seam 全部回到 `[89,98,86]`。這一格不隨 seam 移動，所以不是 seam 造成的。
  - 2 葉案例：shifts 是 `[0,0]`，gap 從 18 加寬到 131（full）。
- **做法**
  1. `plan_clash` 改為回傳 `BTreeSet<Clash>`，其中 `enum Clash { Pair(usize, usize), Halo { trunk: usize, chunk: ChunkId } }`：halo 入侵是單一 trunk 落進 child halo（`F/fabric.rs:355-357`），沒有第二條 trunk。保留第一個衝突格給 trace。
  2. `PackedConnectionError::FabricClash`（`F/parent.rs:161`）改為攜帶這個集合。
  3. 進迴圈前，先對 `self.full()` 只 plan 不 route，得到 full strip 本來就有的衝突對，當作基準。
  4. 迴圈內只把基準以外的新衝突算給 seam。`checked: bool` 改成 `Option<&BTreeSet<Clash>>`。
  5. trunk 的索引依訊號順序決定，結果仍是決定性的。
- **成功標準**
  - 4 葉牆面不再出現「加寬到 full、衝突格不變」的序列，gaps 總和 < 273（推估可到 90 以下）。
  - 6 案都 certify，blocks 和 ticks 不變差。
  - decoder 維持 blocks ≤ 20,410、ticks ≤ 190。
- **風險**：容許的衝突對收窄後可能變成真的耦合。認證會攔下來，但 node 認證在選定 layout 之後才做（`F/packed_node.rs:1025`），失敗就是整個 producer 失敗，不會自動退回 full。由 Q1 的下一個候選接手；若要退回 full，需要在 1025 行加 retry。
- **工作量**：S｜**依賴**：P4a（只 plan 不 route 的入口）。

#### D1.2 port 與 feet 間隙只算真正需要的面
- **問題**
  - `east` 取所有 child 的最大值（`F/packed_node.rs:553`）。`ports` 把所有 port 都算進去（549 行），但真正由 node 建的 port 只有兩種：682 行的輸入和 738 行的輸出。
  - 實測 port gap 109、feet gap 85，改用最後一個 child 的 reach 可省約 24 欄。
- **做法**：只在 unpinned（`room.is_none()`）時，553 行改用 `order.last()` 的東面 reach，549 行的計數改用和 682、738 行相同的判斷式。pinned fabric 也會走到 548 行的 `if fabric` 分支，所以 pinned 保持原算法。
- **成功標準**：unpinned seven_segment 的 strip 縮短 ≥ 30 欄，blocks −2~4%。pinned decoder 逐位元不變。
- **風險**：comb 太窄時會出現 `FabricClash`，但 full 仍是最後的候選。
- **工作量**：S｜**依賴**：無。

#### D1.3 z 向爬升 pad
- **問題**：同列 seam 的寬度是 `2·pad_reach(climb) + 7 + 3(nA+nB)`，其中 `pad_reach` 隨 climb 線性成長（`F/fabric.rs:145-148`）。decoder 那條 100 欄的 seam 佔 blocks 的 28%。
- **做法**
  1. `FabricEnd` 加 `style: PadStyle { Along, Across }`。
  2. Across 模式下的 `pad()` 與 `leg()`（`F/fabric.rs:115-142`、`260-283`）：
     - 保留 4 格 approach，符合 runway 契約。
     - 在地面層走到私有欄 `+5+3k`，k 用 `F/fabric.rs:203-212` 的 z 序。
     - 沿 z 直梯爬到 Z 層（lid+5），每 `LANDING_EVERY` 層一段平台。
  3. Across 面的 `face_reach` 改為 `5+3n+3`。
  4. 列選擇的 `clear` 條件（`F/fabric.rs:243`）對 Across 端改為 `|R−z_t| ≥ 爬升長度`。
  5. 新增 kernel `k7_z_stair_over_foreign_ground_run_and_adjacent_stairs`，比照 `tests/fabric_kernels.rs:344`、`417`。
  6. `StripShape`（`F/packed_node.rs:1706-1790`）逐 seam 依固定順序只做 `plan_clash`：`[Along 且可錯開][Across/Across][Along/Across][Across/Along][full]`。
- **成功標準**
  - 同列 seam 寬度 ≤ `11+3(nA+nB)+PITCH`。
  - decoder 那條 seam ≤ 78 欄，blocks −5%。
  - seven_segment 在 D1.2 之後再降 8–14%，ticks 不增。
  - 高度仍是 lid+9。`F/fabric.rs:471` 的測試加入 Across 變體後通過。
  - 1 vs N 一致。
- **風險**
  - 只有自家 simulator 背書，C3 要補一條 vanilla 探針。
  - climb > 12 時平行的梯子不再保持 dy=3。
  - comb 的 3n 仍然佔 seam 的大部分。
- **工作量**：M｜**依賴**：D1.1。

#### D2 葉片切分：貪婪取最大可認證粒度
- **問題**
  - `flat_leaves` 一律對半切（`F/packed_recursive.rs:342-351`，`split_of` 在 `F/recursive.rs:1013-1015`）。84 gates 切成 4×21，但 3×28 就已經 ≤ 32。
  - 被拒的葉對半切到單一 gate 為止，從不合併（`F/packed_recursive.rs:445-475`）。seven_segment 因此從 4 片變成 8 片（21,21,11,10,11,3,2,5）。
  - 每多一片葉約多 2,250 blocks，再加一條 seam。
  - 舊量測：貪婪切成 5 片是 −15.5%，3×28 是 −22%。
- **做法**
  1. `partition.rs` 新增 `partition_sizes(netlist, parent, sizes)`，泛化 211-217 行和 253-254 行。`partition()` 改成均等切點的包裝。未使用的 primary input 固定掛在第一塊（240-246 行），`ChunkId` 仍是決定性的。
  2. `flat_leaves` 沿 T2 選出的順序貪婪前進：
     - 候選大小是 `[s0, s0−1, s0+1, ⌈3s0/4⌉, ⌈s0/2⌉, ⌈s0/4⌉]`，其中 `s0=⌈r/⌈r/grain⌉⌉`，每個候選都套上 `min(grain)`（`TERMINAL_GATES`），例如 r=32 時 `s0+1=33` 截成 32。
     - 一個位置的候選用 `run_indexed` 並行建，取清單順序中第一個成功的。
     - 全部失敗才對 `⌈s0/4⌉` 呼叫 `build_leaf_finer`，保留 typed refusal（相關測試在 `F/packed_recursive.rs:1661`、`1747`、`2032`）。
- **成功標準**
  - seven_segment 葉片 ≤ 5 片，blocks ≤ 19,700，ticks ≤ 198。
  - 每案仍 ≤ 180 s（取代舊稿的「≤ 2 倍今天」）。
  - segment_a 不變差。C5 sweep 通過。
- **風險**
  - 一個被拒的候選要 30–100 s，而且各位置之間必須串行。靠 P1 快取和位置內並行壓住。
  - 可行性對大小不單調。
- **工作量**：M｜**依賴**：P1、P2；和 T2 共用 `flat_leaves`（見 T2）；開放問題 4 的決定（交接規則禁止藉調 partition/grain 避開問題）。

#### D3 nested packed（只在 Q1 報告顯示 nested 候選會勝出或差距很小時才做）
- **D3.1 從 lane 爬升推導 `port_gap`**（S）
  - 問題：`port_gap` 寫死為 10（`F/packed_node.rs:557`，用在 686、745 行），每層 nested 多出 13–14 欄。
  - 做法：若 `coil_egress` 能把爬升收在 `RISER_REACH=3` 之內（`F/parent.rs:1997-2066`），就依序試 `[7, 10]`。
  - 標準：每層省 ≥ 3 欄，其他案例逐位元不變。
  - 風險：gap 7 撞到 egress 時多一次 layout 失敗。
  - 依賴：Q1 報告顯示 nested 值得做。
- **D3.2 skyline 接觸候選**（M，實驗性）
  - 問題：`candidate_translations` 只做角落接觸（`F/packing.rs:1340-1371`；aligned 候選在 1114-1158 行）。42-gate node 留下 78×99 的空角，84-gate root 只有 2 種 layout，兩種都撞 queue limit。
  - 做法：在 aligned 之後加 bottom-left skyline 接觸，用 `BTreeMap` 去重。rank 0 仍然是 `pack_free_leaves`（1048-1085 行）。
  - 標準：42-gate node 的平面面積 −15%，seven_segment nested root 至少多 2 種 layout。
  - 風險：layout 越密越容易撞 queue limit，每次 route 1.5–12 s。
  - 依賴：Q1 報告；P3a 先把 layout 失敗的成本壓低。
- **D3.3 halo 對 occupied 互測**（M，低優先）
  - 問題：packing 要求兩邊的 halo 不相交（`F/packing.rs:1028-1033`、`1068-1073`；halo 定義在 `F/leaf.rs:521-546`、`598-602`），間距被算兩次。
  - 做法：改成 halo 對 occupied∪access 互測。trunk keep-out 仍用完整的 halo 聯集。`reserve_packed_children` 只保留 occupied（`F/parent.rs:2458-2493`）。
  - 標準：面對面端點之間的空欄從 6 降到 4。
  - 風險：halo 互測放寬後，trunk 可能擠進耦合範圍；由完整 halo 的 keep-out 和認證擋。
  - 依賴：Q1 報告；D3.2 之後。

#### D4 pinned room：先疊層，再有上限地往側邊長

先評估：
- pin 矩形是 45×121，房間 z 方向可用深度 105（`extent` 在 `F/packed_node.rs:1528-1533`）。
- 葉的 z 跨距 76–84，x 寬 80–100。放不進矩形；沿 z 折成兩列需要 ≥ 2×76 加 comb，也放不下。所以原本 S3 提的 shelf fold 刪除。
- 可行的是疊層：certification 只要求 dust、二極體、torch 下方有支撐，石頭可以懸空（`F/certification.rs:637-645`）。

- **D4.0 量測 overhang 並立上限參數**（S）
  - 問題：
    - `PinnedRoom::of`（`F/packed_node.rs:1463-1512`）把沒有 caller 的面標成 open。
    - `extent()` 在 open 面回 `None`（1520-1539 行）。
    - `place()` 在遠端沒有上限（1553、1586-1600 行）。
    - footprint `(370,90)` 放在 `(93,32)`，本體一直延伸到 x≈462。
  - 做法：
    - 新增 `pinned_overhang`：footprint 在各 open 面超出矩形的最大距離，進 trace 和報告。acceptance 斷言它 ≤ 今天的值。
    - 在 `PINNED_MARGIN`（`F/packed_node.rs:490`）旁邊加 `ponytail:` 校準參數 `PINNED_OVERHANG_LIMIT`。初值公式 `max(寬, 深) + 2·PINNED_MARGIN`（decoder 算出 136），但數值等 D4.1 量測之後再定。這一步先不強制。
  - 標準：報告有這個數字，產物逐位元不變。
  - 風險：低，只加報告。
- **D4.1 pinned fabric 疊層**（L）
  - 問題：只有一層（`F/packed_node.rs:1959-1960`、`F/recursive.rs:835-837` 的 ponytail 註解）。
  - 做法：
    1. `StripShape` 加 `floors`，依 dataflow 順序把連續的葉分到 F 層。`fabric_strip`（`F/packed_node.rs:1867-1894`）的 y 位移為 `floor·(最高 child + 3) − low.y`。
    2. `fabric_reach` 的 lid 取最頂層。
    3. F 取最小值，使每層長度 ≤ max(開放軸寬度, 最寬葉加 comb)，上限 `F_MAX = 3`。依 `[F, F−1, …, 1]` 嘗試，1 就是今天的產物。
    4. 下層 pad 不能穿過上層 halo，由 `plan_clash` 擋。
  - 標準：
    - decoder 的 `pinned_overhang` 比今天少 ≥ 40%，體積 ≤ 0.8M，blocks ≤ 20,410，ticks ≤ 209（Phase 2 要回到 ≤ 190）。量完後定下 `PINNED_OVERHANG_LIMIT` 的值。
    - certify 通過，`tests/build_circuit_pins.rs` 通過，1 vs N 一致。
  - 風險：下層多爬約 13 層，ticks 小幅上升。高度從 20 升到約 33。需要 dy≥3 的 kernel。
  - 依賴：D1.1、D1.3。
- **D4.2 N/S 面端點直接進 fabric，拿掉 feet**（M–L）
  - 問題：
    - fabric 只接受 E/W 面（`F/fabric.rs:61-66`、`179-184`），因為 `along()` 只在 x 軸有值（103-108 行）。
    - 牆面 pin 朝北，得先各自搜到一個 foot（`F/parent.rs:825-858`、`879-891`；feet 在 `F/packed_node.rs:554-555`、`824-826`）。x68–187 約 120 欄都是 pin 腳與 foot pad。
  - 做法：
    1. 新增 K2 kernel：沿 z 的相鄰 ramp，以及 V2/V3 旁的平行軌道（`docs/fabric-plan.md:34-38`），兩種極性都要。
    2. `along()` 改成軸加方向。N/S 面的 column 用端子自己的 x；疊在同一 x 的 pin 以 3 格側移錯開。`pad()` 已處理 N/S 的步進（`F/fabric.rs:92-100`）。
    3. `fabric_reach`（`F/packed_node.rs:1657-1700`）回傳四個面的 reach。`fabric_strip` 依 north reach 平移 child。
    4. 補測試 `north_and_south_faces_plan_clean` 和 `mixed_faces_keep_l1_three`。
  - 標準：矩形內的 torch > 0（今天 0/30），`pinned_overhang` 再縮 ≥ 60，certify 和規則掃描都通過。
  - 風險：wall-design 列的下降段未保留問題仍在，`terminal_geometry` 只處理爬升。
  - 依賴：D1.3。
- **D4.3 grain 迴圈先加層再減半，並修正每 gate 面積下界**（S）
  - 問題：
    - 房間不足時 grain 減半（`F/packed_recursive.rs:299-302`、`412-415`），結果葉變多、strip 變長（`docs/fabric-plan.md` 寫明「FabricCapacity never halves the grain」）。
    - `MIN_LEAF_AREA_PER_GATE = 344`（`F/packed_node.rs:1611`）是用 pitch 6 量的，pitch 4 只要 199。這會讓 `pinned_floors_short`（1908-1915 行）誤判，提前轉去 allocation。
  - 做法：`short` 時先試 F+1，到 `F_MAX` 才減半。344 改為 199，或像 `pinned_space`（1922-1954 行）那樣用實建的 footprint 推導。
  - 標準：pinned 測試通過（`F/packed_node.rs:2470` 更新期望值），封閉房間改走疊層。
  - 風險：344 改成 199 會讓原本被提早拒絕的房間進入完整嘗試，失敗成本變高。
  - 依賴：D4.1；開放問題 4 的決定。
- **D4.4 強制 overhang 上限**（S）
  - 問題：D4.0 只量不擋，本體仍可無上限延伸。
  - 做法：`place()` 拒絕超限的 footprint，回傳 layout-dependent 的 `PinnedRegionTooSmall`。
  - 偏好順序：矩形內 → 疊層 → overhang ≤ 上限 → typed refusal。不允許無上限延伸。
  - 標準：所有 footprint 都滿足 `pinned_overhang ≤ PINNED_OVERHANG_LIMIT`（D4.1 定值），decoder 仍通過 certify。
  - 風險：上限太緊時，原本能做的 pinned 案例會變成 typed refusal。
  - 依賴：D4.1、D4.3，以及開放問題 2 的決定。

---

### 3.C 延遲（settle ticks）

**歸因實測**

方法：scratch probe 重現最差 transition，取每個 gate「最後一次變化」的時間，依 `RecursiveDiagnostics.leaves` 判斷每一跳在 leaf 內還是跨 leaf。probe 的最差值和 acceptance ticks 一致。

| case | 現在 / legacy | 關鍵路徑 gates | 跨越次數 | lead | leaf 內 | 跨越 | tail |
|---|---|---|---|---|---|---|---|
| seven_segment | 198 / 98 | 10 | 3 | 8 | 42 | **138**（42 / 46 / 50） | 10 |
| segment_a | 124 / 72 | 10 | 2 | 8 | 32 | **76**（34 / 42） | 8 |
| full_adder | 54 / 46 | 11 | 0 | 1 | 49 | 0 | 4 |

- **一次跨越約 44 ticks。**
  - 幹線本身是 16–26 ticks（每個 repeater 2 ticks：`routing.rs:3621-3629` 加 `src/redstone/simulator/component.rs:307-310`）。
  - 其餘 22–26 ticks 是兩側 leaf 內的腿。例如 L2 在 x=134 沿 z 走了 63 格。
- **幹線的 repeater 密度已接近上限**：每 12.9–14.3 格一個，上限是 15。瓶頸是長度。
- **分割讓深路徑穿過每一片葉**：`canonical_order`（`F/partition.rs:103-118`）加 `split_of`。改成輸出錐順序最多跨 2 次；切成 2×42 只跨 1 次。
- **glitch 不增加 settle。**
- **工具壞了**：`routing_cost_report` 在 `src/bin/routing_cost_report.rs:430-431` panic，錯誤是 `PartitionMismatch`。
- **預算模型**：ticks ≈ lead + leaf 內 + k×C + tail。seven_segment 要 ≤ 98，需要 k=1 且 C ≤ 38。**必須同時減少 k，並把 C 壓到 ≤ 30。**

#### T0 量測管線：每跳歸因
- **問題**
  - `attribution.rs:236-257` 仍假設靜態分割。
  - `TrunkSummary`（`F/attribution.rs:44-52`）沒有 sink 身分。
  - `timing::critical_path` 有破同分錯誤（`src/timing/mod.rs:411-438`）。
- **做法**
  1. `attribute_path` 改成接收 `owner`，新增 `leaf_owner(&[LeafDiagnostic])`。`production_leaf_chunks` 只留給測試。
  2. `F/packed_recursive.rs:719-734` 建 `TrunkSummary` 時加 `sink_chunks`。
  3. `routing_cost_report` 的 `--recursive-segment-a` 改為 `--recursive <fixture>`：
     - 移除 `verify_partition`。
     - 用「最後一次變化」反推關鍵路徑。
     - 每一跳印出：實測 ticks、幹線 repeaters×2、剩餘量、spine 列與 z 繞行。
     - 另外印出靜態指標「最多跨越次數」。
- **成功標準**：四個非 and4 案例都能跑完，lead+內部+跨越+tail 等於 acceptance ticks（用 assert 檢查），並重現上表。
- **風險**：低。
- **工作量**：S｜**依賴**：無（放在 Phase 0）。

#### T1 直接葉改用精確 refresh 政策（對應 full_adder）
- **問題**
  - planner 走 `route_with_policy`（`src/compile/planner.rs:5127`；`routing.rs:1440-1460`），也就是 `TotalStairs`。這個政策的文件自己寫了：樓梯多時大約每 3 格就 refresh 一次（`routing.rs:3667-3676`）。
  - 實例：g12→g13 距離 11 卻花 10 ticks，legacy 是 4。
- **做法**
  1. 先驗證：印出關鍵 route 的格數和 `terminals[].repeaters`（`planner.rs:7217`）。
  2. 在 `compile_root_leaf`（`F/recursive.rs:497-503`）的範圍內加 exact-refresh thread-local：
     - `reserve_policy` 改用 `LatestLegalCell`。
     - `routing.rs:1710` 的條件擴成 `strict_local || exact`。
     - C1 刪掉 own-crush 開關之後，這個旗標自成一個範圍，寫法照 `routing.rs:2245-2255`。
  3. planner 若因此被拒，比照 `Unsupported`（`F/recursive.rs:770`）落回 packed 路徑。
- **成功標準**：full_adder ≤ 46 ticks 且 ≤ 1,784 blocks。and4 ≤ 14/232，verilog:and4 ≤ 14/290。範圍外的 legacy 輸出逐位元不變。
- **風險**：原本能鋪的 route 可能改為被拒，落回 packed 後 ticks 可能變差。
- **工作量**：S–M｜**依賴**：排在 C1 之後。C1 會刪掉 own-crush 開關，exact-refresh 自建範圍，不共用。

#### T2 時序驅動的切分順序
- **問題**：照拓撲序切片（`F/partition.rs:70-130`），深路徑穿過每一塊。
- **做法**
  1. 新增 `cone_order(netlist)`：從每個輸出做 DFS postorder，約 25 行。
  2. 新增 `max_crossings(netlist, assignment)`，約 20 行。
  3. `flat_leaves`（`F/packed_recursive.rs:332-358`）和 `build_leaf_finer` 以均等切點比較兩種順序，取 `max_crossings` 較小者，同分用 canonical。D2 的貪婪切分沿著選出的順序走。
- **成功標準**：靜態指標 seven_segment 從 3 降到 2。實測 ≤ 160 ticks，blocks ≤ 21,847×1.02。6 案都 certify，1 vs N 一致。
- **風險**：新的葉可能被拒而觸發修補拆分，反而增加跨越。這時不另加 fallback。
- **工作量**：M｜**依賴**：T0、D2；開放問題 4 的決定。要做到 k=1 需要 ≥ 42-gate 的葉。

#### T3 關鍵邊界訊號在 leaf 內對齊
- **問題**
  - placer 只替 leaf 內部零 slack 的邊加權 ×4（`F/placement.rs:1327-1331`），邊界的 `PrimaryInput`／`DeclaredOutput` 一律不算 critical。
  - `colour_intervals`（1231-1243 行）分配 track 時不看消費端，所以邊界 interface 的 z 和讀它的 gate 無關。
- **做法**
  1. 用 root netlist 算 `critical_boundary: BTreeSet<String>`（gate level slack，每次跨越加罰分），約 20 行。
  2. 經 `synthesise_free_leaf` 傳進 `SeedPlacementRequest`。
  3. 在 1331 行把這些邊界邊也當作 critical。在 `colour_intervals` 裡，critical 的 track 排最前，並貼近消費 gate 的重心。
- **成功標準**：每次關鍵跨越的剩餘量 ≤ 12 ticks（今天 22–26），seven_segment 再少 ≥ 15 ticks，leaf blocks 變化 ±3%。
- **風險**：`interleaved_shifts`（`F/packed_node.rs:1818`）依 interface z 算的 seam 會跟著改變。
- **工作量**：M｜**依賴**：T0；T2 可選。

#### T4 fabric 關鍵幹線優先選 spine 列（先量再做）
- **問題**：spine 列從 `row_base = 1`（`F/parent.rs:872`）起，用 left-edge 演算法堆疊（`F/fabric.rs:215-256`），trunk 依名稱排序（`F/parent.rs:505`、`709`）。
- **做法**：只在 T0 顯示某條關鍵幹線繞行 ≥ 15 格時才做。依 criticality 排序幹線，每條從兩端 z 範圍之內的列開始試。`plan_clash` 不動。
- **成功標準**：繞行 ≤ `PITCH`，ticks 減 2–8，blocks ±1%。
- **風險**：改變 spine 列的分配可能讓 `plan_clash` 多出衝突，seam 被迫加寬。
- **工作量**：S｜**依賴**：T0、T3。

#### T5 leaf pin-to-pin 靜態時序（延後）
- **問題**
  - leaf 的 `CompleteCandidateCertifier` 已經算出時序（`F/certification.rs:1118-1119`），但在 `F/leaf.rs:254-258` 被丟掉。
  - `static_routed_delay` 被測試鎖住等於 settle（`F/recursive.rs:1798-1801`）。
- **做法**
  1. 新增 `RealisedTimingGraph::pin_to_pin()`，沿用 `topological_order`（`timing_graph.rs:304`）。
  2. `FreeLeafArtifact` 加 `pin_delays`。
  3. root 估計值填進 `F/recursive.rs:664-666`，並改掉 1798-1801 行的測試。
- **成功標準**：估計值與（實測 − tail）相差 ≤ 4 ticks。
- **觸發條件**：T3 認錯關鍵幹線，或 Q1 需要在認證前比較 ticks。
- **風險**：靜態估計和實測不符時可能誤導選擇，所以只當參考，不取代實測。
- **工作量**：M｜**依賴**：T0。

---

### 3.D 正確性與可信度

#### C0 查清 6 個 lib 失敗
- **問題**：1128 passed / 6 failed / 71 ignored。

  | 測試 | 位置 | 失敗內容 |
  |---|---|---|
  | `a_negotiation_that_has_not_converged_returns_an_error_and_not_a_plan` | `planner.rs:25744` | 序列是 `[8,5,5,0]`，預期 `[8,5,6,0]` |
  | `a_pinned_output_that_also_feeds_a_gate_is_isolated_at_that_gate` | `planner.rs:13007` | 前提失效 |
  | `an_unpinned_grown_and4_is_the_world_it_always_shipped` | `planner.rs:10434` | world digest 改變 |
  | `legacy_adapter_keeps_the_all_pinned_full_adder_routes_byte_exact` | `planner.rs:8390` | g4 route 不同 |
  | `the_keep_out_footprint_per_circuit_is_what_is_recorded` | `planner.rs:25396` | footprint 數字改變 |
  | `the_reported_stale_dust_case_settles_clean` | `src/compile/resettle_differential.rs:223` | `(57,2,99)` 是 0，預期 10 |

- **做法**：用 `git worktree add` 開 9dd064f 跑這 6 個測試，再讀 `git diff 9dd064f cc43241 -- src/compile/routing.rs src/compile/planner.rs`，找出是哪條規則改的。
  - 是回歸就修。
  - 是快照漂移，就在新世界通過 certify 且真值表正確時重新釘數字，並在註解寫明原因。
  - `a_pinned_output_…` 改用固定世界或固定 plan 重建原本的情境，否則刪掉並註明。
  - 最後一個交給 C2。
- **成功標準**：每個失敗都有原因紀錄，lib 0 failed（和 C1、C2 一起完成）。
- **風險**：重新釘快照可能把真的回歸合法化，所以每次都要附證據。
- **工作量**：S–M｜**依賴**：無，最先做。

#### C1 planner 路徑出貨 dust-on-dust
- **問題**
  - 規則開關是 thread-local，預設關閉：`REFUSE_OWN_CRUSH`（`routing.rs:2229`）和 `refusing_own_crush`（2245-2253 行）。只有 `seed_rules || own_crush_refused()` 時才拒絕（3030、3290 行）。
  - 只有 `compile_root_leaf` 打開它（`F/recursive.rs:501-503`）。
  - 預設的 `compile()`（`src/bin/build_circuit.rs:799`、`src/bin/mc_dump.rs:362`）走 `compile_planned_within`（`src/compile/mod.rs:7996-8025`），沒有打開。`compile_grown`（7937-7967 行）和 DFF 分支（7556 行）也沒有。
  - 缺陷有文件記載（`planner.rs:2708-2733`）。測試 `a_route_lays_dust_on_its_own_committed_stone`（`planner.rs:14249`）把 3 格釘死（14275 行）。
  - planner 的 verifier 沒有支撐檢查（`src/compile/verification.rs:54-59`、`115-119`）。
- **做法**（在共用處修）
  1. 刪掉 `REFUSE_OWN_CRUSH`、`own_crush_refused`、`refusing_own_crush`，3030、3290 兩處一律拒絕。同時刪掉 `F/recursive.rs:501` 的包裝。
  2. `unsupported_component` 移到 `compile::verification`，加一個 `RealisedWorldVerifierCheckId::Support`，接進 `verify_legacy_candidate`。`certify_root_world` 改呼叫同一個函式。
  3. 新增測試：對 `compile`、`compile_legacy`、`compile_grown` 產出的全部 reference 和 verilog 電路跑 Support 檢查。
  4. 逐一讀過再重錄：
     - `tests/reference_circuits.rs:557`。
     - `planner.rs:14249` 改為斷言 0 並改名。
     - `planner.rs:14314` 的註解數字。
     - README 的表。
- **成功標準**：
  - `grep -rn REFUSE_OWN_CRUSH src` 為 0。
  - Support 在三條路徑上 0 違規。
  - 6 個 acceptance 的 fingerprint 不變。
- **風險**
  - planner 可能 route 不出來，一般電路會 fallback 到 legacy。
  - DFF 沒有 fallback（`mod.rs:7556-7561`），可能從「出貨壞世界」變成「編譯失敗」。先跑 `tests/systemverilog_dff.rs` 和 `tests/m3_m4_dff_yosys_differential.rs`。
  - `compile_legacy` 的輸出若改變，baseline JSON 仍凍結不重抓（開放問題 5）。
- **工作量**：M｜**依賴**：C0。

#### C2 simulator 自我一致性 gate 修復
- **問題**
  - `the_reported_stale_dust_case_settles_clean`（`resettle_differential.rs:205-243`）的世界每次都由 planner 即時產生（146-191 行）。第一個斷言失敗後，真正的 differential 斷言（238-242 行）根本沒執行。
  - 現在只剩 `and4s_full_sweep_is_differential_clean` 在守。
- **做法**
  1. 比對 `(57,2,99)` 在綠燈 commit 和現在的差異。
  2. 若是漂移：在綠燈 commit 用 `litematic::save` 產出 `tests/fixtures/stale_dust_reported.litematic`，測試改成 `litematic::load`。
  3. 若是回歸：先修 simulator。
  4. 對 full_adder 的 acceptance 世界，每個輸入向量跑一次 `resettle_differential`，放在非 ignored 的測試裡。
- **成功標準**：轉綠。暫時拿掉 `active_dust_networks` 的 incoming-edge walk 時必須轉紅。
- **風險**：若查出是 simulator 回歸，現有 certify 結果要重新評估。
- **工作量**：S｜**依賴**：無。

#### C3 真實 Minecraft 驗證
- **問題**
  - 規格記錄過兩次「全綠卻整個電路死掉」（`docs/superpowers/specs/2026-08-07-minecraft-conformance.md:8-13`）。
  - harness 已有：`conformance/circuit_conformance.py` 和 `probes.py`。缺口：
    - 它只吃 `mc_dump`，而 `mc_dump` 固定走 `compile()`，所以 0 個 fragment_synth 產物跑過真實遊戲。
    - `conformance/results/26.2.json` 有 13 項不符，之後沒重跑。
    - harness 斷言每個 INPUT 恰好一個 lever（`circuit_conformance.py:1117`），pinned 案例會直接失敗。
    - 計時用 wall-clock poll（899-902 行）。
  - Mac 上沒有伺服器。
- **做法**
  1. 使用者在 Mac 裝 JDK 25 和 vanilla 26.2，打開 `enable-rcon`。`eula=true` 必須由使用者自己寫。
  2. `mc_dump` 加 `--synth [--pins]`：用 `SynthesisBudget::Evaluations(0)`，同 `build_circuit.rs:783-790`；GATEOUT 取自 `F/api.rs:144`；pins 解析抽到 lib 共用。
  3. harness 支援 pinned 埠：輸入用 `setblock redstone_block|air`，對應 `mod.rs:7015-7022`；輸出放 lamp，對應 `mod.rs:7034`。
  4. 在 26.2 重跑 `probes.py`，逐項修探針或修 simulator 並補測試。
  5. 對 6 案跑 `circuit_conformance.py`，結果存 `conformance/results/synth/<case>.json`，內含 fingerprint。
  6. 用 `/tick freeze|step` 量精確 tick，和 `worst_settle_game_ticks` 比對。先只報告。
  7. 新增 `conformance/acceptance.sh`，不進 `cargo test`。規則：改變 acceptance fingerprint 的 PR 要附同一 fingerprint 的真實遊戲結果。
- **成功標準**
  - 26.2 探針 0 不符，或每個殘留項都有 simulator 修正和測試。
  - 6/6 案全部向量吻合，0 個 `RegionNotReady`。
  - 故意把一個 repeater 反向的世界要回報 `CircuitMismatch`。
  - 6 案總時間 ≤ 20 分鐘。
- **風險**
  - 可能揭露 simulator 真的錯了（這正是要找的）。
  - decoder 的 forceload 要分片（`circuit_conformance.py:523` 附近）。
  - 依賴使用者安裝伺服器。
- **工作量**：M｜**依賴**：使用者環境（開放問題 3）。

#### C4 出貨 litematic 的初始狀態與 dust 形狀
- **問題**
  - 狀態不自洽：
    - `viewer/baked/segment_a.synth.litematic` 裡 repeater 全是 `powered=true`，torch 全是 `lit=true`，4,200 個 dust 全是 power=0。
    - 原因：`wall_torch` 固定 `lit = true`（`src/compile/mod.rs:333-339`）；`structured_property_keys` 對 RedstoneWire 只寫 `power`（`src/formats/litematic.rs:112-114`、`374-376`）。
  - simulator 每個 tick 全域重掃（`src/redstone/simulator/mod.rs:436`、`555-561`、`570-580`、`690-720`），所以錯誤狀態會自己「癒合」。遊戲是更新驅動的，不會（`circuit_conformance.py:77-87`）。
  - 缺形狀屬性時遊戲預設 dot，dot 不對水平方向供能（`connectivity.rs:216-220`）。
- **做法**
  1. 新增 `settled_for_export`：所有輸入 off，跑 `run_until_stable`。在 `build_circuit` 和 bake 存檔前呼叫。certification 不動。
  2. `litematic::save` 依 `simulator::connectivity` 寫出 `north/east/south/west`。
  3. harness 加 `--paste` 模式：用檔案裡的狀態一次放完，模擬 Litematica 貼上。`mc_dump` 的 BLOCK 行加屬性欄。
- **成功標準**：
  - 6 個世界經 `settled_for_export` 後，再 `Simulator::new` 加 `step()` 一次，changed 為 0。
  - `--paste` 模式在 26.2 上 6/6 通過。
- **風險**：形狀規則寫錯會弄壞原本能用的檔案，必須用 `--paste` 驗證。
- **工作量**：M｜**依賴**：第 3 步依賴 C3。

#### C5 決定性覆蓋缺口
- **問題**
  - 已經證明的：
    - `F/recursive.rs:2629`（chain(33)，出貨入口）。
    - `F/recursive.rs:1755`、`2069`（allocating）。
    - `F/packed_recursive.rs:2112`。
    - `F/packed_recursive.rs:1017` 的 lid fabric 測試：ignored，約 8 分鐘，2026-09-27 通過。
  - `F/recursive.rs:3042` 走的是 allocating。
  - 6 個 acceptance 案例在出貨入口一個都沒證明過。`PinnedRoom` 完全沒有 worker 測試。
  - 出貨的 worker 數是 `min(available_parallelism, 8)`（`F/recursive.rs:452-461`），不同機器跑的 N 不同。
- **做法**
  1. 加一個 `#[ignore = "slow gate: …"]` sweep：6 案 × {1, 2, `many_workers()`}，比對 `canonical_world_fingerprint` 和 `candidate_fingerprint`。
  2. 加一個非 ignored 的小型 `PinnedRoom` worker 測試（2–4 gate），進 `check.sh`。
- **成功標準**：sweep 全部通過，並記錄單 worker 耗時。小測試在 `check.sh` 內。
- **風險**：單 worker 的 seven_segment 可能很慢，實測後決定是否只跑 1 vs N。
- **工作量**：S–M｜**依賴**：無（Phase 0 就要有，作為 P1、Q1 的保險）。

#### C6 `PhysicalReservationKind` 重構
- **問題**
  - `routing.rs:241-247` 的 `KeepOut` 同時表示三件事：
    - 實體方塊：`F/seed.rs:3473`、`3499-3516`；`planner.rs:5267-5273`。
    - 隔離用的空氣：`IsolatingOwners`，見 `F/parent.rs:943-946`。
    - 保留通道。
  - 意義靠 `air_keep_out`（`routing.rs:319-326`）、8 個魔術 owner（`F/parent.rs:127`、`132`、`2381`、`2398`、`2585`、`2593`、`2602`；基準值在 `routing.rs:402`）和側表拼出來。
  - 讀取點：`floorable`（414-422 行）、`keep_out_block`（425）、`route_owns_floor`（1852-1868）、`floor_is_forbidden`（2139-2163）、`floor_is_inert`（2172）。
  - 使用點共 94 處。
- **做法**
  1. 第一步逐位元不變：拆成 `Solid(BlockState) | Isolation | Reserved | Occupied`。映射方式完全照目前的行為（`treat_keep_out_as_air` 在 `F/parent.rs:2750`）。
  2. 刪掉 `AirKeepOut`、`IsolatingOwners`、`treat_keep_out_as_air`、`floorable`、`keep_out_block`。
  3. 第二步另開 PR：C1 之後再決定 `Occupied` 的去留。
- **成功標準**：fingerprint 和測試通過集合都不變，grep 上述符號為 0，每個 kind 各有一個測試。
- **風險**：94 處容易有語意微漂移。會和密度工作在 `parent.rs` 衝突。
- **工作量**：L｜**依賴**：C1；排在密度工作之後（Phase 3）。

#### C7 simulator 沒檢查的其他物理

| 候選 | 證據 | 處置 | 量 |
|---|---|---|---|
| 更新驅動 vs 全域重掃 | `mod.rs:436`、`555-561`；載入時信任存檔狀態（`mod.rs:288-293`） | C4；`probes.py` 加一條「沒有更新就保持過期」的探針 | S |
| 更新順序（非 locational） | `mod.rs:3` | C3 第 6 步 | — |
| torch burnout 的恢復 | `component.rs:26-30`、`149-157`；目前沒有探針 | 加 `torch_burnout_and_recovery` 探針 | S |
| 支撐檢查的涵蓋範圍 | Lever 走 `_ => true`（`F/certification.rs:637-644`）；從 y=1 開始（631 行） | 把 `lever_support_position` 移進 `unsupported_component`；y=0 改成斷言沒有元件 | S |
| 模擬距離 | decoder 約 32 個 chunk 長，預設模擬距離只涵蓋約 21 個 | 報告加 `chunk_span`，超過 21 印 WARN；真正的修正交給 D4 | S |
| QC、活塞、lamp 延遲 | `mod.rs:5-6`、`647-667`；`26.2.json` 相符 | 無 | — |

---

### 3.E 結構、工具與維護

#### S1 收斂 producer
- **問題**
  - `compile_with_cutoff` 依序試：直接葉（`F/recursive.rs:764-775`）→ nested 或 fabric 乘上 `LEAF_LADDERS` → pinned（`pinned_floors_short`，`F/recursive.rs:838-846`）→ allocation（`solve_subtree` 加 `compose`，`F/recursive.rs:901`、`911`；還有 `split_refused_child` 1142 行、`synthesise_node` 1454 行）。
  - 在 6 案中，allocation、nested、pinned packed 都沒有出貨過。
- **做法**（在 Q1 之後，依 `candidates` 報告決定）
  1. 移除 allocation。前提有兩個：D4.4 已上線，且使用者同意超出上限時回傳 typed refusal（開放問題 2）。
     - allocation 會把本體放到所有 pin 的南側（`F/allocation.rs:888`），違反 pinned 規則。
     - 把 `SignalContract`（`F/allocation.rs:199`）、`RootPort`（369）、`Prism`（163）、`root_placement`（558）、`normalise_root_pins`（619）移到 `contract.rs`。
     - 先把 `split_of`（`F/recursive.rs:1013-1015`）移到 `partition.rs`：`F/packed_recursive.rs:344`、`454`、`497` 和 `F/attribution.rs:249` 都在用，D2、T2 也建立在它上面。
     - 然後刪除 `allocate*`、`schedule.rs`、`F/parent.rs:2677` 的 `compose`、`compile_with_cutoff` 裡的 allocation fallback（`F/recursive.rs:896-994`），以及 `F/recursive.rs:996-1550` 裡 `split_of` 以外的部分。
     - 刪除只測 allocation 的測試（`F/recursive.rs:1692`、`1755`、`2214`、`3020`、`3041`）。
     - `F/certification.rs:224-249` 的測試改用 fabric 產物。
  2. 移除 nested 和 pinned packed：前提是在所有量測過的 netlist 上，Q1 報告顯示 fabric 的 `QualityKey` 都不輸 nested。
  3. 最終形態：直接葉 → fabric（pinned 或 unpinned）× ladder → typed refusal。
- **成功標準**：`rg -n 'allocate_with|fn compose' src` 為空，lib 全綠，1 vs N 一致。
- **風險**：語料之外的 netlist 可能只有 nested 或 allocation 做得出來。先用 diagnostics 蒐集更多統計再刪。
- **工作量**：allocation M，nested L｜**依賴**：Q1、D4.4、開放問題 2。

#### S2 fabric E/W 不變量
- **問題**：這個限制今天不會觸發。
  - 沒有 pin 時 frame 預設朝東（`F/placement.rs:1003`、`1016-1026`，測試在 2995 行）。
  - port 用 `plan.frame.forward`（`F/seed.rs:1709-1737`），所以 `interface_route_direction`（`F/leaf.rs:441-446`）只會給出 W 進、E 出。
  - pinned port 經朝東的 foot 接入（`F/parent.rs:837-851`）。
- **做法**：加一個測試，斷言 corpus 裡所有 free leaf interface 都是 E/W。`fabric_reach` 加 `debug_assert!`。完整的 N/S 支援在 D4.2。
- **成功標準**：測試通過，產物逐位元不變。
- **風險**：低。
- **工作量**：S｜**依賴**：無。

#### S3 target 目錄與 bake 腳本
- **問題**
  - `/Users/seith/Desktop/REDA/.cargo/config.toml` 設定 `target-dir = "target"`，9 個 worktree 共用同一個 target。
  - 這個 worktree 還有一個 11:36 的過期本地 `target/release/build_circuit`，而 scratch `bake.sh:5-7` 優先用它。
  - repo 沒有 bake 指令：`build_circuit` 寫到 `output/`（`src/bin/build_circuit.rs:821`）。
  - `verilog:*` 需要 `yowasp-yosys`（`src/frontend/mod.rs:402`），這裡沒有 `.venv`。
- **做法**
  - 新增 `tools/bake-viewer.sh`，寫法仿照 `check.sh`：
    - 預設 `REDA_PYTHON=$PWD/.venv/bin/python`。缺 `yowasp_yosys` 時提示 `uv venv --python 3.12 && uv pip install -r requirements.txt` 並結束。
    - 每案用 `cargo run --release --bin build_circuit -- <name> --synth [--pins …]`，複製到 `viewer/baked/`，印出 bbox 和 `git diff --stat`。
  - 刪掉本地 `target/`。
  - `README.md:101-102` 加一行：不要直接執行 `target/release/*`。
  - 預設不改成每個 worktree 各自一個 target。
- **成功標準**：
  - 乾淨的 worktree 一次重建 5 個案例。
  - 連續執行兩次後 `git diff --quiet viewer/baked` 成立。
  - `rg -n 'target/release' tools check.sh README.md` 為空。
- **風險**：輪流在不同 worktree 跑會觸發重編，約 19 s。
- **工作量**：S｜**依賴**：無（放在 Phase 0，因為量測需要正確的 binary）。

#### S4 測試健康
- **問題**
  - clippy 有 154 個 warning，其中 127 個是 `result_large_err`，來自 `SeedError`（`F/seed.rs:123-128`）、`PackedConnectionError`（`F/parent.rs:256`、`295`）等 6 個型別。另有 `F/packing.rs:1298` 多餘的 `mut`、`F/packing.rs:636` 未讀取的 `rank_zero`。
  - ignored：lib 71 個，`tests/` 7 個。其中：
    - 重複的測試：`tests/fragment_synth_acceptance.rs:224`。
    - 標著 "known:" 的已知失敗：`planner.rs:8969`、`14051`。
    - 慢但有意義的 gate：`planner.rs:10468`、`13790`、`F/attribution.rs:452`。
  - 兩個 fabric 量測測試合計 1,707.9 s。`check.sh:14` 跑全部，沒有快速版。
- **做法**
  - clippy：
    - 把大 payload 裝箱：`Box<SeedRepairRefusal>`、`Box<RouterFailure>`。
    - 機械性項目用 `cargo clippy --fix`。
    - `too_many_arguments` 依先例加 allow（`F/packed_recursive.rs:365`）。
  - ignored：
    - 刪掉重複的那個。
    - "known:" 的要嘛修好，要嘛刪掉並註明。
    - reason 統一加前綴 `measurement:`、`review:` 或 `slow gate:`。
  - `check.sh`：
    - 加 `--fast`（lib、clippy、wasm）和 `--slow`。
    - 每一段印耗時。
    - 缺 `yowasp_yosys` 時立刻失敗。
  - 三個品質紅燈保持紅燈。
- **成功標準**：`cargo clippy --all-targets -- -D warnings` 回傳 0。ignore 總數（lib 加 `tests/`，今天 71 + 7 = 78）≤ 70，且都有前綴。`check.sh --fast` 在 3 分鐘內完成。
- **工作量**：M｜**依賴**：C0；allocation 的測試隨 S1 刪除。

#### S5 repo 整理
- **問題**
  - `docs/handoff-2026-09-24/` 共 2.4 MB：
    - `evidence/` 1.7 MB，其中兩個 trace 檔權限是 0600。
    - `.rs` 原始碼副本共 692 KB。
  - 交接文件自己寫明這些會誤導搜尋（`docs/recursive-synthesis-claude-handoff-2026-09-24.md:191-202`）。
  - 未追蹤的檔案：`viewer/_wallshot.html`、`viewer/baked/debug/`（由 `viewer/index.html:3974-3976` 載入）。
- **做法**
  - `git rm -r docs/handoff-2026-09-24`。
  - 交接文件改成指向 `git show cc43241:…`，並標明已被 `docs/fabric-plan.md` 取代。
  - `.gitignore` 加 `viewer/baked/debug/`，刪掉 `_wallshot.html`。
- **成功標準**：`du -sh docs` 少 ≥ 2.3 MB，`git ls-files docs | rg '\.rs$'` 為空。
- **風險**：交接文件裡的引用會失效，要改成 `git show cc43241:…`。
- **工作量**：S｜**依賴**：無。

#### S6 viewer 相機
- **問題**：
  - `frameCamera`（`viewer/index.html:1677-1689`）只有一個對角視角：fov 60（1629 行），配 OrbitControls（1631-1636 行）。
  - 500×21×148 的 decoder 從對角看只剩一條細帶。
  - 面板裡已經有 clip box（264-298 行；`readClipInputs` 1909 行、`applyClip` 1937 行），可以直接當作區域。
- **做法**
  - 新增 `viewAlong(dir)`：
    - 取景框取目前的 `clipRange`。
    - 距離 = `max(halfW/tan(hfov/2), halfH/tan(vfov/2)) + halfDepth`。
    - 俯視時加一個很小的 z 偏移，避開極點。
  - 在 296-298 行那排加 Top / Front / Side / Fit 按鈕，鍵盤 7/1/3/F。
  - 長寬比 ≥ 3 時預設 Top-fit。
- **成功標準**：每個預設視角下，bbox 的 8 個角都落在 NDC [-1,1] 內，較大軸的範圍 ≥ 0.8。`check.sh:43-47` 通過。
- **風險**：低，只影響 viewer。
- **工作量**：S｜**依賴**：S3。

---

## 4. 建議順序

| Phase | 內容 | 退出條件 |
|---|---|---|
| **0 量測與加速**（產物逐位元不變） | S3 → M1、M2、T0、C5 → P1 → P2 → P3a | 6 案和 decoder 的 fingerprint 不變；3 次中位數：seven_segment ≤ 90 s、segment_a ≤ 65 s、pinned ≤ 22 s；報告行完整；T0 加總 assert 通過；1/2/N sweep 綠；M2 回答了 P3c、P5、P6 的三個問題 |
| **1a 選擇與 unpinned 密度**（D2 需要開放問題 4 的決定） | Q1 → P4a → D1.1 → D1.2 → D2 → D1.3；D3.x 依 Q1 報告決定 | 6 案都 certify，ticks 和 blocks 都不比今天差；seven_segment blocks ≤ 19,700（目標 16–18k）且 ticks ≤ 198；segment_a ≤ 9,305 blocks 且 ≤ 124 ticks，並以 Q1 報告的最佳候選為準（若 nested 勝出，目標 ≤ 7,000 blocks 且 ≤ 110 ticks）；每案 ≤ 180 s；`producer_revision` v2；viewer 重烤 |
| **1b pinned room**（D4.3 需要開放問題 4 的決定） | D4.0 → D4.1 → D4.3 → D4.2 → D4.4 | decoder `pinned_overhang ≤ PINNED_OVERHANG_LIMIT`（D4.1 後定值），體積 ≤ 0.8M，blocks ≤ 20,410，ticks ≤ 209；矩形內 torch > 0；規則掃描通過 |
| **2 延遲** | T1 → T2 → T3 → T4；T5 視觸發條件 | full_adder ≤ 46 且 ≤ 1,784（gate 綠）；seven_segment ≤ 140；segment_a ≤ 105；pinned ≤ 190；blocks +2% 以內 |
| **正確性**（從 Phase 0 起並行） | C0 → C1 → C2 → C3（需要使用者環境）→ C4 → C7 | lib 0 failed；Support 在所有 compile 路徑上 0 違規；26.2 探針 0 不符或逐項有解釋；6/6 真實遊戲向量吻合 |
| **3 收斂與清理** | S1 → C6；P4 跟在 D1 之後；P5、P6 依 M2；S2、S4、S5、S6 無依賴，可隨時插進去 | `check.sh` 只剩品質紅燈（品質工作完成後全綠）；clippy 0；只剩兩個 producer |

**品質目標總表**（ticks / blocks；各欄的收益不可直接相加）

| case | legacy | 今天 | Phase 1 後 | Phase 2 後 | 若葉 ≥ 42 gates 或走直接葉 |
|---|---|---|---|---|---|
| seven_segment | 98 / 16,244 | 198 / 21,847 | ≤ 198 / ≤ 19,700 | ≤ 140 | ≈ 92（k=1，勉強過） |
| segment_a | 72 / 6,416 | 124 / 9,305 | ≤ 110 / ≤ 7,000 | ≤ 105 | ≤ 72（k=0） |
| full_adder | 46 / 1,784 | 54 / 1,094 | 不變 | ≤ 46 / ≤ 1,784 | — |
| pinned decoder | — | 190 / 20,410 | ≤ 209 / ≤ 20,410 | ≤ 190 | — |

## 5. 不做的事與已排除的方案

| 方案 | 理由（實測或結構） |
|---|---|
| 無界房間的 2-D shelf fabric | trunk 格數 8,696，比單條共用 seam 的 7,843 還多 |
| decoder 的 shelf fold（原 S3） | 房間深 105，葉的 z 跨距 76–84，兩列放不下；改走疊層 |
| 依 net 親和度重排葉 | dataflow 順序已經是最小跨距 |
| lid 只降 1 | 只省 1.6% |
| 用 planner 建密葉 | 9 個 chunk 只 certify 了 2 個 |
| 自適應 channel 寬度 | 葉反而長大 66–100% |
| (3,3) pitch rung | 23-gate 的 segment_a 葉被拒 |
| certify 後逐步二分 seam | 最多約 45 次 route 加 certify；改用 `plan_clash` 的幾何階梯 |
| coil riser 當 pad、E 層依側分高度 | 見 §3.B 的評估表 |
| 調幹線 repeater 政策 | 已經每 12.9–14.3 格一個，上限是 15 |
| strip 依 dataflow 重排 | 關鍵路徑已經沿 x 單調；`dataflow_order` 用 `ChunkId` 破同分（`F/packed_node.rs:1125`） |
| 抑制 glitch | 增加的是 event 數，不是 settle |
| 改用「輸出到達」取代 settle | tail 只有 4–10 ticks，而且 baseline 用的就是 settle |
| 按時間截斷候選、認證前的啟發式剪枝 | 破壞決定性，或改變候選集合 |
| 提高 A\* 上限、第二個 router | 違反硬限制 |
| hypergraph 切割 | 精確均衡時贏不了拓撲切片，收益小 |
| 另寫獨立的規則檢查器、Paper 或 Fabric mod | 會複製同樣的假設；vanilla 加既有 harness 才是 ground truth |
| 預設每個 worktree 各自一個 target、viewer 正交相機 | 不需要；等真的有問題再加 |
| 現在就做 N/S fabric、node 級快取 | YAGNI；前者在 D4.2 需要時做，後者等 M2 證明需要 |
| PITCH 2、拿掉 E 層（原 D1.4） | 延後：PITCH 2（`F/fabric.rs:40`）估計再省 92 欄，但要改 L1≥3 規則並先在 vanilla 驗證 K1/K2；D1.3 若讓所有 pad 都走 Across，E 層就沒用了，高度可從 20 降到 17。兩者都等 D1.3 量測和 C3 之後再決定 |

## 6. 開放問題

1. **Q1 的選擇規則**：預設用 dominance / keep-better（`docs/fabric-plan.md:234-239`）：先出貨今天的產物，只有另一個候選 ticks 和 blocks 都不差時才換，字典序只在這些候選之間排序。要不要改成純 `QualityKey` 字典序（ticks 優先）？字典序可能挑到 ticks 較好但 blocks 超過 baseline 的候選，讓 gate 翻紅。
2. **pinned 房間放不下時**：回傳 typed refusal，還是保留今天的 allocation fallback？allocation 會違反 pinned 規則（`F/allocation.rs:888`）。S1 刪除 allocation 和 D4.4 強制上限都需要這個決定。
3. **真實遊戲環境**：能否在 Mac 裝 JDK 25 加 vanilla 26.2 並自己同意 EULA？C3、C4 驗證和「改變 fingerprint 要附真實遊戲結果」的合併規則都卡在這裡。
4. **seven_segment 要追平 98 ticks**，需要 ≥ 42-gate 的葉（k=1），超過今天的 32 grain。要允許更大的葉（會提高撞 A\* 上限的風險），還是接受 seven_segment 的 ticks gate 長期紅燈？另外，交接規則寫了「不藉調 partition/grain 避開問題」，D2 和 T2 屬於品質優化，是否同意納入？
5. **legacy baseline 本身的物理性**：baseline 由 `compile_legacy` 產生（`F/benchmark.rs:251`）。2026-09-27 已查 and4、full_adder、seven_segment 的 legacy 世界：懸空 0；segment_a 尚未查。若 segment_a 也乾淨，baseline 就是合法的比較標準。預設：JSON 凍結不改，C1 的 Support 檢查補查 segment_a 並在報告註明。
6. **DFF 路徑**：C1 之後 DFF 可能從「出貨壞世界」變成「編譯失敗」，可以接受嗎？
7. **每案 180 s 上限**：要當成正式的合併 gate 嗎？CI 的核心數和這台 Mac 不同，1 vs N 要不要跨機器比對 fingerprint？
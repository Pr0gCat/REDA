# fragment_synth 優化計畫

> 基準：main `fb74453`，與 `cc43241` 是同一棵樹，`git diff` 為空。本文所有 file:line 都以這棵樹為準。`F` 代表 `src/compile/fragment_synth/`。
> 量測條件：release 建置，使用獨立的 `CARGO_TARGET_DIR`，10 核 Mac。每案一個 process，指令為 `--exact --nocapture --test-threads=1`，外包 `/usr/bin/time -l`。每案只跑了一次，不是中位數。
> 原始紀錄放在 scratchpad `/private/tmp/claude-501/-Users-seith-Desktop-REDA--claude-worktrees-caveman-full-3b51d3/afa6a9f0-76f5-41fd-a860-f7be868bf89d/scratchpad/`（`plan-time/`、`density/`、`tick-probe/`、`wall/`、`plan-lib.log`、`plan-clippy.log`、`ign.log`）。這些檔案不在 repo 裡，session 結束後可能會消失。
> Mac 的出貨 worker 數是 `min(10, MAX_RECURSIVE_WORKERS=8) = 8`（`F/recursive.rs:77`、`457-460`）。當時的 rustc 版本沒有記錄，repo 也沒有 `rust-toolchain.toml`。
>
> **2026-09-27 更新**：§6 的 7 個開放問題都補上了研究結論與建議。各節內文中已被量測推翻或過時的敘述，直接改寫並標上「（09-27 更正）」。
> - 這一輪量測在 Linux x86_64、4 核、rustc 1.94.1 的雲端容器上做（出貨 worker 數 4），程式碼是 `d8f3af0`（與 `cc43241` 的差異只有本文件）。
> - 部分計時是在其他建置同時執行時量的，只能當粗略參考。產物的逐位元比對和 certify 結果不受機器影響。
> - 原始紀錄同樣放在 session scratchpad，session 結束後可能會消失。

## 1. 現況摘要

### 1.1 數字（對照 `tests/fixtures/fragment_synth_baseline.json`，legacy 是 `compile_legacy`，見 `F/benchmark.rs:251`）

| 案例 | 現在 ticks / blocks | legacy ticks / blocks | 體積：現在 vs legacy | wall | 出貨 producer | gate |
|---|---|---|---|---|---|---|
| and4 | 14 / 232 | 18 / 472 | — / 9,212 | 0.08 s | planner 直接葉 | 過 |
| verilog:and4 | 14 / 290 | 22 / 480 | — / 8,836 | 0.06 s | planner 直接葉 | 過 |
| full_adder | 54 / 1,094 | 46 / 1,784 | 18,725 / 39,750 | 0.31 s | planner 直接葉 | **ticks 紅** |
| segment_a | 124 / 9,305 | 72 / 6,416 | 391,680（272×18×80）/ 149,604 | 118.4 s | lid fabric `[4,6]` | **紅** |
| seven_segment | 198 / 21,847 | 98 / 16,244 | 707,850（325×18×121）/ 329,814（09-27 補量，2.15 倍） | 161.4 s | lid fabric `[6]`（第 4 個嘗試） | **紅** |
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
     - （09-27 更正）這只在 10 核 Mac 上成立。同一個 commit 在 4 核 x86_64 上，seven_segment 要 236.6 s（1 worker 時 315.5 s），segment_a 要 160.9 s（3 次中位數；1 worker 194.0 s），pinned 要 46.9 s。
     - 180 s 的出處是交接文件給 agent 的迭代規則，不是產品需求，見開放問題 7。
   - nested packed 一次都沒出貨，卻佔掉約 55% 的時間。每個 producer 都從頭重建葉。
   - 10 核只用到 1.2–1.9 倍。4 核上是 121%（segment_a）、163%（seven_segment）、175%（pinned）。
     - seven_segment 在 4 worker 時的 user CPU 比 1 worker 多 22%（384.8 s vs 314.6 s），多出來的是 `run_indexed` 的投機性工作。
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
     - （09-27 補充）另外，integration test `tests/fragment_synth_baseline.rs:56`（凍結 baseline JSON 的守門測試）在 HEAD 也是紅的，見 C0。
   - clippy 有 154 個不重複 warning，`check.sh:31` 用 `-D warnings` 所以會失敗。
   - `docs/handoff-2026-09-24/` 有 2.4 MB 的過期檔案。
   - 共用的 target dir 會拿到舊 binary。bake 沒有腳本。
     - （09-27 補充）問題比這更廣：連 `cargo test` 都可能跑到別的 checkout 編出來的 lib，見 S3。
   - 在 Linux 上，`TMPDIR` 位於 `/tmp` 底下時，`verilog:*` 會失敗，所以 `check.sh` 在一般 Linux 機器上會紅，見 S3。
   - 沒有 CI：repo 裡沒有 `.github/`，GitHub Actions 的 workflow 數是 0。唯一的 gate 是手動執行 `check.sh`。
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
   - （09-27 更正）那個掃描只涵蓋 decoder 一個案例，而且半平面是寫死的（`tests/build_circuit_pins.rs:524` 的 `z in 144..`、`536` 的 `z in 0..=24`）。沒有通用的掃描，`docs/fabric-plan.md` 提過的 `pinned_fabric_stays_inside_its_room_or_open_sides` 也從沒加進來（grep 0 筆）。見開放問題 2 的建議。
6. baseline JSON（`baseline_commit` afe577d）不改，acceptance 標準不降。三個紅燈 gate（`F/benchmark.rs:484-489`）保持紅燈，直到品質工作完成。

**工作規則**

- 加速類項目的產物必須逐位元不變。比對方式是 `canonical_world_fingerprint`（`F/benchmark.rs:1190`）。
- 會改變產物的項目：
  - 今天的產物必在候選清單內，而且清單順序等於今天的嘗試順序。
    - 例外（待開放問題 1 決定）：刻意用一項指標換另一項的 PR（例如 T2–T4 以少量 blocks 換 ticks、D4.1 以 ticks 換體積），可以把新做法排到清單第一位當基準，並附上 M1 報告證明沒有 gate 由綠轉紅。
  - 升 `producer_revision`（`F/recursive.rs:446-450`）。
  - 重新烤 `viewer/baked/`。
  - commit 附上 M1 報告的 3 次中位數。
- 新的幾何（z 向 pad、疊層、N/S 面、PITCH 2）要先有 `tests/fabric_kernels.rs` 的 kernel 測試。C3 的真實遊戲環境就緒後，還要附上真實遊戲的結果。
  - C3 已暫緩（使用者 2026-09-27 決定），期間不要求真實遊戲結果。
- 每案單次執行不超過 180 s，超過就不合併。
  - （09-27 更正）這個數字取自交接文件 `:20` 給 agent 的迭代規則（「單次實際測試約三分鐘上限」），而且沒有指定機器。同一個 commit 在 4 核 x86_64 上，seven_segment 就要 236.6 s。
  - 建議改為只報告、不擋合併，秒數目標綁定參考機（10 核 Mac、8 worker、3 次中位數），見開放問題 7。

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
    - （09-27 補充）再加機器標記：arch、`available_parallelism`、實際 worker 數。秒數只有在同一台參考機上才能比較，見開放問題 7。
  - 不加 volume gate。
- **成功標準**
  - 6 個 `budget_zero_*` 各印一行，包含上面的欄位。
  - baseline JSON 仍能解析，`F/benchmark.rs:1602` 的 schema 測試不變。
    - （09-27 更正）1602 行的測試解析的是字面 JSON，跟 checked-in 的檔案無關。真正把 JSON 和目前 revision 綁在一起的是 `tests/fragment_synth_baseline.rs:44-56`，而它在 HEAD 已經是紅的，見 C0。
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
  - `QualityKey` 的 derive `Ord` 是 (ticks, blocks, volume, `static_routed_delay`) 四欄的字典序（09-27 更正：原寫三欄）。第四欄只是複製 settle（664-666 行），所以行為等同三欄。它在 `assemble_product` 用已認證的 world 填好（`F/recursive.rs:659-667`），但 production 從來不比較它。
  - seven_segment 出貨的是第 4 個嘗試。
  - （09-27 更正）原本這裡寫「segment_a 的 nested `[6]` 從來沒被試過；舊量測約 6,566 blocks、102–108 ticks，兩項都比今天的 fabric 好」，這已經過時。
    - 那組數字來自合併前 2026-09-24 交接時的樹（交接文件 `:50`、`:173`）。
    - 在 `d8f3af0` 上，nested `[6]` 在 1、4、8 worker 都被拒：child `f6450e12…` 的 seed repair 做了 27 次仍失敗。
  - （09-27 實測）每個候選單獨建出來的結果（4 核容器）：

    | 案例 | 候選 1 | 候選 2 | 候選 3 | 候選 4 |
    |---|---|---|---|---|
    | segment_a | nested `[4,6]` 拒 | fabric `[4,6]` 124 / 9,305 / 391,680 | nested `[6]` 拒 | fabric `[6]` 148 / 10,973 / 443,592 |
    | seven_segment | nested `[4,6]` 拒 | fabric `[4,6]` 拒 | nested `[6]` 拒 | fabric `[6]` 198 / 21,847 / 707,850 |
    | pinned | pinned packed `[4,6]` 拒 | pinned fabric `[4,6]` 190 / 20,410 / 1,025,780 | pinned packed `[6]` 拒 | pinned fabric `[6]` 240 / 24,204 / 1,161,440 |

    - 第二個能 certify 的候選，每一項都比第一個差。所以在今天的樹上，無論 dominance 或純字典序，Q1 出貨的都和今天逐位元相同。
    - 出貨 fingerprint：segment_a `59578911…`、pinned `03f7e130…`、seven_segment `06972a43…`。
    - 5 個能 certify 的候選，certificate 的 `QualityKey` 都等於 benchmark 量到的值，但目前沒有測試斷言這件事。
    - and4、verilog:and4、full_adder 的 gate 數不超過 32（7、9、22），走直接葉，沒有 Q1 候選。
- **做法**
  - 固定的候選清單，不隨 budget、時間或 worker 數變化：
    - 未釘選，gates > `TERMINAL_GATES`（`F/recursive.rs:76`）：`[nested [4,6], fabric [4,6], nested [6], fabric [6]]`。
    - 釘選：`[pinned packed [4,6], pinned fabric [4,6], pinned packed [6], pinned fabric [6]]`。四個都失敗才走 allocation（`F/recursive.rs:896` 起，S1 會處理它的去留；開放問題 2 建議先收窄成只剩 CallerRow，見 D4 的 D4.R0）。
    - 若開放問題 4 採用寬葉，未釘選清單的最後再附加兩個候選，見 T6。
    - 直接葉不動。
    - seam 形狀不另列候選：`tightest` 失敗時已經會退回 full strip（`F/packed_node.rs:874-885`）。
  - 執行：`run_indexed(CertificationWorkers::bounded(n), n, |i| Ok::<_, Infallible>(attempt(i)))`，每個候選一路做到 `adapt_packed_root`，分到 `max(1, workers/n)` 個 worker，葉經由 P1 快取共用。
  - 選擇：純函數 `pick`，用 dominance 規則：
    - 基準 = 依清單順序第一個認證成功的候選，也就是今天會出貨的產物。
    - 只在 ticks 和 blocks 都不比基準差的候選之間，依 `(QualityKey, index)` 取最小；沒有這樣的候選就出貨基準。
    - 全部失敗時回傳最後一個候選的錯誤，和 `F/recursive.rs:828` 一致。
    - 等價的寫法，可以寫進 doc comment：「以 ticks 優先的 `QualityKey` 字典序，限制在 `non_air_blocks` ≤ 基準的候選之間」。
    - `pick` 只讀整數鍵和 index，不讀 baseline JSON、時間或 worker 數。
    - 不要沿用 `F/search.rs:253-257` 的 `candidate.quality() < best.quality()`，那是純字典序（測試 `only_a_strictly_better_certified_proposal_replaces_the_best` 在 502-510 行接受 quality(7,100) 取代 (9,20)）。
  - 報告：`RecursiveDiagnostics` 加 `candidates: Vec<(label, Result<QualityKey, String>)>` 和 `chosen`，不含時間。
    - 另加 `lex_alt`：當不受限制的字典序最小值和 `chosen` 不同時才填。累積這份資料，將來才有依據重新檢討規則。
  - `producer_revision` 升到 v2。
  - 不設時間預算，認證前也不剪枝。
- **成功標準**
  - 6 案出貨的 ticks 和 blocks 都不比今天差。這由構造保證，因為今天的產物就在清單裡，而且只有 dominance 才換。
    - （09-27 補充）這個保證是相對於「第一個認證成功的候選」，不是相對於昨天的產物。若日後某個排在前面的候選開始能 certify（例如 D3 之後的 nested `[4,6]`），基準就會移動。
    - 所以要加一個測試，把每案的基準 index 釘住：segment_a 1、pinned 1、seven_segment 3。
    - 另外要注意：今天第一個成功的 producer 若在 `adapt_packed_root` 出錯，整個 compile 會經由 `?` 中止（`F/recursive.rs:803-805` 等處）。Q1 之後 adapter 錯誤會變成 `r[i]=Err`，改由後面的候選出貨。
  - 沒有任何 `no_tick_regression` 或 `no_block_regression` 由 true 變 false；目前綠燈的 and4、verilog:and4、pinned 保持綠燈。
  - `pick` 有單元測試。至少涵蓋：
    - 只有一個 Ok。
    - 基準不被支配，例如 (124, 9305) 對 (100, 9400)，保留基準。
    - 支配的候選勝出，例如 (102, 6566) 勝 (124, 9305)。
    - 多個支配候選之間照字典序。
    - ticks 和 blocks 相同時，體積小的勝。
    - 完全相同時 index 小的勝。
    - 全部 Err 時回傳最後一個 index。
    - 上表三組實測向量，預期選 1、1、3。
  - 新增 `candidate_selection_is_worker_invariant`。
  - 和 P1–P3a 合起來的 wall：seven_segment ≤ 110 s，segment_a ≤ 75 s，pinned ≤ 30 s。user time 不超過今天的 1.5 倍。
    - （09-27 補充）這些秒數要在參考機上量（開放問題 7）。pinned ≤ 30 s 看起來偏緊：4 核容器上單是 pinned fabric `[6]` 就要 70.3 s。
- **風險**
  - dominance 可能讓只在一項上明顯更好的候選出不了貨。這是刻意的保守選擇，見開放問題 1。
  - 在今天的樹上 Q1 對 6 案沒有任何收益，卻要把四個候選都跑完。pinned 今天 45 s，四個候選串行在 4 核容器上是 21.3 + 24.0 + 65.7 + 70.3 s。
    - 若開放問題 4 的寬葉候選被採用，Q1 就有收益，見 T6。
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
    - （09-27 更正）今天出貨的 seven_segment（fabric `[6]`，198/21,847）依 `RecursiveDiagnostics` 是 4 片 `[21,21,21,21]`。8 片的切法來自其他嘗試（待查是哪一個），不是出貨產物。
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
- **（09-27 補充）與開放問題 4 的關係**
  - 第 2 步把每個大小都截在 `min(grain)`，所以 D2 永遠不會做出超過 32 gates 的葉。它跟開放問題 4 相關的只有「partition 政策」這一點。
  - 照上面的寫法，D2 會就地取代今天的 4×21 對半切，今天的產物就不在候選清單裡了，違反 §2 的工作規則。要合規，必須改寫成附加在 Q1 清單後面的候選。
  - 它的 seven_segment 目標（blocks ≤ 19,700）已經被 T6 的對半切寬葉候選（15,261）超過。若採用 T6，D2 的優先度應該重新評估。
- **風險**
  - 一個被拒的候選要 30–100 s，而且各位置之間必須串行。靠 P1 快取和位置內並行壓住。
  - 可行性對大小不單調。實測：32/30/22 切法裡的 22-gate 葉兩個 pitch 都被拒，但 42、46-gate 的葉都能 certify。
- **工作量**：M｜**依賴**：P1、P2；和 T2 共用 `flat_leaves`（見 T2）；開放問題 4 的決定（交接規則禁止藉調 partition/grain 避開問題）。

#### D3 nested packed（只在 Q1 報告顯示 nested 候選會勝出或差距很小時才做）
- （09-27 補充）在今天的樹上，這個觸發條件不成立：6 案裡 nested 候選全部被拒（見 Q1 的實測表），所以 D3 目前擱置。
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
- （09-27 補充）decoder 的 netlist 是 47 gates（grain 1 時 47 片葉），算房間面積時用得到：47×344 = 16,168。

- **D4.R0 pinned fallback 收窄成 CallerRow，其他情況回傳結構化錯誤**（S，09-27 新增，待開放問題 2 決定）
  - 問題：
    - allocation 把本體放在所有 pin 的南側（`F/allocation.rs:803`、`888`；走廊從 `caller_row_z+1` 開始，1477 行）。
    - 只有 `RootAccess::Landed` 時才違反 pinned 規則。`CallerRow`（一排 pin、輸入朝南、輸出朝北，994-1003 行）本身合規。
    - `pinned_floors_short` 只在南面有 caller 時才會回 `Some`，所以每一次 `short` → allocation 都一定蓋過有 caller 的那面。
    - 實測（4 核容器）：
      - short_room：2,042 blocks 中有 1,966（96%）蓋在輸入後方。
      - wide_short_room：2,630 中有 2,554（97%）。
      - 兩者都通過 certify，除非設了 `REDA_TRACE_PINNED` 否則沒有任何警告。
    - 對真正的 decoder 強制走 allocation，60.6 s 後 certify 失敗（懸空 dust 在 z=204，輸入在 z=144）。封閉房間的變體在 60.4 s 後也是同樣的失敗。
    - 6 案和 baked 世界都不會走到 allocation。lib 測試中只有 `F/recursive.rs:1692` 會走到（pin 列在 z=0，四個 pinned 嘗試都因 NegativeCanvas 失敗）。
    - pinned producer 的 typed refusal（`PinnedSpaceShort`、`PinnedRegionTooSmall`）只經由 `trace_refusal`（`F/recursive.rs:741-742`，需要 `REDA_TRACE_PINNED`）才看得到。
  - 做法：
    1. `compile_with_cutoff` 只在 `root_placement()` 回傳 `RootAccess::CallerRow` 時才走 allocation。這是 allocation 唯一實測過的合法用途：pin 列在 z=0、z=1 時，pinned producer 會因負座標失敗，allocation 在 0.4–0.49 s 內做出合規世界（caller 側 0 blocks）。從 z=2 起，pinned packed 就能成功。
    2. 其他 `Landed` 失敗和所有 `short` 命中，都回傳新的結構化錯誤，例如 `SynthesisError::PinnedRoom { pin_rect, closed_sides, room, smallest_footprint_built, area_needed_lower_bound, floors, candidates }`。
       - `build_circuit` 要能印出它。目前 `--synth` 分支在 `map_err` 裡就 `process::exit`（`src/bin/build_circuit.rs:791-795`），碰不到 809 行 `pins:` 的輸出，要先改這段流程。
       - 開放的軸印成 `open`，不要印 `2147483647`；錯誤文字裡也不要放 ChunkId 雜湊。
       - `short` 在建任何 child 之前就觸發，所以它只能報估計值，要標明是下界。
    3. 344/gate 的估計不能單獨造成拒絕。改成不高於所有實測葉的值，並用 corpus 測試守住。
       - 不要改成只進 trace：那樣封閉房間的 decoder 要 193.9 s 才會拒絕（grain 32→1 兩個 ladder），超過 180 s。
       - 驗證者算過：用約 96/gate 時，47×96 = 4,512 > 29×105 = 3,045，仍能快速拒絕。
    4. 加一個通用的 pinned 規則掃描（任何有 caller 那面的半平面外不得有方塊，邊界定義和 `PinnedRoom::fed` 一致），套到每個 pinned 測試產物上。
    5. 升 `producer_revision`：對部分輸入，行為會從「回傳世界」變成「回傳錯誤」。
  - 標準：6 案的 `canonical_world_fingerprint` 不變（case fingerprint 會隨 `producer_revision` 改變）；`F/recursive.rs:1692` 照常通過；short_room 類輸入回傳結構化錯誤，不再出貨違規世界。
  - 風險：
    - 有些今天會回傳世界的 `Landed` 輸入會改成回錯誤。實測過的案例中，那些世界不是違反規則，就是 certify 失敗。
    - 只保留 CallerRow 是保守的做法。南面沒有 caller 的 `Landed` 輸入（例如 `F/recursive.rs:2298-2309` 的 split_row）若也靠 allocation 救，就會被擋掉。
    - 更精確的版本是「`PinnedRoom.fed[3]` 為 false 才准 allocation」，或對 allocation 的產物跑規則掃描。
  - 依賴：無；必須在 D4.4 之前落地。
  - 這會推翻 `docs/fabric-plan.md` 第 12 項「最後退回 allocation」的原設計。

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
       - （09-27 補充）這個順序把今天的產物放在最後，不符合 §2「清單順序等於今天的嘗試順序」。要嘛把它當成 §2 例外（開放問題 1），要嘛改成附加候選。D4.1 的標準本來就接受 ticks 從 190 升到 209，dominance 不會自動選它。
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
    - （09-27 更正）199 也未必是下界：decoder 在 grain 1 時實建是 9,553 格（`[4,6]`，203/gate）和 15,137 格（`[6]`，322/gate）；chain(34) 有一個放得進 196×18 的 layout（104/gate，只有 placed、沒有實建驗證，待重測）。
    - 下界要從實建的 footprint 推導，或用 corpus 測試守住的常數，見 D4.R0 第 3 步。
    - `short` 的預先檢查也要把 `F_MAX` 層算進去（需要的面積 > 房間面積 × `F_MAX` 才算不夠），並讓拒絕在 180 s 內完成。
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
    - （09-27 補充）不只是「決定」：D4.R0 必須先落地。否則超限的 `PinnedRegionTooSmall` 會先觸發 grain 減半（`F/packed_recursive.rs:287-302`），再掉進 allocation，D4.4 的 typed refusal 根本到不了使用者手上。

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
  - （09-27 更正）算式本身沒錯（8+42+38+10 = 98），但「leaf 內」和 C 不是常數，會隨葉變大而增加。
  - 實測 k=1（兩片 42-gate 葉）：lead 6 + leaf 內 54 + 跨越 70 + tail 12 = 142。
    - 那條跨越幹線 g31 長 226 格、14 個 terminal repeater，約 28 ticks，其餘約 42 ticks 在兩側葉內的腿。
    - 在 k=1 時要到 98，需要 C ≤ 26，比今天這條幹線本身還短。除非 seam 大幅收窄，否則不太可能。
  - segment_a 做成單一片 46-gate 葉（k=0）實測 80 ticks（lead 8 + 葉內 66 + tail 6），仍高於 legacy 的 72。見 T6。

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
  - （09-27 補充）`production_leaf_chunks`（`F/attribution.rs:236-257`）用的是靜態 `TERMINAL_GATES` 分割。產物若有 42、46-gate 的葉（T6），葉內的跳會被算成跨越。所以 T0 必須在 T6 出貨之前落地。
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
- **（09-27 更正與補充）**
  - 「k=1 需要 ≥ 42-gate 的葉」在結構上不成立：32/30/22 的分層切法最多只跨 1 次（交換 trunk 14 條）。
  - 但實測這個切法失敗了：
    - 22-gate 那片兩個 pitch 都被拒（27 次 NoLocalRoute），拆成 11+11。
    - fabric `[4,6]` node 被拒，fabric `[6]` 做出 4 片 `[30,11,11,32]`，162 ticks / 21,725 blocks，4 核上 591 s。
  - 在 32 grain 下，cone order 的 4×21 靜態也只到 k=2。T6 的對半切寬葉候選（142 / 15,261）兩項都比它好，所以 T2 對 seven_segment 的優先度應該降低。
  - 照上面的寫法，T2 會就地換掉今天的切法。要合規，必須改成附加在 Q1 清單後面的候選（同 D2）。

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

#### T6 寬葉候選（09-27 新增，待開放問題 4 決定）
- **問題**
  - seven_segment 出貨的 4×21 切法有 3 次跨越，佔 138 ticks。
  - 超過 32 gates 的葉從沒試過。grain 在 git 歷史裡一直是 32。
- **實測**（4 核容器，scratch 副本，沒有改 repo）
  - 每個世界都走真正的 `adapt_packed_root` → `certify_root_world` → `evaluate_world`。對照組重現了今天的 198 / 21,847。

    | 做法 | 結果 | ticks / blocks | 時間（4 核） |
    |---|---|---|---|
    | seven_segment 切兩片 42-gate 葉 | certify | **142 / 15,261**（lead 6 + 葉內 54 + 跨越 70 + tail 12；trunk 22→12 條） | 308–333 s；1 worker 581 s |
    | segment_a 單一片 46-gate 葉 | certify | **80 / 4,183**（k=0） | 葉 43.9 s，全程 48 s |
    | seven_segment 單一片 84-gate 葉 | 58 次 seed 嘗試後被拒（694.8 s） | 自動退回 42+42，產物和上面逐位元相同 | 1,170 s |

  - 兩個 blocks gate 會轉綠（15,261 ≤ 16,244；4,183 ≤ 6,416），兩個 ticks gate 仍紅。
  - 決定性：
    - 42+42 版在 1、2 worker 的 fingerprint 相同（`989be5da…`）。4 worker 的那一筆來自 84 單葉退回後的產物，不是直接比對。
    - 46 單葉在 1、3 worker 相同，但這筆沒有保存紀錄。
  - A\* 餘裕：
    - 今天的 21-gate 葉：queue 峰值 37,692–68,613，最多是上限的 26%，8 次建置共撞上限 1 次。
    - 42-gate 葉：峰值 154,665–239,683，最高到上限的 91%，每次建置撞 2–11 次，全部由既有的 seed repair 吸收。上限沒有提高。
- **做法**
  - 不動 `TERMINAL_GATES=32`（`F/recursive.rs:76`）。它同時是直接葉 planner 的門檻、pinned grain 迴圈的起點、歸因的 grain，而且併在 `producer_revision` 裡。
  - 新增具名常數 `WIDE_LEAF_GATES = 48`，併進 `producer_revision`（v2）。不做成環境變數或設定欄位（`PackedGrain` 的文件明言 grain 不是旋鈕，`F/packed_recursive.rs:64-67`）。
  - 在 Q1 未釘選清單的最後附加兩個候選：
    - 「fabric 對半切 `[4,6]`」：root 在 `split_of(n)` 切一刀，每半各一片葉。只在 32 < ⌈n/2⌉ ≤ 48（n = 65–96）時啟用。
    - 「fabric 單葉 `[4,6]`」：整個 root 一片葉。只在 32 < n ≤ 48 時啟用。
  - 被拒的寬葉經由既有的 `build_leaf_finer` 拆回和今天完全相同的 chunk。
  - 由 Q1 的 dominance 選擇。今天的產物仍是基準，pinned 路徑不動。
- **成功標準**
  - seven_segment ≤ 142 / ≤ 15,261，segment_a ≤ 80 / ≤ 4,183。
  - 其他案例逐位元不變。C5 sweep 涵蓋新候選。
  - M1、M2 報告每片葉的 queue 峰值，讓餘裕的變化看得見。
- **風險**
  - **時間是最大的問題。** Q1 下每個候選分到 `max(1, workers/n)` 個 worker；在 Mac 上 8 worker、6 個候選，每個也只有 1 個 worker，兩片 42-gate 葉只能串行建。
    - 依實測的葉時間（139–156 s 加 293–311 s）除以粗估的 2.4 倍機器差，推估 Mac 上約 185–240 s，超過 180 s。
    - P1 幫不上忙（沒有其他候選會建這兩個 chunk），P2 也需要每個候選至少 2 個 worker。
    - 所以合併前要有 Mac 實測 ≤ 180 s，再加上以下其中一項：
      - 一條不隨 worker 數改變的靜態規則，把多餘的 worker 分給寬葉候選。
      - P5。
      - S1 刪掉 nested 候選。
  - A\* 餘裕從 ≥ 74% 掉到 9%。接近 48 gates 的其他 netlist 可能從 certify 變成被拒，損失的是時間而不是正確性。
  - 能不能 certify 不只看大小：22-gate 的葉被拒，42、46-gate 的卻通過。48 這個值只有三個尺寸的量測撐著（42、46 通過，84 被拒）。
  - 證據只來自一個 netlist 家族（BCD decoder）。
  - Q1 全部失敗時回傳「最後一個候選的錯誤」，附加候選後，回報的錯誤會跟著改變。
- **工作量**：M｜**依賴**：Q1、T0、P1、P2；開放問題 4 的決定；合併前需要 Mac 實測。

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

- **（09-27 補充）lib 以外還有一個紅燈**：`tests/fragment_synth_baseline.rs:56` 的 `checked_fixture_keeps_schema_order_revisions_hashes_and_new_coverage`。
  - JSON 的 `verifier_revision` 是 `1af20604…`，`physical_verifier_revision()` 現在回傳 `049a81d8…`。
  - 唯一原因是 b1ddbe5（2026-09-01）在 verifier descriptor 加了 `policy` 欄位；規則清單沒變，只算規則的 fingerprint 仍等於 `1af20604…`。
  - case fingerprint 用的是 `expanded_physical_verifier_revision`（`F/api.rs:283`），不受影響。
  - 可能沒人發現的原因：`cargo test` 預設在第一個失敗的 test target 就停，lib 有 6 個失敗時，`check.sh` 大概跑不到這個檔案（推論，未重跑確認）。
  - 修法：不改 JSON（硬限制 6）。在測試裡把當初的值 `1af20604…` 釘成常數，現況的漂移另外報告。
    - 不要改成「`LegacyCompatibility` 時不序列化 `policy`」：C1 第 2 步加 Support 規則時 revision 還會再變一次，那種修法只撐到 C1。
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
     - （09-27 補充）`compile_planned_within`、`compile_planned`、`compile_grown` 都經由 `realise_and_verify` 走到 `verify_legacy_candidate`（`planner.rs:6391`），所以這一步已經涵蓋 DFF 分支，不必另外接線。
     - 這一步會改變 `physical_verifier_revision()`，見 C0 的守門測試。
  3. 新增測試：對 `compile`、`compile_legacy`、`compile_grown` 產出的全部 reference 和 verilog 電路跑 Support 檢查。
     - （09-27 補充）這兩份清單都沒有任何時序電路（`src/circuits/verilog.rs:99-116` 和手寫 ladder）。要加上：
       - `dff.sv`、`dff_enable.sv`（兩種 lowering）。
       - `dff.v`（Yosys；Linux 上 `TMPDIR` 要在 `/tmp` 以外，見 S3）。
       - pinned 的 `compile_planned` DFF 案例（`planner.rs:13146`）。
     - DFF 另外加 simulator 對 Evaluator 的追蹤，輸入要包含「同一時鐘相位內多次切換 D」。Support 為 0 不代表 DFF 世界是對的，見 C8。
     - 同時把 5 個 legacy 世界的 fingerprint 全部釘住（今天只有 and4 有，`F/benchmark.rs:2189-2193`），並斷言 Support 0 違規（含 lever 與 y=0）。5 個 `compile_legacy` 合計約 0.6 s。
       - lever 檢查沿用 `tests/reference_circuits.rs:593` 的 `lever_support_position` 和 621 行的 `face_is_supported`。
  4. 逐一讀過再重錄：
     - `tests/reference_circuits.rs:557`。
     - `planner.rs:14249` 改為斷言 0 並改名。
     - `planner.rs:14314` 的註解數字。
     - README 的表。
- **成功標準**：
  - `grep -rn REFUSE_OWN_CRUSH src` 為 0。
  - Support 在三條路徑上 0 違規。（09-27 補充）`compile_planned` 也要列入，pinned 的 DFF netlist 走它。
  - 6 個 acceptance 的 fingerprint 不變。
- **風險**
  - planner 可能 route 不出來，一般電路會 fallback 到 legacy。
  - DFF 沒有 fallback（`mod.rs:7556-7561`），可能從「出貨壞世界」變成「編譯失敗」。先跑 `tests/systemverilog_dff.rs` 和 `tests/m3_m4_dff_yosys_differential.rs`。
    - （09-27 更正）這兩個測試只到 netlist 和 Evaluator，不會建世界，偵測不到這件事。會建世界的 DFF 測試在：
      - `tests/m5_tier_c_bridge.rs:180`、`232`。
      - `tests/verilog_frontend.rs:358`。
      - `tests/compile_timing_harness.rs:169`（ignored）。
      - `src/compile/mod.rs:8548`、`8653`。
      - `planner.rs:13146`。
    - （09-27 實測）C1 第 1 步套上後，repo 裡 8 個不同的 DFF 世界沒有一個變成編譯失敗，所有 DFF 測試照常通過（唯一的新失敗是預期中要重錄的 `a_route_lays_dust_on_its_own_committed_stone`）。
      - 唯一改變的是 `dff_enable`：原本在 (33,2,48) 有一個 dust 疊在 dust 上。照遊戲規則把懸空的 dust 拿掉後，32 次取樣錯 9 次。
      - C1 之後它重新繞線，0 懸空、0/32 錯，275→284 blocks，編譯 25 ms→130 ms。
      - 其他 7 個 DFF 世界逐位元不變。
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
- **狀態：暫緩**（使用者 2026-09-27 決定）。
  - 暫緩期間，§2「改變 fingerprint 要附真實遊戲結果」的合併規則不啟用，改由開放問題 7 建議的出貨 fingerprint fixture，加上 simulator 檢查把關。
  - 雲端容器目前只有 Java 21，而且網路政策擋掉了 Mojang 的下載點（`piston-meta.mojang.com`、`piston-data.mojang.com`）。
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
- **工作量**：M｜**依賴**：第 3 步依賴 C3（隨 C3 暫緩）。第 1、2 步可以先做。

#### C5 決定性覆蓋缺口
- **問題**
  - 已經證明的：
    - `F/recursive.rs:2629`（chain(33)，出貨入口）。
    - `F/recursive.rs:1755`、`2069`。（09-27 更正）原寫「allocating」：實測這兩個是 pinned packed（`PinnedRoom`）路徑上的 1 vs 多 worker 測試（trace：「pinned layout rank 3」）。
    - `F/packed_recursive.rs:2112`。
    - `F/packed_recursive.rs:1017` 的 lid fabric 測試：ignored，約 8 分鐘，2026-09-27 通過。
  - `F/recursive.rs:3042` 走的是 allocating。
  - 6 個 acceptance 案例在出貨入口一個都沒證明過。`PinnedRoom` 完全沒有 worker 測試。
    - （09-27 更正）`PinnedRoom` 的 worker 測試其實就是上面的 1755、2069。
    - （09-27 實測，手動、非 committed 測試）在出貨入口已量到：
      - segment_a 在 1、2、4 worker 相同。
      - pinned、seven_segment、full_adder 在 1、4 worker 相同。
      - Mac（arm64、8 worker）烤的 5 個 synth 檔，在 x86_64、4 worker 上逐位元重現，見開放問題 7。
    - committed 的 sweep 仍然需要，但風險比原本估的低。
  - 出貨的 worker 數是 `min(available_parallelism, 8)`（`F/recursive.rs:452-461`），不同機器跑的 N 不同。
  - （09-27 補充）測試裡要求的 8 個 worker 會被 `CertificationWorkers::bounded` 壓到 `available_parallelism`（`F/certification.rs:684-689`）。
    - 所以在 4 核機器上測的是 1 vs 4。
    - 在 1 vCPU 上 `assert!(many.peak_workers > 1)`（`F/packed_recursive.rs:1036`）會失敗，1 vs N 的比較也失去意義。
    - CI 若要跑這些，至少要 2 vCPU。
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

#### C8 DFF 路徑的既有問題（09-27 新增，與 C1 無關）
- **問題**（C1 之前、之後都一樣）
  - 稍大的時序電路今天 `compile()` 就失敗。25-gate 的 4-bit enable register 和 27-gate 的計數器都回報「no safe local route」，C1 前後錯誤文字相同。
    - 目前的影響還小：沒有任何 catalog 或 viewer 電路是時序電路，而 Yosys 前端目前也吃不進 enable register（「cell … has no `Y` connection」）。
    - 但 native SV 前端已經能 lower enable register，`docs/native-wasm-verilog-compiler-plan.md:771`、`778` 也計畫讓 CLI 和 viewer 直接吃 `.sv`，使用者遲早會碰到這條路。
  - DFF 世界在「同一時鐘相位內多次切換 D」時跟 Evaluator 不一致：
    - single DFF 2/32 錯。
    - `dff.v` 1/32 錯。
    - shift4 8/128 錯。
    - 現有的 7 步追蹤從不在同一相位切換 D 兩次，所以測試都是綠的。原因還沒查。
- **不建議現在做**：在 `compile()` 失敗後改走 `compile_grown` 的 fallback。
  - 它能做出 reg4_en 和 counter4（C1 之後 Support 0），但 grown 版 reg4_en 在 C1 之後 128 次取樣錯 1 次（C1 之前 0/128）。counter4 的 grown 世界沒有做功能檢查。
  - 那等於把「出貨壞世界」帶回來。
  - 它也是交接規則說的 whole-circuit fallback。
  - `compile_grown` 的文件自己說它不是 `compile()` 的預設（`src/compile/mod.rs:7926-7928`），在 segment_a 上曾跑 315 s 與 1,802 s。
- **做法**：先把多次切換的追蹤加進 C1 的 DFF 掃描（成為功能 gate），查清不一致的根因。等有功能 gate，並在較大的時序設計上量過有上限的成本之後，再評估 fallback。
- **工作量**：M｜**依賴**：C1。

---

### 3.E 結構、工具與維護

#### S1 收斂 producer
- **問題**
  - `compile_with_cutoff` 依序試：直接葉（`F/recursive.rs:764-775`）→ nested 或 fabric 乘上 `LEAF_LADDERS` → pinned（`pinned_floors_short`，`F/recursive.rs:838-846`）→ allocation（`solve_subtree` 加 `compose`，`F/recursive.rs:901`、`911`；還有 `split_refused_child` 1142 行、`synthesise_node` 1454 行）。
  - 在 6 案中，allocation、nested、pinned packed 都沒有出貨過。
- **做法**（在 Q1 之後，依 `candidates` 報告決定）
  1. 移除 allocation。前提有兩個：D4.4 已上線，且使用者同意超出上限時回傳 typed refusal（開放問題 2）。
     - allocation 會把本體放到所有 pin 的南側（`F/allocation.rs:888`），違反 pinned 規則。
       - （09-27 更正）只有 `RootAccess::Landed` 才違反。`CallerRow`（一排 pin、輸入朝南、輸出朝北）本身合規，而且是 pin 列在 z=0、z=1 時唯一的救法。
       - 刪除前，要嘛讓 pinned producer 接受 z < 2 的 pin 列，要嘛加一個 typed refusal（「`--synth` 的 pin 列必須在 z ≥ 2」）。見 D4.R0。
     - 把 `SignalContract`（`F/allocation.rs:199`）、`RootPort`（369）、`Prism`（163）、`root_placement`（558）、`normalise_root_pins`（619）移到 `contract.rs`。
     - 先把 `split_of`（`F/recursive.rs:1013-1015`）移到 `partition.rs`：`F/packed_recursive.rs:344`、`454`、`497` 和 `F/attribution.rs:249` 都在用，D2、T2 也建立在它上面。
     - 然後刪除 `allocate*`、`schedule.rs`、`F/parent.rs:2677` 的 `compose`、`compile_with_cutoff` 裡的 allocation fallback（`F/recursive.rs:896-994`），以及 `F/recursive.rs:996-1550` 裡 `split_of` 以外的部分。
     - 刪除只測 allocation 的測試（`F/recursive.rs:1692`、`1755`、`2214`、`3020`、`3041`）。
       - （09-27 更正）實測只有 1692 會走到 `compile_with_cutoff` 的 allocation fallback。它的註解「Pins three cells apart leave no rectangle a layout fits」（1702 行）也不對：`place()` 在 (13,8) 成功，是每個 layout 都在 z=-2 碰到 NegativeCanvas。
       - 1755、2069、2108 走 pinned packed。2214 走 pinned fabric。3021 呼叫 unpinned 的 `compile()`。只有 3042 直接呼叫 `solve_subtree`/`compose`。
       - 刪除清單要照這個重列；1692 要改寫成 CallerRow 或 z<2 refusal 的測試。
     - （09-27 補充）順便修正過時的程式碼註解：`F/api.rs:101-104`（「a pinned root above it is allocated and composed」）和 `F/recursive.rs:776-783`（「continues down the allocating path untouched」）。pinned root 今天會先試 pinned packed 和 pinned fabric（847-893 行）。
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
  - （09-27 補充）共用 target 的問題不只 `target/release/*`。
    - 每個 checkout 編出來的 lib 測試 binary 名稱都一樣（例如 `target/release/deps/reda-<同一個 hash>`）。
    - dep-info 裡記的是相對路徑，cargo 會把別的 checkout 編的產物當成最新的。
    - 所以連 `cargo test` 都可能跑到別的 checkout 的程式碼。09-27 平行實驗時真的發生過：
      - 測試過濾器配到 0 個測試。
      - 套了 C1 patch 的快照測試仍看到 3 格 dust-on-dust。
      - 當時改用自訂 profile（`--profile q5probe`，寫到 `target/q5probe/`），或每個 checkout 各自的 `CARGO_TARGET_DIR`（`cp -a` 一份 `release/` 重用依賴），才解決。
  - （09-27 補充）在 Linux 上，`TMPDIR` 位於 `/tmp` 底下時，`verilog:*` 會失敗（「Can't open log file `/tmp/reda-verilog-…/out.json.yosys.log`」）。
    - 原因：yowasp_runtime 把自己 `mkdtemp` 的目錄掛成 WASI 的 `/tmp`（`yowasp_runtime/__init__.py:96-97`），遮住了 reda 用 `std::env::temp_dir()` 建的工作目錄（`src/frontend/mod.rs:388`）。
    - 所以 `check.sh` 裡非 ignored 的 `the_baked_netlists_match_fresh_synthesis` 在一般 Linux 上會紅。
    - 根本修法在 `src/frontend/mod.rs:388`（工作目錄不要放在 `temp_dir()` 底下，或傳 yowasp 看得到的路徑）；在腳本裡設 `TMPDIR` 只是權宜。
  - （09-27 補充）`viewer/baked/verilog_and4.synth.litematic` 不是 gate 量的那個世界。
    - viewer 版由 `build_circuit` 用即時 Yosys 加 `lower_optimised` 產生（`src/bin/build_circuit.rs:437-448`）：214 blocks，(71,6,45)。
    - gate 的 `verilog:and4` 用 baked netlist 加 `lower`（`F/benchmark.rs:893-896`）：290 blocks，(72,6,59)。
- **做法**
  - 新增 `tools/bake-viewer.sh`，寫法仿照 `check.sh`：
    - 預設 `REDA_PYTHON=$PWD/.venv/bin/python`。缺 `yowasp_yosys` 時提示 `uv venv --python 3.12 && uv pip install -r requirements.txt` 並結束。
    - 每案用 `cargo run --release --bin build_circuit -- <name> --synth [--pins …]`，複製到 `viewer/baked/`，印出 bbox 和 `git diff --stat`。
    - （09-27 補充）同時印出每個烤好檔案的 `canonical_world_fingerprint`，方便和開放問題 7 建議的 fixture 對照。
  - 刪掉本地 `target/`。
  - `README.md:101-102` 加一行：不要直接執行 `target/release/*`。
  - 預設不改成每個 worktree 各自一個 target。
    - （09-27 建議翻案）依上面的實測，平行工作時共用 target 會讓 `cargo test` 跑錯程式碼。建議平行的 worktree 各自設 `CARGO_TARGET_DIR`，或至少各用一個自訂 profile。
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
| **1a 選擇與 unpinned 密度**（D2 需要開放問題 4 的決定） | Q1 → T6（若開放問題 4 採用）→ P4a → D1.1 → D1.2 → D2 → D1.3；D3.x 依 Q1 報告決定（09-27：目前不成立） | 6 案都 certify，ticks 和 blocks 都不比今天差；seven_segment blocks ≤ 19,700（目標 16–18k）且 ticks ≤ 198；segment_a ≤ 9,305 blocks 且 ≤ 124 ticks，並以 Q1 報告的最佳候選為準（若 nested 勝出，目標 ≤ 7,000 blocks 且 ≤ 110 ticks）；每案 ≤ 180 s；`producer_revision` v2；viewer 重烤。（09-27）若採用 T6：seven_segment ≤ 142 / ≤ 15,261，segment_a ≤ 80 / ≤ 4,183，兩個 blocks gate 轉綠；D2 的優先度重新評估 |
| **1b pinned room**（D4.3 需要開放問題 4 的決定） | D4.R0 → D4.0 → D4.1 → D4.3 → D4.2 → D4.4 | decoder `pinned_overhang ≤ PINNED_OVERHANG_LIMIT`（D4.1 後定值），體積 ≤ 0.8M，blocks ≤ 20,410，ticks ≤ 209；矩形內 torch > 0；規則掃描通過。（09-27）pinned 放不下時回傳結構化錯誤，不再出貨違規世界 |
| **2 延遲** | T1 → T2 → T3 → T4；T5 視觸發條件 | full_adder ≤ 46 且 ≤ 1,784（gate 綠）；seven_segment ≤ 140；segment_a ≤ 105；pinned ≤ 190；blocks +2% 以內。（09-27）若採用 T6：seven_segment 追蹤目標約 112–115（T3 把葉內的腿壓到 ≤ 12 ticks），segment_a ≤ 80；T2 的優先度降低 |
| **正確性**（從 Phase 0 起並行） | C0 → C1 → C8 → C2 → C3（暫緩）→ C4（第 1、2 步）→ C7 | lib 0 failed，`tests/fragment_synth_baseline.rs:56` 轉綠；Support 在所有 compile 路徑（含 `compile_planned` 與 DFF）上 0 違規；5 個 legacy fingerprint 釘住；26.2 探針與 6/6 真實遊戲向量隨 C3 暫緩 |
| **3 收斂與清理** | S1 → C6；P4 跟在 D1 之後；P5、P6 依 M2；S2、S4、S5、S6 無依賴，可隨時插進去 | `check.sh` 只剩品質紅燈（品質工作完成後全綠）；clippy 0；只剩兩個 producer |

- （09-27 補充）上表所有秒數，都指參考機（10 核 Mac、8 worker、`--exact --test-threads=1`、開始前 1 分鐘 load < 1.0、3 次中位數）上的量測。其他機器只報告，不當退出條件，見開放問題 7。

**品質目標總表**（ticks / blocks；各欄的收益不可直接相加）

| case | legacy | 今天 | Phase 1 後 | Phase 2 後 | 若葉 ≥ 42 gates 或走直接葉 |
|---|---|---|---|---|---|
| seven_segment | 98 / 16,244 | 198 / 21,847 | ≤ 198 / ≤ 19,700 | ≤ 140 | ~~≈ 92（k=1，勉強過）~~ 實測 142 / 15,261（k=1，09-27） |
| segment_a | 72 / 6,416 | 124 / 9,305 | ≤ 110 / ≤ 7,000 | ≤ 105 | ~~≤ 72（k=0）~~ 實測 80 / 4,183（單一 46-gate 葉，k=0，09-27） |
| full_adder | 46 / 1,784 | 54 / 1,094 | 不變 | ≤ 46 / ≤ 1,784 | — |
| pinned decoder | — | 190 / 20,410 | ≤ 209 / ≤ 20,410 | ≤ 190 | — |

- （09-27 更正）最後一欄原本的推估假設「葉內」時間和跨越成本 C 不隨葉變大，實測兩者都會增加，見 §3.C 的預算模型與 T6。
  - `flat_leaves` 一律先對半切 root（`F/packed_recursive.rs:344`），所以沒有任何 grain 值能做出 segment_a 的單葉，k=0 需要 T6 的專用候選。
  - seven_segment 的 98 ticks 在可預見的範圍內不會達到，ticks gate 預期長期維持紅燈，見開放問題 4。

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
| viewer 正交相機 | 不需要；等真的有問題再加。（09-27：原本同一列的「預設每個 worktree 各自一個 target」已經出過問題，建議翻案，見 S3） |
| Q1 改用純 `QualityKey` 字典序（09-27 新增） | 可能為了少 1 tick 接受任意多的 blocks，讓綠燈 gate 翻紅（20 萬組隨機模擬：綠轉紅 8,530 次，dominance 0 次）；`compile_fragment_synth` 是公開入口，不能讀 baseline 來把關。見開放問題 1 |
| 直接把 `TERMINAL_GATES` 調到 42 或 48（09-27 新增） | 它同時管直接葉的 planner 門檻（planner 在 46、84 gates 會拒絕）；會取代今天的產物而不是新增候選；`PackedGrain` 文件明言 grain 不是旋鈕。改用 T6 的附加候選 |
| DFF 的 `compile()` 失敗時改走 `compile_grown`（09-27 新增，暫不做） | grown 版 reg4_en 在 C1 後 128 次取樣錯 1 次，等於帶回「出貨壞世界」；也是 whole-circuit fallback。等 C8 的功能 gate 就緒再評估 |
| 以 wall-clock 180 s 當硬合併 gate（09-27 新增，待開放問題 7） | 同一個 commit、逐位元相同的產物，4 核 x86_64 比 10 核 Mac 慢 1.28–1.47 倍，seven_segment 在 4 核上就是 236.6 s；結果取決於硬體，不是程式碼 |
| 現在就做 N/S fabric、node 級快取 | YAGNI；前者在 D4.2 需要時做，後者等 M2 證明需要 |
| PITCH 2、拿掉 E 層（原 D1.4） | 延後：PITCH 2（`F/fabric.rs:40`）估計再省 92 欄，但要改 L1≥3 規則並先在 vanilla 驗證 K1/K2；D1.3 若讓所有 pad 都走 Across，E 層就沒用了，高度可從 20 降到 17。兩者都等 D1.3 量測和 C3 之後再決定 |

## 6. 開放問題

> 2026-09-27：每一題都經過研究、實測，並由另一個人逐條核對證據、試著推翻。下面的「建議」是研究結論，還不是決定。除了第 3 題已由使用者決定之外，其他都待拍板。
> 量測環境見文件開頭。

### 1. Q1 的選擇規則（建議：維持 dominance）

- **原問題**：預設用 dominance / keep-better（`docs/fabric-plan.md:234-239`）：先出貨今天的產物，只有另一個候選 ticks 和 blocks 都不差時才換，字典序只在這些候選之間排序。要不要改成純 `QualityKey` 字典序（ticks 優先）？字典序可能挑到 ticks 較好但 blocks 超過 baseline 的候選，讓 gate 翻紅。
- **建議**：維持 dominance，照 §3.A Q1 寫的 `pick` 實作。不加容忍帶。
  - 將來若要加，做成 `SearchConfig` 裡的整數千分比欄位（會進 case fingerprint），只套用在 ticks 嚴格較少的候選，預設 0。
- **依據**
  - 今天兩種規則在 6 案選出的產物逐位元相同（見 Q1 的實測表），現在決定不會影響任何東西。
  - gate 的形式都是 `新 ≤ legacy`，是單調的：只要出貨產物在 ticks 和 blocks 上都不比基準差，任何 gate 都不可能由綠轉紅。
    - 20 萬組隨機清單模擬：dominance 綠轉紅 0 次，純字典序 8,530 次。
  - `compile_fragment_synth` 是公開入口（`src/compile/mod.rs:101` 再匯出，`build_circuit --synth` 對任意 netlist 呼叫），baseline JSON 只有測試會讀。選擇規則本身就是唯一的把關。
  - 「Pareto 前緣加字典序破同分」就是純字典序；「字典序限制在 blocks ≤ 基準」就是 dominance。唯一真正的參數是 blocks 上限要設在基準之上多少。
  - 目前沒有任何適用 Q1 的案例有綠燈的數值 gate，所以要等 Phase 1、2 讓某個案例接近 legacy 線之後，規則的差別才會顯現。
- **待決定**
  - 同意 dominance。
  - 同意 §2 工作規則的例外：刻意用一項指標換另一項的 PR（T2–T4 以少量 blocks 換 ticks、D4.1 以 ticks 換體積），可以把新做法排到候選清單第一位當基準，升 `producer_revision`，並附上 M1 報告證明沒有 gate 由綠轉紅。
  - 若寧可讓編譯器自動做這種取捨，就改選容忍帶並定出千分比，但要接受 gate 可能由綠轉紅。

### 2. pinned 房間放不下時（建議：回傳結構化錯誤，暫留 CallerRow）

- **原問題**：回傳 typed refusal，還是保留今天的 allocation fallback？allocation 會違反 pinned 規則（`F/allocation.rs:888`）。S1 刪除 allocation 和 D4.4 強制上限都需要這個決定。
  - （09-27 補充）實際上 fallback 不只在「房間太小」時觸發，而是任何 pinned producer 失敗都會觸發（例如 pin 列在 z ≤ 1 時的 NegativeCanvas，`F/parent.rs` 約 1806 行），而 `short` 命中時甚至不試任何 producer 就直接走 allocation（`F/recursive.rs:838-847`）。
- **建議**：分三步。
  1. 先做 D4.R0：只在 `RootAccess::CallerRow` 時保留 allocation，其他情況回傳結構化錯誤（房間大小、封閉的面、需要的面積），並升 `producer_revision`。
  2. 照原計畫做 D4 系列。D4.R0 落地後，D4.4 超限的 `PinnedRegionTooSmall` 會直接成為使用者看到的錯誤。
  3. S1 時整個刪掉 allocation。刪之前先處理 z < 2 的 pin 列。
- **依據**
  - 在 `Landed` 的情況下，allocation 就是違規：short_room 有 96%、wide_short_room 有 97% 的方塊蓋在輸入後方，而且都通過 certify，沒有任何警告。
  - 當安全網也不管用：真正的 decoder 強制走 allocation，60 s 後 certify 失敗，錯誤訊息對使用者沒有幫助。
  - 沒有任何出貨產物用到它：6 案和 baked 世界都不會走到。lib 測試只有 `F/recursive.rs:1692` 走到，而那是 z=0 的 CallerRow。
  - pinned producer 已有的 typed refusal 被它蓋掉了，只在設了 `REDA_TRACE_PINNED` 時才看得到。
  - 若保留 fallback，D4.4 會比不做還糟：超限時先 grain 減半，再掉進 allocation。
  - 交接規則（`:15`、`:17`）傾向移除，但它點名的是 whole-circuit seed，不是 allocation，所以仍需要你拍板。這也會推翻 `docs/fabric-plan.md` 第 12 項「最後退回 allocation」的原設計。
- **待決定**
  - 同意 pinned 放不下時回傳結構化錯誤，而不是出貨蓋在 pin 外面或後面的世界（建議：同意）。
  - pin 列太靠近世界邊緣（z = 0 或 1）時二選一：
    - (a) 暫時保留 allocation 處理單排 CallerRow（合規；今天唯一的救法），到 S1 為止。
    - (b) 現在就拿掉，z < 2 的 pin 列直接拒絕。

### 3. 真實遊戲環境（已決定：暫緩）

- **原問題**：能否在 Mac 裝 JDK 25 加 vanilla 26.2 並自己同意 EULA？C3、C4 驗證和「改變 fingerprint 要附真實遊戲結果」的合併規則都卡在這裡。
- **決定**：使用者 2026-09-27 決定，先不做真實 Minecraft 測試。
- **影響**
  - C3 暫緩。C4 只做第 1、2 步。
  - §2 的「附真實遊戲結果」規則不啟用，改由第 7 題建議的出貨 fingerprint fixture，加上 simulator 檢查（含 C1 的 DFF 追蹤、C2）把關。
- **之後恢復時的兩條路**
  - Mac 本機：照 `docs/minecraft-server.md`，26.2 伺服器用自帶的 JDK 25；`eula=true` 必須由使用者本人寫。
  - 雲端容器：目前只有 Java 21，而且網路政策擋掉了 `piston-meta.mojang.com`、`piston-data.mojang.com`，要先在環境設定裡開放這兩個網域。

### 4. seven_segment 的 98 ticks 與大葉（建議：大葉當附加候選，98 接受長期紅燈）

- **原問題**：seven_segment 要追平 98 ticks，需要 ≥ 42-gate 的葉（k=1），超過今天的 32 grain。要允許更大的葉（會提高撞 A\* 上限的風險），還是接受 seven_segment 的 ticks gate 長期紅燈？另外，交接規則寫了「不藉調 partition/grain 避開問題」，D2 和 T2 屬於品質優化，是否同意納入？
- **建議**
  - (a) 允許大葉，但只以 T6 的兩個附加候選的形式加入：新增常數 `WIDE_LEAF_GATES=48`，不動 `TERMINAL_GATES=32`，由 Q1 的 dominance 選擇。
  - (b) D2、T2 只有在改寫成附加候選時才合乎交接規則。具體條件：
    - 今天的產物留在清單的原位置。
    - 參數是程式裡的固定常數。
    - 對所有電路用同一套規則。
    - A\* 上限、router 和 root certify 都不變。
    - 每個被拒的候選都出現在 Q1 的 `candidates` 報告裡。
    - 照計畫原本「就地取代」的寫法則不合規。D4.1 和 D4.3 也適用同樣的檢查。
  - (c) 接受 seven_segment 的 98 ticks gate 長期紅燈（硬限制 6 不允許改 gate）。改追蹤不擋合併的目標：
    - Phase 1（T6）：seven_segment ≤ 142 / ≤ 15,261，segment_a ≤ 80 / ≤ 4,183。
    - Phase 2：若 T3 能把葉內的腿壓到 ≤ 12 ticks，seven_segment 約 112–115。
- **依據**
  - 大葉可行，而且收益很大：seven_segment 142 / 15,261，segment_a 80 / 4,183，兩者的 blocks 都贏 legacy。見 T6。
  - 撞 A\* 上限的風險是真的，但只花時間：42-gate 葉 queue 峰值到上限的 91%，全部由既有的 seed repair 吸收；84-gate 單葉 695 s 後被拒，自動退回 42+42。
  - 98 不實際：k=1 時實測葉內 54、跨越 70 ticks，要到 98 需要 C ≤ 26，比今天那條幹線本身（約 28 ticks）還短；k=0（84 單葉）被拒。
  - 「k=1 需要 ≥ 42-gate 葉」在結構上不成立（32/30/22 就能 k=1），但那個切法實測失敗（162 / 21,725，591 s），見 T2。
  - 交接規則讀上下文，是在 seven_segment 還無法 certify 時寫的，每一處明確禁止調 grain 的地方（`:9`、`:209`、`:262`）都綁在 g19 階梯 bug 上；`:19` 則和「不改 baseline、不降 acceptance」放在同一條。附加、固定、報告所有拒絕的候選不會隱藏任何東西，符合它的精神。
- **待決定**
  - 要不要加 T6 的兩個寬葉候選（`WIDE_LEAF_GATES=48` 併進 `producer_revision`），還是所有葉維持 ≤ 32。
  - 接不接受 seven_segment 的 ticks gate 長期紅燈，並改用上面那組追蹤目標。
  - T6 在 Q1 下只分到 1 個 worker，推估 Mac 上 185–240 s。在 Mac 實測 ≤ 180 s 之前，T6 要不要等 P1、P2 加上「分 worker 的靜態規則、P5 或 S1」其中一項完成。

### 5. legacy baseline 本身的物理性（建議：照預設結案）

- **原問題**：baseline 由 `compile_legacy` 產生（`F/benchmark.rs:251`）。2026-09-27 已查 and4、full_adder、seven_segment 的 legacy 世界：懸空 0；segment_a 尚未查。若 segment_a 也乾淨，baseline 就是合法的比較標準。預設：JSON 凍結不改，C1 的 Support 檢查補查 segment_a 並在報告註明。
- **結果**：已查完，全部乾淨。
  - 5 個 legacy 世界（and4、verilog:and4、full_adder、segment_a、seven_segment），用 certifier 同一條規則（`unsupported_component`）檢查，全部 0 懸空，共 12,698 個元件。
    - segment_a 有 3,208 個（2,955 dust、203 repeater、46 wall torch、4 lever），0 懸空。
  - 規則的兩個已知漏洞也補查了：19 個 lever 全部是 Floor 貼在石頭上；y=0 只有 Solid 和 Lamp，沒有需要支撐的元件。
  - 今天的 `compile_legacy`（`d8f3af0`）重現了 JSON 的每一個欄位，包括 `generated_world_fingerprint`，所以查的就是 baseline 當初那批世界，不需要回到 afe577d 重查。
  - baseline 世界就是 `compiled.world` 本身：unpinned 案例的 driver 重用 `compile_legacy` 自己放的 lever（`F/benchmark.rs:690`），probe 只加在 pinned 輸出上（699 行）。
  - 今天的 fragment_synth 產物用同一條規則和漏洞檢查也全部 0，兩邊用同一把尺。
  - 範圍說明：這裡只查支撐規則，沒有把 legacy 世界跑完整個 `certify_root_world`；也只代表符合本 repo 的規則，不等於在 vanilla 驗證過。
- **建議**：照預設結案。JSON 逐位元凍結，gate 不動。C1 加一個非 ignored 測試，釘住 5 個 legacy fingerprint 並斷言 Support 0 違規（見 C1 第 3 步）。
- **待決定**
  - 同意結案。
  - 凍結 JSON 的守門測試 `tests/fragment_synth_baseline.rs:56` 現在是紅的，見 C0。建議在測試裡把 `1af20604…` 釘成常數、另外報告漂移，不改 JSON。

### 6. DFF 路徑（建議：可以接受，C1 不用等）

- **原問題**：C1 之後 DFF 可能從「出貨壞世界」變成「編譯失敗」，可以接受嗎？
- **建議**：可以接受，C1 不需要先修 DFF。
  - C1 第 3 步的 Support 掃描加入時序電路，並加多次切換輸入的功能追蹤。
  - 現在不加 `compile_grown` fallback。
  - 既有的 DFF 問題另立 C8。
- **依據**
  - 實測 C1 第 1 步之後，repo 裡 8 個不同的 DFF 世界沒有一個變成編譯失敗，所有 DFF 測試照常通過。
  - 唯一改變的 `dff_enable` 是變好：原本有一個 dust 疊在 dust 上（拿掉懸空 dust 後 32 次取樣錯 9 次），C1 之後乾淨重繞，0/32 錯。其他 7 個逐位元不變。
  - 計畫原本要跑的兩個 DFF 測試不會建世界，偵測不到這件事，見 C1 的更正。
  - 目前沒有任何 catalog 或 viewer 電路是時序電路，使用者影響小。不過 native SV 前端的路線圖會讓使用者碰到這條路，見 C8。
- **待決定**
  - 同意 C1 照原樣落地，不以 DFF 修正為前提。
  - 同意先不加 `compile()` → `compile_grown` 的 fallback，等 C8 的功能 gate 就緒再評估。

### 7. 180 s 上限與跨機器 fingerprint（建議：秒數只報告，fingerprint 寫進 fixture）

- **原問題**：每案 180 s 上限要當成正式的合併 gate 嗎？CI 的核心數和這台 Mac 不同，1 vs N 要不要跨機器比對 fingerprint？
- **事實**
  - repo 沒有 CI：沒有 `.github/`，GitHub Actions 的 workflow 數是 0。唯一的 gate 是手動執行 `check.sh`。
  - 180 s 出自交接文件 `:20`、`:143`，是給 agent 的迭代規則。計畫在 §2 把它升格成合併規則，但沒指定機器。
    - 不過 `docs/superpowers/specs/2026-08-05-redstone-eda-design.md:937` 顯示，擁有者把「每次要等好幾分鐘」視為真正的產品問題（該處講的是 fast / refinement 模式）。
  - 跨機器實測：
    - 這台 x86_64、4 核、rustc 1.94.1 重建的 5 個 viewer synth 檔，和 Mac（arm64、8 worker）烤的 sha256 完全相同。
    - acceptance 入口的世界 fingerprint 也和 baked 檔一致。
    - 原因：浮點只用 IEEE 精確運算；`HashMap` 只做查找；`sort_unstable` 只用在原生型別；平行結果依 index 合併。
  - 時間取決於機器：

    | 案例 | Mac（8 worker） | 4 核 x86_64（4 worker） | 比值 | 1 worker |
    |---|---|---|---|---|
    | segment_a | 118.4 s | 160.9 s（3 次中位數） | 1.36× | 194.0 s |
    | seven_segment | 161.4 s | 236.6 s（單次） | 1.47× | 315.5 s |
    | pinned decoder | 36.7 s | 46.9 s（3 次中位數） | 1.28× | 82.3 s |

    - 閒置時重複量測的差異不到 2.2%。
- **建議**
  - (i) 180 s 不當硬合併 gate。
    - M1 報告印出 `compile_ms`、`measure_ms` 和機器標記。
    - 180 s 和 §4 的秒數目標保留為參考機上的軟目標（10 核 Mac、8 worker；load < 1.0 時開始；3 次中位數；max/min > 1.05 就重量）。
    - 另外可以加一個不隨機器改變的工作量計數（A\* 展開數加 queue 數，只算「串行等價」會被用到的 job）。先當 fixture 欄位，變動時要在 PR 明講；等 M1、M2 證明它和 wall 相關再設上限。Q1、D2、T6 本來就會讓它上升。
  - (ii) 跨機器用「寫死的預期值」比對，不是兩台機器即時互比。
    - 新增 `tests/fixtures/fragment_synth_shipping_fingerprints.json`，baseline JSON 不動。每案記 `producer_revision`、case fingerprint、candidate fingerprint、`canonical_world_fingerprint`（之後加工作量）。
    - 在 `run_budget_zero_case`（`tests/fragment_synth_acceptance.rs:159`）裡、`assert!(case.passed())` 之前檢查，並用獨立的錯誤訊息。否則三個紅燈案例永遠跑不到這一步，漂移也會被已知的品質失敗蓋住。
    - C5 的 sweep 在每台機器上都對照這份 fixture。Mac 涵蓋 1/2/8、4 核機涵蓋 1/2/4。
    - 09-27 量到的 `canonical_world_fingerprint`（凡有 baked 檔的都和 Mac 一致）：

      | case | canonical_world_fingerprint |
      |---|---|
      | and4 | `9dceef51a784f74a3e568b571b2206b1c92471ff21fabaadaea5d069ac784222` |
      | verilog:and4 | `38755625b7d92fbfdb64a6023a3d84bfa58481e17fffdfaf8d0abfc662756da4` |
      | full_adder | `899a6a536b59f52f1c08a17441c158759b43874b887856da159b7cbca8cba3f5` |
      | segment_a | `59578911145b965553876289c3c91fa77ac02caefca6db932e1852e862121ed5` |
      | seven_segment | `06972a439e229c3824036482773fd237b59c0f11e59f11b071c254c11dc7b730` |
      | pinned:verilog:seven_segment | `03f7e1309c575ba832b5b0267c1de25f5179e51989c0786d6629ff5d9986d051` |

  - (iii) 若將來加 CI：
    - 每案設約 450 s 的卡死防護。libtest 沒有 per-test 逾時，要用 nextest 的 terminate-after 或外包 `timeout`。
    - 450 s 只防卡死，不是 2 倍回歸偵測器：實測最差的 1 核慢速比是 pinned 的 2.24 倍，180 × 2.24 ≈ 404 s。
    - 至少 2 vCPU，否則 1 vs N 沒有意義。
    - `rust-toolchain.toml` 釘版本可選：兩台機器 rustc 版本不同，產物仍一致，fixture 才是真正的守門。
- **待決定**
  - 秒數只報告、另設工作量計數，還是堅持設 wall gate？若設 wall gate，確認參考機是 10 核 Mac、8 worker、3 次中位數。
  - 同不同意新增出貨 fingerprint fixture（baseline JSON 不動）。
  - 要不要設 CI，例如在公開 repo 上跑 GitHub Actions。目前沒有。
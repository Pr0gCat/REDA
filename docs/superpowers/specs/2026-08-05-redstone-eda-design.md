# REDA — 紅石 EDA 架構設計

日期：2026-08-05
狀態：架構定案，待實作計畫

---

## 1. 目標

把 VHDL 編譯成延遲最小化的 Minecraft 紅石電路。

- **通用編譯器**：任何可合成的 VHDL 都能編譯。紅石電腦是其中一個測試案例，不是唯一目標。
- **輸出**：`.litematic` 藍圖 + 中間網表 JSON + 自帶模擬器驗證結果。
- **延遲最小化的定義**：最小化組合邏輯關鍵路徑的 redstone tick 數，該數字決定電路可運行的最高時脈。

### 非目標（v1）

- Bedrock 版支援
- 類比／訊號強度編碼（SSD）邏輯 —— 架構預留位置，v1 不實作
- **litematic 的自動邏輯反推**。讀取本身是需要的（社群結構要當測資與 benchmark 對照組），不做的是「自動推導出它的邏輯功能並註冊成 rule」那一段 —— 見 §10.2

---

## 2. 為什麼紅石值得一套自己的 EDA

紅石的成本模型與 CMOS 有幾處根本差異，這些差異定義了整個系統的設計：

| 性質 | CMOS | 紅石 |
|---|---|---|
| 線延遲 | 連續（RC） | **階梯**：`ceil(len/15)` 個中繼器，每個 1 tick |
| 15 格內的線 | 有成本 | **完全免費** |
| 扇出 | 越多越慢 | **延遲上免費**，只吃體積與繞線空間 |
| 閘的扇入 | 串聯越多越慢 | **任意扇入 NOR 都是 1 tick**（實體上限約 5） |
| 製程變異 | 有 | **無**。拓撲相同即行為相同 |
| 時序分析 | 統計估計 | **精確整數運算** |
| 幾何 | 2.5D，層數固定 | **完全 3D，層數不限** |

兩個直接後果：

1. **延遲主要是佈局問題，不是邏輯問題。** abc 壓低邏輯深度之後，真正決定關鍵路徑長度的是「相鄰的閘擺得多遠」。
2. **placement 不是連續優化，是離散約束滿足。** 成本函數的梯度處處為零，解析式 placer 直接失效。真正的目標是「關鍵路徑上每一跳都保持在 15 格內」，而非「線長總和最小」。

理論下限因此可以明確定義：**總延遲 = 邏輯深度 × 1 tick，繞線貢獻 0**。品質指標就是「關鍵路徑上有幾跳被迫插了中繼器」。

---

## 3. 整體流程

```
VHDL
  │
  ├─[外部] GHDL → Yosys ──► gate-level netlist (JSON)
  │
  ▼
┌──────────────── REDA ────────────────┐
│  word dialect                        │
│    └─ e-graph (equality saturation)  │
│  gate dialect                        │
│    └─ e-graph，含 tech mapping        │
│  phys dialect   ← placer, router     │
│  block dialect  ← peephole           │
│    └─ .litematic 輸出                 │
│                                      │
│  redstone-core（橫貫全流程）           │
│    等價性驗證 / DRC / 延遲量測         │
└──────────────────────────────────────┘
```

前端不自己寫。VHDL 的 parser 與 RTL 合成是已解決的問題，重寫只會得到比 GHDL 差的結果，且與紅石無關。我們唯一保留的干預點是 **abc 的 liberty 檔由我們撰寫**，映射策略仍在掌握中。

---

## 4. redstone-core：地基

所有驗證機制的基礎。等價性驗證、DRC、延遲量測、benchmark 對比全部依賴它。

### 4.1 雙引擎

| | 快速引擎 | 忠實引擎 |
|---|---|---|
| 模型 | 理想化。net 層級距離場，紅石粉瞬間穩定 | 完全複製 Java 1.20：scheduled tick、TickPriority、neighbor update 順序、QC、紅石粉遞迴 power 重算 |
| 用途 | 編譯迴圈內的等價性驗證（每次編譯數千次呼叫） | 最終 sign-off、locational 偵測 |
| 速度 | 快數個數量級 | 慢，只跑數次 |

**兩者行為不一致 = 電路依賴了 Minecraft 的詭異行為。** 不需要另寫 QC 偵測器或 locational 偵測器，分歧本身就是警報。忠實引擎再配合 update-order fuzzing（隨機打亂更新順序重跑），可揪出隱藏的位置相關性。偵測到即退回重繞。

### 4.2 效能要求

快速引擎的效能是硬需求，不是 nice-to-have —— 提煉迴圈的秒級目標直接建立在它之上。

- 事件驅動，非每 tick 全掃描
- 增量式：router 改動局部，只重算受影響區域
- net 層級抽象：不逐格模擬紅石粉，直接算距離場
- 儲存：編譯期 bounding box 已知，密集 3D array + palette

### 4.3 模擬器自身的驗證

三道防線：

1. **對照 MCHPRS**（Rust 寫的高效能紅石伺服器，紅石規則實作完整）跑同一批電路比對
2. **黃金軌跡**：真實遊戲中蓋測試電路，逐 tick 錄下狀態匯出。須涵蓋 QC 觸發、1-tick pulse、中繼器鎖存、比較器減法模式、觀察者鏈
3. **社群 litematic** 當測試套件

### 4.4 模組邊界

```
redstone-core
 ├── world      方塊狀態定義 + 3D 儲存 + palette
 ├── rules      規則集介面（v1: java_1_20；後續 java_1_21）
 ├── sim_fast   理想化引擎
 ├── sim_exact  忠實引擎
 └── probe      座標 ↔ 邏輯訊號綁定
```

`rules` 做成 trait 是為了之後加入 1.21，但 v1 只實作一套，不做無謂抽象。

### 4.5 Quasi-connectivity 策略

QC 只影響活塞、發射器、投擲器。**v1 的 cell library 全部使用火把／中繼器／比較器，因此產生的電路不會出現 QC。**

QC 進場的時機是活塞記憶體與社群結構。屆時的原則是：結構內部使用 QC 沒有問題（自洽），要防的是外部侵入 —— 每個生成器產生的結構帶 **keep-out 禁區**，router 不得侵入（對應 ASIC 的 blockage）。

---

## 5. IR 與 e-graph

### 5.1 四層 dialect

| 層 | 內容 | 誰在這層工作 |
|---|---|---|
| `word` | 保留位寬與算術結構（`$add`、`$mux`、`$dff`、`$mem`） | analog SSD、memory generator、datapath 識別 |
| `gate` | 純布林 DAG，NOR 為主 + DFF | 邏輯優化、tech mapping |
| `phys` | cell 實例 + 3D 座標 + net 幾何 | placer、router |
| `block` | 方塊陣列 | peephole、litematic 輸出 |

lowering 是 `word → gate → phys → block`。

**e-graph 只用在 word 與 gate 層。** phys 帶座標，重寫不是純代數的，走傳統 pass pipeline。

**IR 設計約束**：word 層的位寬與規則結構資訊，在 lowering 到 gate 時必須以標註形式保留 —— datapath 的程式化佈局完全依賴它（見 §7.2）。

### 5.2 為什麼是 equality saturation

LLVM 最惡名昭彰的 phase ordering 問題（pass 順序影響結果，最佳順序無解，只能人工調校 `-O2`），在 equality saturation 下不存在 —— 所有 rewrite 同時反覆套用，最後用成本函數抽取。沒有順序，因為沒有序列。

equality saturation 在真實編譯器不普及的主因是成本模型不準，抽取時不知道哪個形式真的比較快。**紅石的成本是精確整數 tick，這個障礙不存在。**

實務注意：e-graph 不接受 cycle，而電路有回授。切在暫存器邊界 —— 只在組合邏輯區塊內做 saturation，DFF 當 cut point。這與 STA 的切法一致。

### 5.3 tech mapping 併入抽取

「用哪個實體結構實作這坨邏輯」是一條 `gate → phys` 的 lowering rule。作法是讓 e-graph 節點可以是實體結構實例，成本為實測 tick。

於是**邏輯優化與技術映射不再是兩個階段，而是同一次抽取裡互相競爭的選項**。

### 5.4 相對 LLVM 的簡化

| LLVM 需要 | REDA |
|---|---|
| `-passes=` 手工排序的 pipeline | 不需要，e-graph 自己管 |
| `PreservedAnalyses` 失效機制 | 不需要，e-graph 是非破壞性的，舊形式永遠留在等價類裡 |

加一條技巧的成本應低於 LLVM 加一條 InstCombine pattern —— 不必煩惱它跟別的 pass 打架或該排在哪。這是「提煉迴圈是產品」這個主張的底氣。

---

## 6. Cost function

### 6.1 複合成本

egg 的 `CostFunction` 只要求 Cost 型別有序，不要求純量。各分量有自己的組合規則：

```rust
struct RedstoneCost {
    tick:    u32,   // 關鍵路徑延遲   → max 組合
    volume:  u32,   // 3D 體積(方塊)  → sum 組合（DAG-aware）
    cells:   u32,   // cell 數        → sum 組合（DAG-aware）
    fanout:  u32,   // 最大扇出       → max 組合
    density: f32,   // cells/volume   → 導出，擁塞代理指標
}
```

### 6.2 用體積估計線延遲

抽取當下不知道佈局，但**體積可以 bottom-up 算，而線長與體積相關**。3D 空間中體積 V 的子電路，其對外線長約在 `V^(1/3)` 量級（Rent's rule 的 3D 類比）：

```
volume(n)      = cell_volume(n) + Σ volume(children)      // DAG-aware
wire_est(c)    = ceil( k · volume(c)^(1/3) / 15 )
tick(n)        = 1 + max over children ( tick(c) + wire_est(c) )
```

這把「子電路越大、拉出的線越長、要多插中繼器」編碼進純 bottom-up 的成本函數。

係數 `k` 由 benchmark 校準：拿實際跑過 full P&R 的電路回歸取得。這是估計，但驗證成本低 —— 跑一次 full P&R 就知道準不準。

### 6.3 扇出的不對稱

紅石的扇出**延遲上免費、體積上不免費**。因此 `fanout` 只影響 `volume` 與 `density` 分量，**不影響 `tick` 分量**。這是成本模型必須與傳統 EDA 分道揚鑣之處。

### 6.4 DAG-aware 抽取

egg 預設抽取是樹狀 DP，會重複計價共享的子電路，嚴重高估面積。我們的網表是 DAG，共享子電路只需蓋一次。

最優 DAG extraction 是 NP-hard。實務路線：ILP（小電路可求最優）／貪婪近似（大電路）。參考 egg 社群的 `extraction-gym` 的比較結論，不自行從頭試。

`tick` 分量不受影響 —— max 組合下重複計算不會出錯。**我們最在意的分量剛好最不受此坑影響。**

### 6.5 多目標排序

字典序，可設定：

```
預設：    tick > volume > cells
面積模式： volume > tick > cells
```

字典序會拒絕「多花 1 tick 換體積減半」這類划算交易，因此保留 **ε 容忍**：`tick` 在最佳值 +ε 內視為同級，再比 volume。ε 由使用者指定。

### 6.6 塞不進去的東西

| | 原因 | 對策 |
|---|---|---|
| 真實擁塞 | 全域性質，綁定實際座標 | `density` 當代理，靠 P&R 迴圈回饋修正 |
| context-sensitive 成本 | 破壞 DP 的局部性 | 多輪迭代：先抽一輪標出關鍵路徑，第二輪調權重 |
| 全域最優 DAG extraction | NP-hard | ILP／貪婪 |

### 6.7 兩檔模式

成本越複雜抽取越慢，而抽取在提煉迴圈裡跑上百次：

- **fast**：只算 `tick` + `cells`，貪婪抽取。供 `reda check-rule`
- **full**：完整複合成本 + ILP 抽取。供正式 benchmark

---

## 7. Placer

### 7.1 為何不用 analytical placement

成本是階梯函數 `ceil(len/15)`，梯度處處為零 —— 從 3 格移到 12 格成本完全不變，優化器看不到方向。解析式方法失效，必須走離散路線。這是被成本模型逼出來的，不是偏好。

### 7.2 分兩類處理

**Datapath（規則結構）→ 程式化佈局，不搜尋**

ALU、暫存器、移位器的 N 個 bit slice 結構相同。word dialect 保留了位寬資訊，編譯器知道哪些 cell 屬於同一 bit slice，佈局可以直接算出來。規則佈局天然 bus 對齊、好繞線、低延遲。

這是白拿的勝利，前提是 word 層的結構資訊不在 lowering 時遺失（見 §5.1 的 IR 設計約束）。

**Random logic（控制邏輯）→ 模擬退火**

狀態機、解碼、雜項控制沒有規則性，只能搜尋。SA 對離散／階梯成本天然適合（不需梯度），FPGA 界的 VPR 已驗證多年。成本函數可直接放**關鍵路徑實際 tick 數 + 擁塞估計**，不需 HPWL 這種代理指標。

**Hierarchical placement**：沿用 VHDL 的 module 階層，每個 module 先獨立擺成一塊，再擺 block。搜尋空間大幅縮小，結果對人類也可讀。

---

## 8. Router

### 8.1 PathFinder（negotiated congestion routing）

1. 每條 net 用 A\* 找路（3D 曼哈頓距離當 heuristic，非常準）
2. 允許暫時重疊
3. 逐輪提高擁塞區域的通行代價
4. rip-up & reroute 直到無衝突

天然支援時序驅動：關鍵 net 給高優先權與低繞路容忍度，非關鍵 net 讓路。對應「非關鍵路徑愛拉多遠拉多遠」的策略。

### 8.2 中繼器插入

繞線同時要決定中繼器位置。訊號從 driver 出發有 15 的強度預算，用完必須插中繼器（+1 tick）。插入位置在 1~15 之間有自由度，可用於避開擁塞。

**中繼器可設 1~4 tick，等於免費的可調 delay line。** 紅石電路經常需要多條訊號同時到達（bus 對齊、進位鏈同步），在 ASIC 需插 delay buffer 花面積，在紅石只需把既有中繼器轉檔位。時序對齊幾乎零成本。

### 8.3 DRC 兩層

- **第一層（繞線時）**：保守幾何規則 —— 線間距 ≥2（含對角）、紅石粉須有載體方塊、垂直爬升的階梯結構。擋掉九成問題且快。
- **第二層（繞完後）**：快速引擎跑等價性驗證，抓漏網的耦合。

兩層都需要。只靠第二層則每輪驗證太慢；只靠第一層則紅石的隱性耦合列不完。

### 8.4 Clock

低 skew 的樹狀分配。skew 由繞線延遲差決定。（intentional skew 是可用的工具，但屬後續優化。）

### 8.5 兩檔模式

| | 用途 | 做法 |
|---|---|---|
| **fast** | 提煉迴圈、rule 回歸測試 | datapath 程式化佈局 + 粗略 SA + 解析式線延遲估計，不真繞線 |
| **full** | sign-off、benchmark 正式數字 | 完整 SA + PathFinder + 忠實引擎驗證 |

### 8.6 Timing-driven 迴圈

抽取 → 佈局繞線 → 取得**實際**線延遲 → 標註回 e-graph → 重新抽取。跑兩三輪收斂。這是真實 EDA 也在做的事。

---

## 9. 記憶體

`$mem` 是 Yosys 對 RTL 陣列的推斷結果，其參數（`ABITS`、`WIDTH`、埠數、`INIT`、`RD_CLK_ENABLE`）恰好就是 memory generator 的輸入規格。

- `$dff` → 一般 cell（中繼器鎖存做 D-latch，master-slave 兩級成 DFF，2 tick），走一般 P&R
- `$mem` → **memory generator**，`word → phys` 的 lowering rule

**不使用 Yosys 的 `memory_map`。** 它會把 256×8 的 RAM 展開成 2048 個 latch 加巨大 mux 樹，在紅石上是數萬方塊、讀取路徑十餘 tick，且被通用 P&R 隨機灑開。不可用。

memory generator **是 generator 不是 macro** —— 它是吃 depth/width 參數長出結構的程式碼，不是死的方塊陣列。這與 §10 的原則一致。

- `WR_PORTS = 0` 且有 `INIT` → ROM（位址解碼樹 + 硬接線）
- 有寫埠 → RAM（tileable bit-cell 陣列 + 解碼器）
- `RD_CLK_ENABLE = 0` → 讀取路徑是組合的，但紅石上有物理延遲，時序模型須算成多 tick 路徑

---

## 10. 技巧提煉：本專案的價值累積機制

### 10.1 不用巨集，只用 rule

**巨集與 e-graph 互斥。** e-graph 的威力來自小規則的組合探索；黑盒巨集是不可分解的原子，saturation 對它們無事可做。

巨集另有結構性缺陷：8-bit 加法器的巨集遇到 11-bit 就無用；巨集邊界固定，跨邊界的優化機會直接放棄；累積下來的是成品而非知識。

因此：**知識一律以最小、可組合、可參數化的 rule 形式存在。**

### 10.2 提煉不能自動化

從一個結構看出它為什麼快、抽象成通用原理，是創造性工作。**提煉由專案作者與 AI 協作進行。**

工具的角色不是自動註冊，而是 **gap analysis**：

```
社群最佳 8-bit 加法器 (.litematic) ──► 實測 4 tick
                                          │
同功能 VHDL ──► REDA ──────────────────► 9 tick
                                          │
                             差距 5 tick = 尚未學會的技巧
```

**用差距定位未知的技巧。** 工具負責讀入、模擬、標出關鍵路徑、與我們的輸出逐段對比，指出「你在這三處各慢了 2 tick」。人負責看懂那三處在幹嘛並抽象成 rule。

### 10.3 北極星指標

**REDA 距離人類手刻還差幾 tick。** 每加一條 rule 重跑整個 benchmark，差距縮小多少一目瞭然。

benchmark 套件應早期建立：4-bit 加法器、7-seg 解碼、桶形移位器、ALU 等，每項備妥「VHDL 版本」與「社群最佳 litematic」兩份。這是唯一的客觀標尺。

### 10.4 rule 的兩種形式

| 類型 | 層 | 形式 | 例子 |
|---|---|---|---|
| 代數重寫 | gate | 固定 pattern，egg `rewrite!` | NOR 塔重組、火把鏈化簡 |
| 參數化生成器 | word | egg `Applier` trait，右式是函數 | N-bit 進位鏈、memory generator、analog 加法器 |

第二類是關鍵 —— 紅石許多技巧是「N 級重複結構」（進位鏈、解碼樹、移位器），固定 pattern 表達不了。

### 10.5 不做 DSL

egg 的 `rewrite!` 巨集本身就是宣告式語法：

```rust
rewrite!("nor-tower-fold"; "(nor (nor ?a ?b) ?c)" => "(and-nor ?a ?b ?c)")
```

再包一層 markup 等於用 DSL 生成 DSL，純虧：多了 parser、codegen、錯誤訊息、IDE 無支援、跨層 debug。TableGen 的難用是前車之鑑。

且一半的 rule（參數化生成器）本來就必須是程式碼，任何 markup 都表達不了。加 DSL 的結果會是兩套心智模型並存，比統一用 Rust 更糟。

**熱載入不需要 DSL 就能達成**：egg 的 `Pattern` 實作了 `FromStr`，代數 rule 可以從檔案讀、執行期解析、改完立即生效。只有參數化生成器需要重編譯，而這類 rule 少。

### 10.6 rule 的檔案紀律

不加 DSL 不代表可以隨意撰寫。強制要求：

- 一條 rule 一個檔案，與其 rationale、來源、預期改善的 benchmark 放在一起
- **`rationale` 是強制的**。三個月後回來看，沒有它無人知道當初在想什麼，也是 AI 能否接手繼續提煉的關鍵
- 測試是資料檔不是程式碼（抄 lit/FileCheck 的精神）：輸入電路 + 預期 tick 數
- rule 集中註冊於一處

### 10.7 工具鏈（CLI、純文字輸出）

| 指令 | 作用 |
|---|---|
| `reda explain <bench>` | 關鍵路徑逐段 tick 明細：每一跳是哪個 cell、花幾 tick、為什麼 |
| `reda gap <bench>` | 與人類最佳解逐段對比，指出差距落在何處 |
| `reda check-rule <rule>` | 單獨驗證等價性 + 跑全 benchmark 看有無 regression |

`check-rule` 特別重要：**每條 rule 都要有自己的體檢報告**。提出一條 rule，系統須立刻回報它是否等價、改善多少、有無害到其他 case。沒有這個，加 rule 就是盲改。

**提煉迴圈必須是秒級的。** 若每次要等三分鐘，提煉會變成折磨，這個專案的核心機制就死了。這是對快速引擎與 fast 模式的硬需求。

---

## 11. 技術選型

### 11.1 Rust

**取捨背景**：專案作者不打算逐行閱讀實作程式碼，掌控透過設計文件、benchmark 指標、rule rationale 與遊戲內實測進行。

在此前提下：

- **嚴格的型別系統從負擔變成資產。** 無人 review 的 AI 產出程式碼，需要編譯器當第一道關卡。窮盡 match、`Result` 強制處理、借用檢查會擋掉大量錯誤
- **egg 現成。** congruence closure 效率、rebuild 延遲策略、e-class analysis 傳播，坑很深，是最不該自造的輪子
- **MCHPRS 也是 Rust**，可直接參考而非只當對照組
- **效能**：模擬器與 router 是重度計算，且提煉迴圈要秒級

**痛點隔離**：作者需要接觸的是 rule 層，而 rule 在 Rust 中形式乾淨（見 §10.5），沒有生命週期或 trait bound。引擎內部的囉嗦由 AI 承擔。

### 11.2 目標版本

Java 版 1.20 起，1.21 之後加入（銅燈可作累積性記憶）。`rules` trait 預留版本化介面，v1 只實作一套。

### 11.3 外部相依

- GHDL（VHDL 合成）
- Yosys + abc（netlist 與 technology mapping）
- 紅石 liberty 檔由本專案撰寫

liberty 應以 **NOR-only、fan-in 至 5** 為主體。紅石的 NOR 扇入幾乎免費（一個方塊可被多條線充能，火把插旁邊即為任意扇入 NOR，1 tick），實體上限僅來自「一個方塊有幾面能餵線」。這比提供一堆 AND/OR/XOR 能壓出低得多的邏輯深度。

---

## 12. 開發階段

| 階段 | 內容 | 理由 |
|---|---|---|
| **A** | redstone-core 雙引擎 + litematic 讀寫 | 地基。可獨立驗證 —— 拿社群現成結構跑，比對真實遊戲行為（讀取是為了灌測資，不含邏輯反推） |
| **B** | walking skeleton：4-bit 加法器 VHDL → 完整流程 → litematic → 遊戲中實測可用 | 打通端到端，逼所有介面定案，早期取得「真的動了」的回饋 |
| **C** | e-graph 框架 + cost function + gap analysis 工具 | 有驗證地基才敢做重寫 |
| **D** | placer / router 認真做 | 延遲最小化的主戰場 |
| **E** | memory generator、1.21 支援、analog SSD | 擴充 |

benchmark 套件在階段 A 末期即開始建立 —— 階段 A 用來驗證模擬器的那批社群 litematic，正是階段 C 之後 gap analysis 的輸入，一份資料兩處使用。

每個階段各自產出實作計畫，本文件只定架構。

---

## 13. 已知限制

1. **cost function 的線延遲是估計值。** `k` 係數靠 benchmark 校準，並靠 timing-driven 迴圈修正。e-graph 抽取出的是「該成本模型下的最優」，不是全域最優。
2. **面積會被高估**（DAG extraction 的近似）。延遲不受影響。
3. **擁塞無法進入抽取階段**，只能靠 `density` 代理與 P&R 迴圈回饋。
4. **提煉無法自動化**，是持續的人力與智力投入。這是設計上的選擇，換得的是知識而非成品。
5. **v1 產生的電路不使用 analog 編碼**，密度與延遲不會達到頂尖人類手刻的水準。差距由 gap analysis 量化，作為後續 rule 的來源。

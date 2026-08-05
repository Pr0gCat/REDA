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
| 閘的扇入 | 串聯越多越慢 | **扇入不影響延遲**，但實體上限只有 4（實務 3） |
| 製程變異 | 有 | **無**，但**不等於**拓撲相同即行為相同 —— 見下方警告 |
| 閘延遲 | 統計估計 | **精確整數 tick** |
| 線延遲 | 統計估計 | 佈局後精確，佈局前仍須估計 |
| 幾何 | 2.5D，層數固定 | **完全 3D，層數不限** |

> **⚠️ 「拓撲相同即行為相同」在原版 1.20 是不成立的。** locational 電路確實存在（成因見 §4.1）。這句話只在「電路對更新順序免疫」的前提下成立，而**確保這個前提是編譯器的責任**，不是紅石送我們的禮物。整份文件對「精確時序分析」的樂觀，都建立在這個前提上。

兩個直接後果：

1. **延遲主要是佈局問題，不是邏輯問題。** abc 壓低邏輯深度之後，真正決定關鍵路徑長度的是「相鄰的閘擺得多遠」。
2. **真正的目標是硬約束滿足，不是總線長最小化** —— 「關鍵路徑上每一跳都保持在 15 格內」，而非「線長總和最小」。這與傳統 placer 的優化方向不同（placer 的方法選擇見 §7.1，該處尚未定案）。

理論下限因此可以明確定義：**總延遲 = 邏輯深度 × 1 tick，繞線貢獻 0**。品質指標就是「關鍵路徑上有幾跳被迫插了中繼器」。

### 2.1 紅石特有的硬約束

這些不是可以優化掉的成本，是物理上限，必須進 cell library 與 STA。

**火把 burnout**：紅石火把若在 60 遊戲刻內被迫切換超過 8 次就會燒毀。60 遊戲刻 = 30 紅石刻；若某個火把每個時脈週期切換兩次（輸入每週期都翻轉），則週期不得短於約 7.5 紅石刻。

這給時脈週期設了**第二個下限**：

```
clock_period ≥ max( 關鍵路徑 tick 數, burnout 下限 )
```

對真實規模的 CPU，關鍵路徑通常遠大於 7.5 tick，burnout 不會是瓶頸。但小電路與 benchmark 很容易撞到，**STA 必須把它算成一條獨立檢查**，否則會產出「模擬跑得過、實際會燒火把」的電路。

**NOR 扇入上限是 4，實務是 3**。原因是紅石粉的充能規則：紅石粉只充能它腳下的方塊與它**指向**的方塊，**永遠不會充能上方的方塊**；孤立的點狀紅石粉不指向任何方向。因此一個方塊最多 5 個可餵入位置（4 個水平指入 + 1 個放頂面），火把本身再佔掉一個面 → 上限 4。wiki 的說法是 3 個輸入容易、第 4 個要費工，超過就該拆成 OR 樹再接反相器。

**中繼器會重設訊號強度為 15 並強充能方塊**。對 v1 的 binary 邏輯無妨，但未來做 analog／SSD 時，中繼器對訊號強度運算是破壞性的，不能隨意插在類比路徑上。

**衰減只發生在紅石粉之間**。訊號從紅石粉傳到方塊或元件時不衰減。距離模型要照這個算，不能一律 -1。

**弱充能不能續傳**。只被紅石粉充能的方塊是**弱充能**，它**不能再去充能相鄰的紅石粉**。所以「一條線」不是自由的幾何物件 —— 每一段都必須以火把／中繼器／比較器這類主動元件收尾才能續傳。這是 cell library 與 router 的結構性約束，§2 表格裡「15 格內的線完全免費」是在這個前提下才成立的簡化。

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

**兩者行為不一致 = 電路依賴了 Minecraft 的詭異行為。** 不需要另寫 QC 偵測器或 locational 偵測器，分歧本身就是警報。偵測到即退回重繞。

#### Locational-ness 的精確成因

紅石粉更新功率時，把待通知的座標放進 `java.util.HashSet`，再依 HashSet 的迭代順序送出 block update。迭代順序由 `hashCode()` 決定，而 `BlockPos` 繼承 `Vec3i.hashCode()`：

```
(y + z * 31) * 31 + x
```

**所以更新順序是絕對座標的確定性函數。** 同一個電路平移到別的座標，那些 datum point 被通知的先後就變了 —— 這就是 locational 電路的完整成因。

常見的錯誤歸因，一併排除：

| 說法 | 判定 |
|---|---|
| 方向順序取決於座標 | ❌ 方向順序是寫死的常數（NC：west, east, down, up, north, south） |
| scheduled tick 排程不確定 | ❌ block tick 先按 priority 再按排程順序，是確定性的 |
| chunk section 迭代順序 | ❌ 無任何一手依據 |
| 取決於方塊放置順序 | ❌ 取決於**絕對座標**，與放置歷史無關 |

#### 第二個來源：block event 佇列

活塞推拉、音符盒、箱子開闔走的是另一套機制 —— block event 佇列，實作是 `ObjectLinkedOpenHashSet`，其 hash 同時包含 `BlockPos.hashCode()`、方塊狀態與事件參數，**所以處理順序同時受座標與朝向影響**。

這與紅石粉那條是**兩個獨立的 locational 來源**。v1 不使用活塞，只有紅石粉那條相關；活塞記憶體進場後，兩條都要處理。

#### 這帶來比原計畫更強的驗證方法

既然更新順序是座標的函數，它就是**可計算的，不是隨機的**。所以不必靠隨機 fuzzing 取樣 —— 可以**窮舉座標偏移，覆蓋所有可達的 datum point 排列**，直接證明電路對更新順序免疫，而不只是取得統計信心。

同理，忠實引擎有機會做到 bit-exact：複製 Java `HashSet` 的 bucket 迭代行為即可。

#### 一個必須主動拒絕的誘惑

既然行為取決於座標，placer 理論上可以「挑一個能讓電路正確的座標」。**不可以。** 那會產生只在特定座標成立的藍圖，貼到別處就壞。策略仍然是產生對更新順序免疫的電路。

#### 版本註記

1.13、1.16、1.20 都沒有修過 locationality。1.21.2 的實驗性「Redstone Experiments」開關嘗試消除它（改用 BFS 依功率流方向傳播），但 Mojang 明示該開關「未針對任何未來版本」，截至目前未進入預設遊戲。我們的目標版本行為不變。

### 4.2 效能要求

快速引擎的效能是硬需求，不是 nice-to-have —— 提煉迴圈的秒級目標直接建立在它之上。

- 事件驅動，非每 tick 全掃描
- 增量式：router 改動局部，只重算受影響區域
- net 層級抽象：不逐格模擬紅石粉，直接算距離場
- 儲存：編譯期 bounding box 已知，密集 3D array + palette

**參考實作**：Alternate Current 與 RedstoneWireTurbo 都是為了消除 locationality 而重寫的紅石粉實作，改用 BFS 依功率流方向傳播、先把整條線的訊號強度算完再送更新。那個模型正是快速引擎要的東西 —— 理想化、非 locational、且已被證明與原版在非病態電路上等價。

### 4.3 模擬器自身的驗證

三道防線：

1. **對照 MCHPRS**（Rust，MIT 授權）跑同一批電路比對。
   **適用範圍比預期窄很多**：redpiler 認得的元件只有中繼器、比較器、火把、石按鈕、燈、拉桿、石壓板、紅石線，以及有 comparator override 的容器 —— **沒有活塞、沒有 observer、沒有 dropper、沒有 QC**，且明確不支援高訊號強度邏輯。它也是預先編譯的，設計上不模擬 block update 順序。
   所以它只能對照**我們 v1 那批 cell 的邏輯功能**（剛好涵蓋，這是好消息），但**最需要交叉驗證的那些行為（QC、活塞、observer、locational、BUD、0-tick）它一項都驗不了**。那些只能靠黃金軌跡。
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

QC 只影響活塞、發射器、投擲器（1.21 的合成器明確**沒有** QC）。**v1 的 cell library 全部使用火把／中繼器／比較器，因此產生的電路不會出現 QC。**

#### 機制（常見說法是錯的）

QC 常被描述成「檢查自己正上方那格是否被充能」。**這不精確。** 實際判定是「**假如上方那一格是機械元件，它會不會被啟動**」—— 把鄰居充能判定整個平移一格向上。所以掃描的是上方那格的鄰居：

| | QC 掃描範圍 |
|---|---|
| 活塞 | 上兩格 + 四個斜上方，共 5 個位置（排除活塞自己；此迴圈**不**排除面向側） |
| 發射器／投擲器 | 上方那格的 6 個鄰居（同樣不排除面向側） |

正上方那一格本身是靠普通鄰接規則生效的，不算 QC。（活塞的一般鄰接充能會跳過頭部側，發射器／投擲器不會。）

**這對 router 的 keep-out 區域有直接影響**：不是「別在活塞正上方走線」，而是「別進入上方那格的鄰居區域」，範圍大得多。

#### 更新語意

「QC 不會觸發方塊更新」的說法也要修正。真正的機制是：活塞／發射器**只在自己收到方塊更新時才重新評估充能**，而紅石元件發出的方塊更新只到曼哈頓距離 2 格內 —— **讀取範圍大於更新範圍**，這才是 BUD 的根源。因此 QC 分成兩類，忠實引擎兩類都要實作：

- **Immediate QC**：紅石粉、中繼器、比較器、紅石火把等。改變充能的同時也更新了活塞位置，立即生效
- **Update-based QC**：紅石塊、被充能的方塊、日光感測器、軌道等。需要外部更新才生效，這類才是 BUD 開關的基礎

反向副作用同樣存在：失去 QC 充能也不會被察覺，活塞會卡在伸出狀態（budded）直到下一次更新。

> **來源限制**：以上以 minecraft.wiki 的 1.20 頁面與 MCP-919 的 **1.8.9** 反編譯原始碼交叉驗證，行為描述吻合，但未取得 1.20 的逐字原始碼；此邏輯自 Beta 以來無版本變更紀錄。§4.3 的黃金軌跡測試必須涵蓋此項，以實測為準。

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

紅石相對 CMOS 的優勢是**閘延遲精確**（整數 tick，無統計成分），抽取時的比較基準比 CMOS 可靠。但要誠實：**線延遲在抽取當下仍是估計**（§6.2），所以「成本模型完全精確」這個說法不成立，不能拿它當選擇 equality saturation 的唯一理由。

而 equality saturation 真正的障礙也不是成本模型，是 **e-graph 爆炸與不終止**。這點必須寫清楚：

- egg 的 `Runner` 預設上限是 30 iterations / 10,000 nodes / 5 秒。**非平凡的問題幾乎都是撞上限停下，而不是真的 `Saturated`。**
- 原始論文（Tate et al.）的措辭是 equality saturation「**obviates the need to worry about** optimization ordering」，不是消除；而其 canonizing property 明文以「**if the saturation engine terminates**」為前提，作者自承一般情況下可能不終止。

所以我們選 equality saturation 的理由要重新表述為：**它把「順序」這個維度從人工調校變成預算問題**。預算夠就逼近最優，預算不夠就退化 —— 但退化是可觀測、可調整的（調 node limit / iteration limit），不像 pass ordering 那樣是黑箱。這仍然是划算的交易，只是沒有原本寫的那麼夢幻。

實務注意：**e-graph 是接受 cycle 的**（一個 e-class 可以是自己的後代，這正是它表達力的來源）；受限的是 **extraction** —— 抽出的表達式必須無環，egg 的預設 extractor 會讓純環狀 e-class 拿不到成本而放棄。所以電路的回授不會炸掉 e-graph 本身。我們仍然切在暫存器邊界，但理由是**組合邏輯區塊本來就是 STA 的分析單位**，不是「e-graph 不吃 cycle」。

### 5.3 tech mapping 併入抽取

「用哪個實體結構實作這坨邏輯」是一條 `gate → phys` 的 lowering rule。作法是讓 e-graph 節點可以是實體結構實例，成本為實測 tick。

於是**邏輯優化與技術映射不再是兩個階段，而是同一次抽取裡互相競爭的選項**。

### 5.4 相對 LLVM 的簡化

| LLVM 需要 | REDA |
|---|---|
| `-passes=` 手工排序的 pipeline | 不需要，e-graph 自己管 |
| `PreservedAnalyses` 失效機制 | 不需要，e-graph 是非破壞性的，舊形式永遠留在等價類裡 |

加一條技巧的成本應低於 LLVM 加一條 InstCombine pattern —— 不必煩惱它該排在哪個位置。這是「提煉迴圈是產品」這個主張的底氣。

**但「零互動」是過度宣稱。** egg 預設啟用 `BackoffScheduler`，會暫時封禁匹配數爆炸的規則（預設 match limit 1000、ban 5 iterations）。加上共用的 node/time 預算，**規則之間確實會互相排擠** —— 一條匹配過廣的新 rule 可能把別的 rule 擠出這一輪。

這不是 phase ordering（不依賴人工排序、結果不隨順序改變），但也不是無交互作用。實務後果是：`reda check-rule` 的 regression 檢查**必須跑全 benchmark**，不能只驗這條 rule 自己該改善的 case —— 否則排擠效應會無聲地讓別的電路變差。

---

## 6. Cost function

### 6.1 複合成本

egg 的 `CostFunction` 要求 `type Cost: PartialOrd + Debug + Clone` —— 不必是純量，所以複合成本可行，各分量有自己的組合規則：

```rust
struct RedstoneCost {
    tick:    u32,   // 關鍵路徑延遲   → max 組合
    volume:  u32,   // 3D 體積(方塊)  → sum 組合（樹狀上界，見 §6.4）
    cells:   u32,   // cell 數        → sum 組合（同上）
    fanout:  u32,   // 最大扇出       → max 組合
    density: ???,   // 擁塞代理       → 不可為浮點，見下方警告
}
```

> **⚠️ 兩個 egg 的實作地雷**
>
> 1. **`PartialOrd` 實際上被當全序用。** egg 的 `extract.rs` 是 `a.partial_cmp(b).unwrap()` —— **拿到互不可比的值或 `NaN` 就直接 panic**。`density` 若寫成 `f32` 的 `cells / volume`，在 `volume == 0` 時是 `inf` 或 `NaN`，只要它進入排序就會炸。**改用定點整數**（例如 `cells * 1024 / max(volume, 1)` 存成 `u32`），別讓浮點進到比較路徑。
> 2. **預設 `Extractor` 的貪婪 DP 隱含要求成本 local 且對子成本單調。** 不滿足時**不會報錯**，只會安靜地給出非最佳解。我們的複合成本必須逐項確認單調性，並在測試裡對小電路與窮舉最優解比對。

### 6.2 用體積估計線延遲

抽取當下不知道佈局，但**體積可以 bottom-up 算，而單一條線的長度與體積相關**。體積 V 的子電路，從它拉一條線出來的典型長度在 `V^(1/3)` 量級 —— 這是**純幾何的立方體邊長論證**：

```
volume(n)      = cell_volume(n) + Σ volume(children)      // 樹狀近似，見下方警告
wire_est(c)    = ceil( k · volume(c)^(1/3) / 15 )
tick(n)        = 1 + max over children ( tick(c) + wire_est(c) )
```

這把「子電路越大、拉出的線越長、要多插中繼器」編碼進 bottom-up 的成本函數。係數 `k` 由 benchmark 校準：拿實際跑過 full P&R 的電路回歸取得。

> **不要把這稱作 Rent's rule。** Rent's rule 是**計數律**（`T = t·g^p`，terminal 數 vs gate 數），不是長度律，指數 p 是經驗量（0.5–0.75）而非由維度決定。上面的 `V^(1/3)` 只是立方體邊長，與 Rent 無關。
>
> 兩者的關係是：對外**線數** ~ `g^p`，每條線**長度** ~ `g^(1/3)`，對外**總線長** ~ `g^(p+1/3)`。我們的公式估的是**單條線長**，所以 `V^(1/3)` 用在這裡是對的；但它**完全沒有捕捉到「子電路越大、對外連線數也越多」**這件事，而那正是擁塞的來源。
>
> 實務後果：`wire_est` 會系統性低估大子電路的繞線困難度，因為它只看一條線。這個缺口由 `density` 分量與 §8.6 的 P&R 回饋迴圈補，不能靠調 `k` 補 —— 調 `k` 只會把錯誤的函數形式硬擬合到 benchmark 上，內插看起來準、外插必崩。

### 6.3 扇出的不對稱

紅石的扇出**延遲上免費、體積上不免費**。因此 `fanout` 只影響 `volume` 與 `density` 分量，**不影響 `tick` 分量**。這是成本模型必須與傳統 EDA 分道揚鑣之處。

### 6.4 DAG-aware 抽取

egg 預設抽取是樹狀 DP，會重複計價共享的子電路，嚴重高估面積。我們的網表是 DAG，共享子電路只需蓋一次。

> **§6.2 的 `volume` 公式就是樹狀的。** `volume(n) = cell_volume(n) + Σ volume(children)` 對子節點求和，共享子電路必然被重複計價。這不是筆誤可以修掉的東西 —— **DAG-awareness 原則上無法表達成 bottom-up 的 local cost function**，那正是 DAG extraction 為 NP-hard 的原因。所以 §6.2 的 `volume` 只能當「樹狀上界」用，真正的 DAG 面積要在抽取層處理。

最優 DAG extraction 是 NP-hard（可由 set cover 化約；另有結果指出無法在任意常數比例內近似）。

**實務路線：以貪婪為主，不預設用 ILP。** `extraction-gym` 的實測（PR #16，220 個測例）顯示：bottom-up 貪婪總計約 3.4 秒，ILP-CBC 約 316 秒（**慢約 92 倍**），而 DAG cost 的幾何平均只改善約 **0.2%**；其原始碼裡 ILP 後端標註 `use_for_bench: false, // takes >10 hours sometimes`。花兩個數量級的時間換 0.2%，在我們「提煉迴圈要秒級」的前提下是完全不划算的交易。

`tick` 分量不受此坑影響 —— max 組合下重複計算不會出錯。**我們最在意的分量剛好最不受影響。**

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
- **full**：完整複合成本 + **較好的 DAG 近似演算法**（非 ILP，理由見 §6.4）。供正式 benchmark

ILP 抽取只保留為研究用的離線選項 —— 用來偶爾量測「我們的貪婪離最優還差多少」，不進正常流程。

---

## 7. Placer

### 7.1 方法未定案

> **本節先前的論證是錯的，已撤回。** 原本主張「成本是階梯函數 `ceil(len/15)`，梯度處處為零，所以 analytical placement 失效」。這個推論三層都不成立：
>
> 1. **解析式 placer 從來不對真目標函數求導。** HPWL 本身就是 max/min 組成的 piecewise-linear 不可微函數 —— 整個領域的做法就是換平滑代理（quadratic、log-sum-exp、weighted-average）。「目標不可微 ⇒ 解析式失效」被 analytical placement 的定義本身反駁。
> 2. **`ceil(len/15)` 是 `len` 的單調遞增函數。** 要最小化它不需要 `ceil` 的梯度，只需要 `len` 的梯度，而 `len` 是連續的。
> 3. **ASIC 的 buffer insertion 數量本質上就是 `ceil(len/L_opt)`**，跟每 15 格插一個中繼器是同一個數學結構，業界照用解析式流程不誤。
>
> 附帶一個不利情報：VPR（原本被引用來背書 SA）的 master 分支已在 2026-06 把 **analytical placement 設為預設流程**，理由是線長平均改善約 10%；傳統 SA 流程現在要明確指定才會跑。

真正該考慮的障礙是**別的**，而且都還沒評估完：

- **3D lattice 的 legalization**。解析式流程的 global placement 產生連續座標，之後要 legalize 到合法格點。紅石是 3D 格點 + 方塊佔位 + 元件朝向（中繼器／比較器／火把都有方向），legalizer 的難度遠高於 2D standard cell row。
- **紅石的 net 有方向性語意**，不是對稱的 HPWL 問題 —— driver 到 sink 的強度預算是單向的，且中繼器插入會重設預算。
- **目標是硬約束滿足**（每跳 ≤15）而非最小化總和，這與解析式流程的目標函數形式不同，但可以用約束懲罰項表達。
- **tick 量化確實造成 timing 目標的大片 plateau**，讓 timing-driven net weighting 的訊號 piecewise-constant、對小位移不敏感。這是真實的困難，但正確的表述是「timing weighting 的訊號品質下降」，不是「方法失效」—— 而且 plateau 對模擬退火**同樣有害**（大片中性區 = 隨機遊走）。

**待決策**：global placement 走解析式還是離散搜尋。這需要在階段 D 之前以小規模實驗決定，不在本文件定案。§7.2 的 datapath 程式化佈局與 hierarchical 分解則不受此決策影響，兩條路都適用。

### 7.2 分兩類處理

**Datapath（規則結構）→ 程式化佈局，不搜尋**

ALU、暫存器、移位器的 N 個 bit slice 結構相同。word dialect 保留了位寬資訊，編譯器知道哪些 cell 屬於同一 bit slice，佈局可以直接算出來。規則佈局天然 bus 對齊、好繞線、低延遲。

這是白拿的勝利，前提是 word 層的結構資訊不在 lowering 時遺失（見 §5.1 的 IR 設計約束）。

**Random logic（控制邏輯）→ 模擬退火**

狀態機、解碼、雜項控制沒有規則性，只能搜尋（若 §7.1 決定走解析式，這部分改為 global placement 之後的 detailed placement）。

> **成本函數不能直接放「關鍵路徑實際 tick 數」。** 這是先前寫錯的地方。SA 要跑 10⁶–10⁸ 次 move，**每次 move 重算一遍真實關鍵路徑在計算上不可行**，而且與 §10.7「提煉迴圈要秒級」直接衝突。VPR 用增量 bounding box + 週期性更新的 criticality 預算，正是為了避開這件事。
>
> 我們也必須用同樣的結構：**每次 move 只做增量的線長／hop 數更新，criticality 每隔 N 次 move 才重算一次。** 「不需要代理指標」這句話收回 —— 代理指標是效能上的必需品，不是傳統 EDA 的偷懶。

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

**中繼器可設 1~4 tick，是很便宜的可調 delay line** —— 紅石電路經常需要多條訊號同時到達（bus 對齊、進位鏈同步），在 ASIC 需插 delay buffer 花面積，在紅石只需把既有中繼器轉檔位。

但它**不是免費的，也不是純粹的移相**，router 必須把以下代價算進去：

- **波形失真**：延遲設為 N 的中繼器會把任何短於 N 的 on-pulse 拉長到 N，並直接吞掉短於 N 的 off-pulse。所以調高延遲會改變訊號波形 —— 4 刻中繼器過不了 1 刻脈衝。用它做對齊時，必須確認該路徑上的最短脈衝寬度 ≥ 設定值。
- **下限是 1 紅石刻，拿不到 0**；單顆上限 4 刻，更長要串接，每顆再吃一次上述副作用。
- **會把訊號重設為 15 並強充能**（見 §2.1），未來的 analog 路徑上不能亂插。

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

`$mem` 是 Yosys 對 RTL 陣列的推斷結果，其參數（`MEMID`、`ABITS`、`WIDTH`、`SIZE`、`OFFSET`、`INIT`、`RD_PORTS`／`WR_PORTS`、`RD_CLK_ENABLE`）恰好就是 memory generator 的輸入規格。

**要攔截的是 `$mem_v2`，不是 legacy `$mem`** —— 現行 Yosys 發出的是前者。注意 `$mem_v2` **沒有 `RD_TRANSPARENT`**，該語意已由 `RD_TRANSPARENCY_MASK` + `RD_COLLISION_X_MASK` 取代。（中間過程的 `$memrd_v2` / `$memwr_v2` 由 `memory_collect` 合併成單一 `$mem_v2`。）

- `$dff` → 一般 cell（中繼器鎖存做 D-latch，master-slave 兩級成 DFF，2 tick），走一般 P&R
- `$mem` → **memory generator**，`word → phys` 的 lowering rule

**不使用 Yosys 的 `memory_map`。** 它會把 256×8 的 RAM 展開成 word-wide 的**正反器**（256 個 width-8 `$dff`，不是 latch）加上位址解碼與巨大 mux 樹，在紅石上是數萬方塊、讀取路徑十餘 tick，且被通用 P&R 隨機灑開。不可用。

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

liberty 應以 **NOR 為主體，fan-in 上限 4、主力 3**（理由見 §2.1）。紅石的 NOR 扇入在**延遲上**免費 —— 幾條輸入都是 1 tick，不像 CMOS 串聯 NMOS 越多越慢 —— 但實體上限比直覺低很多，超過就得拆成 OR 樹再接反相器，那是要付 tick 的。

liberty 同時必須標註 **burnout 約束**，讓 STA 能檢查時脈週期下限。

#### abc 的實際能力比預期低

「liberty 是我們唯一的干預點」這個說法要打折，因為 abc 對 liberty 的支援有硬限制：

- **abc 的 liberty parser 會直接跳過所有 sequential cell、tristate cell、多輸出 cell。** 所以 **abc 不映射正反器** —— DFF 要另外走 `dfflibmap`，且應先跑 `dfflibmap` 再映射組合邏輯。
- **`-constr` 只支援 `set_driving_cell` 與 `set_load` 兩行，完全沒有 SDC。** 沒有 clock period、沒有 input/output delay。也就是說 **abc 拿不到真正的時序約束**，我們只能靠 `-D` 丟一個延遲目標給它。
- 缺 `time_unit` 時 abc 會**靜默**假設 1ns。單位一定要寫明，否則錯得無聲無息。

實務後果：abc 給的是「在我們寫的 cell 成本下的低深度映射」，不是「滿足時序約束的最佳映射」。真正的時序收斂責任仍在我們自己的 e-graph 抽取與 P&R 迴圈，不能外包給 abc。

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

### 成本模型與抽取

1. **線延遲是估計值**，且估計式只捕捉「單條線的長度」，沒捕捉「對外連線數隨規模成長」（§6.2）。靠 `density` 與 P&R 回饋補，不能靠調 `k` 補。
2. **面積會被高估**（bottom-up 成本必然重複計價共享子電路，§6.4）。延遲不受影響。
3. **擁塞無法進入抽取階段**，只能靠代理指標與迴圈回饋。
4. **e-graph 會撞預算而非真的飽和**。egg 預設 30 iterations / 10k nodes / 5 秒，非平凡問題幾乎都是撞上限停下。equality saturation 的最優性保證以「引擎終止」為前提，我們多數時候不會滿足這個前提。
5. **規則之間會經由 saturation 預算互相排擠**（`BackoffScheduler` + node limit，§5.4）。所以 `check-rule` 必須跑全 benchmark。

### 外部工具鏈

6. **ghdl-yosys-plugin 官方自述為 experimental / work in progress**，README 的「支援哪些 VHDL 功能」表格至今是 TODO，且有已知的 `rising_edge` 於 conditional expression、record 型別 inout port 等 issue。前端不自己寫這個決策仍然成立，但**「VHDL 進得來」不是可以假設的前提** —— walking skeleton 階段就要確認我們的 benchmark VHDL 全部能通過 GHDL，遇到不支援的構造要有繞道方案。
7. **abc 拿不到真正的時序約束**（無 SDC，見 §11.3），時序收斂責任在我們自己。
8. **MCHPRS 的可對照範圍僅限 v1 那批 cell 的邏輯功能**（§4.3）。

### 方法論

9. **§7.1 的 global placement 方法尚未定案。** 原本的否決理由已撤回，需要小規模實驗才能決定走解析式或離散搜尋。
10. **等價性驗證的規模上限尚未評估。** 目前的計畫是「用模擬器跑等價性比對」，這對小電路可行，但窮舉輸入的成本隨位寬指數成長。大電路需要改用 SAT／BDD 形式化等價驗證或有界隨機測試 —— **本文件尚未決定切換的門檻與方法**，這是階段 C 之前必須補的缺口。
11. **提煉無法自動化**，是持續的人力與智力投入。這是設計上的選擇，換得的是知識而非成品。
12. **v1 不使用 analog 編碼**，密度與延遲不會達到頂尖人類手刻的水準。差距由 gap analysis 量化，作為後續 rule 的來源。

### 事實基礎

13. **本文件的紅石機制描述已經過查核並修正過一輪**，但仍有部分只能以 wiki 與舊版反編譯原始碼交叉驗證，未取得 1.20 逐字原始碼（§4.5）。**最終仲裁者是黃金軌跡的實測結果**，不是本文件。
14. **方塊分類與載體規則（半磚等非完整方塊）尚未寫入本文件**，而它是 router DRC 與 cell library 的地基。查核進行中，補上前 §8.3 的「線間距 ≥2」只是保守佔位值。

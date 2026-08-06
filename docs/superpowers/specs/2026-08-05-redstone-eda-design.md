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

### 2.2 方塊分類：DRC 的地基

**Minecraft 沒有單一的 `isSolid` 屬性。** 紅石行為由三個**彼此獨立**的屬性決定，DRC 必須分成三個欄位建模：

| 屬性 | 決定什麼 |
|---|---|
| **頂面支撐型別** | 能否承載紅石粉／中繼器／火把 |
| **導電性** | 能否被充能並把訊號傳出去 |
| 不透明度 | **只管光照，與紅石無關** |

支撐型別是程式碼裡的 `SupportType` 三值 enum，對應 wiki 的白話術語：

| 元件 | 需要的支撐 |
|---|---|
| 紅石粉 | `FULL`（+ 漏斗是硬編碼特例） |
| **中繼器／比較器** | **`RIGID`** |
| 落地火把 | `CENTER` |
| 牆上火把 | 側面 `FULL` |

> **⚠️ 中繼器的放置條件比紅石粉寬鬆，邏輯是反的。** 「能放紅石粉的地方才能放中繼器」是錯的直覺。三者不是包含關係，是獨立布林值。

#### 導電性的反例（必須查表，不能用規則推導）

- **完整方塊但非導體**：玻璃、幽光玻璃、發光石、海燈籠、TNT、冰、紅石塊、觀察者、活塞
- **半透明但導體**：黏液塊
- **Java 蜂蜜塊不導體、黏液塊導體** —— 最容易搞反的一組
- 半磚／樓梯：**單層非導體**（上下皆然），**雙層半磚是導體**
- 上半磚可承載紅石粉，下半磚不行；倒放樓梯可，正放不行

#### 充能規則

| | 定義 | 來源 |
|---|---|---|
| **強充能** | 可驅動相鄰紅石粉（含上下方） | 電源元件、已充能的中繼器／比較器 |
| **弱充能** | **不能**驅動相鄰紅石粉；但能啟動機械、驅動朝外的中繼器／比較器 | **只有紅石粉** |

- 紅石粉只充能**腳下**與**它指向**的方塊；不指向的側邊不充能。孤立的「十字」形紅石粉會對四個水平方向 + 下方共 5 處弱充能（**無意漏電最常見的來源**；1.16+ 可右鍵切成「點」形態，不對水平相鄰供電）
- 紅石火把：**強**充能正上方，**弱**充能其他相鄰，但**排除它所附著的方塊**
- 中繼器：只對正前方輸出，且是強充能
- **比較器的側面輸入只接受強充能** —— 容易踩雷
- **訊號打不進非導體方塊**：玻璃、上半磚「可以放中繼器」但「不能被中繼器打亮」

#### 垂直傳播：非導體是向上單向導體

- 導體方塊放在低處紅石粉的正上方會**切斷**垂直連接；非導體不會
- **非導體方塊在 Java 版無法向下傳電** —— 所以半磚、玻璃、發光石、倒放樓梯是天生的**向上二極體**。wiki 收錄的 **Transparent Diode：1×2×3，0 tick**

#### 對繞線的實際結論

**平行線的最小 pitch 永遠是 2，換什麼載體都改不了** —— 兩格相鄰的紅石粉必定連成同一 net，那是紅石粉自己的性質。全方塊垂直堆疊同樣能達到 2 格/線的理論下限。**半磚不提高密度。**

pitch 2 也不會串音：直線走向的紅石粉不對左右送電，就算送了也只是弱充能，而弱充能驅動不了紅石粉。**真正需要淨空環的是強充能節點**（中繼器／火把／比較器的輸出方塊），那會驅動上下左右所有相鄰紅石粉。

**玻璃／半磚的正確用途是讓 pitch 2 在有中繼器時也安全**：實心分隔柱被中繼器指到會變強充能而串線，非導體永遠充不上電。建議配置 —— **每 2 格一條線，紅石粉鋪在實心方塊上，兩線之間的分隔柱用玻璃。**

**半磚擋不了 QC。** QC 檢查的是「上方那一格有沒有收到訊號」，wiki 明說「even if that block is air」。放什麼都沒用。

#### cell library 的起始數據

| 元件 | 體積 | 延遲 |
|---|---|---|
| Redstone bridge（線路交叉） | 1×3×4 | **0** |
| Repeater bridge（交叉，省垂直空間） | 2×3×3 | 1 tick（兩條線都加） |
| Transparent diode（單向隔離） | 1×2×3 | **0** |
| Component diode（中繼器） | 1×1×2 | 1–4 tick |
| Block diode | 1×2×2 | 1 tick |
| Redstone ladder（向上） | 1×2×N | 1 tick / 15 格 |
| Redstone staircase（雙向） | 1×N×N | 1 tick / 15 格 |
| Torch tower（向上） | 1×1×N | 1 tick / 2 格 |
| Torch cascade（向下） | 1×2×N | 1 tick / 2 格 |

**線路交叉是零延遲的**（redstone bridge，且不需要半磚）—— 這比原本的假設樂觀。但它有破功條件：**上層線若有中繼器指向中心方塊，會強充能而灌爆下層。**

垂直走線則是半磚真正值得破例的地方：64 條線從第 0 層拉到第 15 層，用透明方塊 ladder 是 1920 格 / 0.1 s；只用全方塊的話，火把塔是 960 格但 **0.75 s**（還會反相 + burnout），階梯是 0.1 s 但 **14400 格**。延遲 ×7.5 或體積 ×7.5，二選一。

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

前端不自己寫，**而且不打包**。VHDL 的 parser 與 RTL 合成是已解決的問題，重寫只會得到比 GHDL 差的結果，且與紅石無關。

**REDA 的入口是 Yosys JSON netlist（或 word dialect IR，見 §5.1），前端是使用者的事。** 這個界線的實際效果：

- Yosys 沒有內建 VHDL 前端，VHDL 必須靠 experimental 的 ghdl-yosys-plugin，而它在 Windows 上取得困難（見 §13）。**把前端排除在相依鏈外，這個問題就從「REDA 跑不起來」降級成「使用者要自備 netlist」** —— 走 Verilog / Chisel / Amaranth 的人根本不會遇到。
- 我們自己的 walking skeleton 仍需驗證「VHDL 真的進得來」，但那是開發環境的問題，不是使用者的部署問題。

#### technology mapping 的分工（兩套 mapper 的界線）

我們有兩個地方在做 technology mapping：abc 的 liberty 映射，以及 §5.3 的 e-graph 抽取。分工必須明確，否則實作階段一定撞車：

| | 職責 |
|---|---|
| **abc + liberty** | 只做**結構映射到抽象 gate**（NOR / INV / BUF），提供一個合理的起始網表 |
| **e-graph 抽取** | 做**實體結構選擇** —— 哪個紅石佈局實作這坨邏輯，成本是實測 tick |

也就是說 **abc 不負責時序收斂**，liberty 不需要精確的 timing 表。原本「abc 給我們延遲最優映射」的說法收回 —— 理由見 §11.3（abc 拿不到真正的時序約束）。

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

1. **對照 MCHPRS**（Rust，MIT 授權）。**它有兩套實作，用途完全不同，別搞混：**

   | | 用途 |
   |---|---|
   | **`crates/redstone/`**<br>（`lib`, `wire/mod`, `wire/turbo`, `repeater`, `comparator`） | **vanilla 紅石語意的 Rust 參考實作** —— 伺服器正常模式跑的那套。這是我們最該讀的東西，同語言、MIT、目標 1.20.4。`wire/turbo.rs` 就是 RedstoneWireTurbo（為消除 locationality 而重寫的紅石粉傳播） |
   | `crates/redpiler/` | 加速用的預編譯器。**不模擬 block update 順序**，且只認中繼器、比較器、火把、按鈕、燈、拉桿、壓板、紅石線與容器 —— 沒有活塞、observer、dropper、QC |

   對照時要清楚自己在對照哪一套：`redstone/` 可作語意參考；`redpiler/` 只能驗**我們 v1 那批 cell 的邏輯功能**，**QC、活塞、observer、locational、BUD、0-tick 它一項都驗不了**，那些只能靠黃金軌跡。

   另外 `crates/blocks/generated.rs`（206 KB）是完整的 blockstate 目錄，`crates/schematic` 可省下格式工作。
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

### 4.6 驗證策略

驗證分散在編譯流程各處，但機制集中在這裡。

**核心原則：沒有任何一層需要對大電路做窮舉功能驗證。** 這靠兩件事達成 —— rewrite 的局部性讓 pattern 永遠很小，而繞線的錯誤是**耦合**問題，可以用比功能驗證便宜得多的方式完備地檢查。

| 層 | 驗什麼 | 方法 | 完備性 | 成本 |
|---|---|---|---|---|
| L1 | 一條 rule 是否等價 | 窮舉 pattern 涉及的那幾個閘 | **完備** | `O(2^k)`，k ≤ 5 |
| L2 | saturation 是否保持語意 | **不需驗** —— e-graph 只儲存等價類，每條 rule 等價則任何抽取結果必等價 | **完備** | 免費 |
| L3 | 參數化生成器 | 小 N（1–8）窮舉，**外加每次編譯對當次的具體 N 驗證** | 對驗過的 N 完備 | 每次編譯一次 |
| L4 | lowering（gate → phys） | 窮舉實體結構的輸入，**且必須做雙向 transition 掃描**（見下） | 組合完備／時序有限 | 便宜 |
| L5 | 繞線是否引入耦合 | **逐 net 注入-觀測**（見下） | 對耦合類 bug **完備** | `O(nets)` 次模擬 |
| L6 | locational 免疫 | **窮舉座標偏移**，覆蓋所有可達的 datum point 排列（§4.1） | **完備** | 中等 |
| L7 | 最終 sign-off | 忠實引擎跑 testbench | 取樣 | 少數幾次 |

#### L4：從 reset 掃真值表是不夠的

一個實體結構可能**每一列真值表從 reset 開始都對，但串進大電路就壞** —— 因為它「能上電、不能放電」：某些 layout 的紅石網路從初始狀態被驅動時會正確亮起，但輸入撤除後放不掉。

這是 Redstone-Compiler 踩過並解決的坑（§14.5）。對策是 **雙向 transition 掃描**：真值表跑一遍升序、再跑一遍降序，讓每個 row 都經歷「從別的狀態轉移過來」而非「從 reset 開始」。

```
for mask in (0..2^n).chain((0..2^n).rev()):
    施加 mask，跑到穩定，比對輸出
```

成本只有兩倍，但它涵蓋的是**狀態相依**的錯誤 —— 那類錯誤在單向掃描下 100% 漏掉。

#### L5：逐 net 注入-觀測

繞線的正確性**不是結構問題**。訊號強度衰減、弱充能規則、QC 都讓「兩個方塊會不會互相影響」變成要模擬才知道的事，所以比對 net 的 driver/sink 集合不夠。

```
for each net:
    只驅動這條 net 的 driver，其餘輸入全部靜置
    跑快速引擎至穩定
    觀測全世界哪些位置被驅動
    比對 == 這條 net 在 netlist 上宣告的 sink 集合
```

多一個位置 = 意外耦合；少一個 = 斷鏈（可能是強度預算算錯或弱充能沒收尾）。

這對「意外耦合」這個 bug 類別是**完備的** —— 不是隨機向量碰運氣。這也是 §8.3 DRC 第二層的實際做法。

#### 為什麼不用 SAT

我們最怕的 bug 全部是**時序與物理**的：繞線耦合、locational、QC 誤觸發、火把 burnout、中繼器吞脈衝、弱充能斷鏈。**沒有一個是 SAT 能驗的** —— SAT 驗布林等價，而我們的問題是「這個結構在真實紅石規則下的行為，是否等於它宣稱的邏輯」。那需要模擬器。

純邏輯正確性也不是我們的風險區 —— GHDL、Yosys、abc 的合成與映射都有成熟驗證。

因此 v1 **不整合 SAT**，但等價性檢查做成介面，日後若需要驗證大 N 的生成器輸出，可接 Yosys 的 `equiv_make` + `sat`。那是工具呼叫，不是架構決定。

#### 三個已知的洞

1. **guard 寫錯抓不到。** conditional rewrite 的 guard 是 Rust 程式碼，L1 驗的是「guard 成立時兩邊等價」，沒驗「guard 本身正確」。guard 太寬鬆會讓 rule 在不該套用時套用，而每一步看起來都合法。**只能靠 benchmark regression 抓** —— 這是 `check-rule` 必須跑全 benchmark 的第二個理由（第一個見 §5.4）。
2. **時序元件的狀態空間**。D-latch 有 1 bit 狀態還能窮舉，多幾 bit 就爆炸。沒有廉價解，對策是**刻意把狀態元件的 cell library 保持小而少**，用限制問題規模繞過去。
3. **生成器的歸納正確性**。L3 只保證驗過的 N，不保證所有 N。實務上靠「每次編譯驗當次的 N」把風險轉成檢查成本，但生成器本身的歸納正確性沒有機器證明。

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

**word dialect 是公開介面，不是內部表示。** REDA 有兩個入口：

```
Verilog / VHDL / SystemVerilog / Chisel / Amaranth ──► Yosys ──► JSON netlist ─┐
                                                                (gate-level)    ├──► REDA
圖形化編輯器 / 程式化生成 ─────────────────────────────► word dialect IR ───────┘
                                                          (結構資訊完整)
```

第二個入口反而是**高品質入口** —— 經過 Yosys 合成後，word-level 的結構經常被打散重組（`a + b` 出來可能已不是可辨識的加法器）；而圖形編輯器裡使用者拖進來的方塊**明確就是一個 8-bit 加法器**，那個意圖是第一手無損的。

所以 word dialect 的格式必須當成穩定的公開介面來設計與版本化。這也讓前端生態與我們解耦：接圖形介面、接其他 HDL、接程式化生成器，後端都不用改。

### 5.2 為什麼是 equality saturation

LLVM 最惡名昭彰的 phase ordering 問題（pass 順序影響結果，最佳順序無解，只能人工調校 `-O2`），在 equality saturation 下不存在 —— 所有 rewrite 同時反覆套用，最後用成本函數抽取。沒有順序，因為沒有序列。

紅石相對 CMOS 的優勢是**閘延遲精確**（整數 tick，無統計成分），抽取時的比較基準比 CMOS 可靠。但要誠實：**線延遲在抽取當下仍是估計**（§6.2），所以「成本模型完全精確」這個說法不成立，不能拿它當選擇 equality saturation 的唯一理由。

而 equality saturation 真正的障礙也不是成本模型，是 **e-graph 爆炸與不終止**。這點必須寫清楚：

- egg 的 `Runner` 預設上限是 30 iterations / 10,000 nodes / 5 秒。**非平凡的問題幾乎都是撞上限停下，而不是真的 `Saturated`。**
- 原始論文（Tate et al.）的措辭是 equality saturation「**obviates the need to worry about** optimization ordering」，不是消除；而其 canonizing property 明文以「**if the saturation engine terminates**」為前提，作者自承一般情況下可能不終止。

所以我們選 equality saturation 的理由要重新表述為：**它把「順序」這個維度從人工調校變成預算問題**。預算夠就逼近最優，預算不夠就退化 —— 但退化是可觀測、可調整的（調 node limit / iteration limit），不像 pass ordering 那樣是黑箱。這仍然是划算的交易，只是沒有原本寫的那麼夢幻。

實務注意：**e-graph 是接受 cycle 的**（一個 e-class 可以是自己的後代，這正是它表達力的來源）；受限的是 **extraction** —— 抽出的表達式必須無環。

處理回授有兩條路，我們選第二條：

- **切在暫存器邊界**（v1 的簡化）：只在組合區塊內 saturate，DFF 當 cut point。理由是組合區塊本來就是 STA 的分析單位。
- **用延遲變數兼作 cycle-breaking**（Nextmap, PLDI 2026 的做法）：「sequential elements are modeled explicitly to permit valid feedback through registers, while combinational cycles are disallowed by bounded propagation delay variables」。**一個機制兩個用途** —— 我們本來就要在抽取階段追蹤關鍵路徑延遲，那個變數同時就是拓樸序，組合迴路自然被排除，而暫存器回授合法保留。

第二條更乾淨，而且不必人為切斷電路。v1 可以先做第一條，但 IR 不該假設「e-graph 裡沒有 register」。

### 5.2.1 規模控制：分區是必需品，不是優化

先前工作的規模天花板有明確數據：E-morphic 論文總結先前工作「**no more than 40,000 e-nodes**」；E-Syn 在 10 個 EPFL benchmark 裡 **9 個 timeout 或 memory-out**；ROVER 最大的 e-graph 只有 17,493 nodes，那已經要 135 秒 ILP。

而 Coward 的觀察讓靜態估算失效：「the final e-graph size is **not well correlated with the number of operators in the initial e-graph**」—— **不能從電路大小預測會不會爆**，只能靠執行期預算與優雅降級。

必要的控制手段（按投報率）：

1. **不追求 saturation，固定 5–13 iterations。** E-morphic 用 5、BoolE 用 10+3。沒有一個成功的系統真的跑到飽和。
2. **分區。** EqMap 有三種現成切法可參考：`r2r`（register-to-register）、`arc-set`、`delay-paths`。
3. **不要用 S-expression 當電路與 e-graph 之間的中介。** E-morphic 明確指出：「shared nodes must be **duplicated in S-expressions**」—— 這是指數級的隱形開銷，也是 E-Syn 在 9/10 benchmark 上炸掉的主因之一。要用唯一 node identifier 直接 DAG↔DAG 建圖。
   （**注意區分**：§10.5 的 rule pattern 用 S-expression 字串是**沒問題**的 —— 那是幾個節點的小 pattern，不是整個電路的序列化。）
4. **solution-space pruning**：每個 e-class 的走訪佇列只保留成本 ≤ 該 class 最小成本的 e-node。
5. **conditional rewrite + 可達性分析**（Coward 的 live e-class analysis，實測 −48% 節點）。

> **⚠️ 粒度警告**：Coward 明言 e-graph 在 **gate level** 有 scalability 問題，ROVER 的成功很大程度來自「一個 e-node = 一個 8-bit 加法器」的粗粒度。**我們的 dialect 粒度越粗越安全**；word 層的 saturation 會比 gate 層健康得多，這是把 word dialect 做扎實的另一個理由。

### 5.3 tech mapping 併入抽取

「用哪個實體結構實作這坨邏輯」是一條 `gate → phys` 的 lowering rule。作法是讓 e-graph 節點可以是實體結構實例，成本為實測 tick。

於是**邏輯優化與技術映射不再是兩個階段，而是同一次抽取裡互相競爭的選項**。

#### structural legality 必須是抽取的約束，不能是後處理

紅石有一類「兩個閘不能直接相接」的規則，最典型的是 **OR→OR**：紅石的 OR 就是兩條紅石粉合流，兩個 OR 直接相接會併成同一個 net，**閘的邊界直接消失**。中間必須插一個主動元件（火把或中繼器）把它們隔開。

這類規則不能等抽取完再修 —— 修補會改變延遲與體積，讓抽取當時的成本比較失效。**它們必須是抽取的合法性約束**，讓不合法的組合根本不進入候選。

（Redstone-Compiler 是用 placer 硬拒絕 + 前置 buffer insertion pass 處理的，那是後處理，也正是他們的 full adder 需要「手工 buffered 版本」才編得出來的原因之一。）

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

紅石的扇出**延遲上免費、體積上不免費**。因此 `fanout` 只影響 `volume` 與 `density` 分量，**不影響 `tick` 分量**。這是成本模型必須與傳統 EDA 分道揚鑣之處（所有 CMOS 的 e-graph EDA 工作都把 fanout 當延遲代價）。

> **⚠️ 但 `fanout` 在 bottom-up 成本函數裡「算不出來」，不是「不夠準」。**
>
> fanout 的定義是「有多少個**被選中的** parent 引用這個節點」。bottom-up 的 `cost(enode, children_costs)` 只拿得到子成本，沒有 parent 資訊 —— 這是表達力問題，不是精度問題。
>
> 這一項只能在 **extraction 層**處理（見 §6.4）。在 `CostFunction` 裡放一個 `fanout` 欄位是自欺欺人。

### 6.4 DAG-aware 抽取

**先講一個讓處境好很多的事實：我們的主目標剛好是 extraction 最容易的情況。**

純 min-delay 的 greedy extraction **就是最優解** —— 延遲只看最長路徑，每個 e-class 各自挑延遲最小的實作即可，不存在跨 e-class 的取捨。所以 `tick` 單目標模式可以直接用 egg 原生 extractor，而且是真最優，不是近似。這同時是免費的 sanity check 與上界。

需要 DAG-aware 的只有**面積類**分量（`volume`、`cells`、`fanout`）。

egg 預設抽取是樹狀 DP，會重複計價共享的子電路，嚴重高估面積。我們的網表是 DAG，共享子電路只需蓋一次。

> **§6.2 的 `volume` 公式就是樹狀的。** `volume(n) = cell_volume(n) + Σ volume(children)` 對子節點求和，共享子電路必然被重複計價。這不是筆誤可以修掉的東西 —— **DAG-awareness 原則上無法表達成 bottom-up 的 local cost function**，那正是 DAG extraction 為 NP-hard 的原因。所以 §6.2 的 `volume` 只能當「樹狀上界」用，真正的 DAG 面積要在抽取層處理。

最優 DAG extraction 是 NP-hard（可由 set cover 化約；另有結果指出無法在任意常數比例內近似）。

#### 貪婪夠不夠？分工作負載，而我們落在壞的那一端

extraction-gym 社群的溫和說法是「貪婪其實差不多」，但 SmoothE 的對照表顯示它高度分裂 —— 相對 10 小時 CPLEX oracle 的 cost 增幅（平均／最差）：

| 資料集 | egg greedy | 改良 greedy | 性質 |
|---|---|---|---|
| diospyros | 0.1% / 0.0% | 0.1% | 共享少 |
| impress | 53.0% / **280%** | **0.0%** | 改良 greedy 救得回來 |
| **rover**（datapath 合成） | 2.9% / **11.0%** | **2.9%（完全沒救回）** | 共享密集 |
| tensat | 12.1% / **46.4%** | 11.9% | 共享密集 |

**紅石的加法器、乘法器就是 rover 那一族**：共享子式密集的 datapath。而改良貪婪對這一類**完全沒有幫助**。

#### 對策：把 extraction 外包，不要自己造

`egraph-serialize`（MIT）約 20 行就能把 `egg::EGraph` 轉成 extraction-gym 的 JSON 格式，接上現成的 DAG-aware extractor。

**首選 e-boost**（MIT、Rust、基於 egg）：平行貪婪 DAG + adaptive pruning + warm-started ILP，實測**比 vanilla ILP 快 558× 且品質好 5.6%**，optimality gap 0.21%，不需要 GPU，可用免費的 OR-Tools CP-SAT。

> **⚠️ 求解器要用 HiGHS，不要用 CBC。** `good_lp` 預設走 `coin_cbc`，需要系統預裝 native library（README 只測過 Debian，**完全沒提 Windows**），且 egg 的 CBC infeasible issue 至今 open，實測在 ASIC 規模題目會回 spurious infeasible。HiGHS 全 MIT、CMake 從 source 編、不需系統預裝。

ILP-CBC 那條原本的規劃收回：不只是慢，在我們的平台上根本裝不起來。

#### `tick` 分量也被污染了

先前這裡寫「tick 不受樹狀重複計價影響，我們最在意的分量剛好最不受影響」。**那是錯的，而且是我們自己造成的。**

§6.2 的 `tick(n) = 1 + max(tick(c) + wire_est(c))` 依賴 `wire_est`，而 `wire_est` 依賴 `volume`。volume 被樹狀抽取高估 → wire_est 高估 → **tick 跟著高估**。引入 `wire_est` 的那一刻就把污染灌進了 tick。

只有**關掉 `wire_est` 的純邏輯深度模式**才真的免疫（而那個模式的 greedy 恰好是最優的，見本節開頭）。

### 6.5 多目標排序

字典序，可設定：

```
預設：    tick > volume > cells
面積模式： volume > tick > cells
```

字典序會拒絕「多花 1 tick 換體積減半」這類划算交易，因此需要 ε 級距。

> **⚠️ ε 不能實作成「容忍」，必須實作成「量化」。**
>
> 「tick 在最佳值 +ε 內視為同級」定義的關係**不具傳遞性**（a~b、b~c 但 a≁c），而 egg 的 extractor fixpoint 依賴傳遞比較。結果會是不收斂或結果隨迭代順序改變，**而且不會報錯**。
>
> 正確做法是量化：比較 `tick / ε` 的整數商。那仍是全序，傳遞性成立。

### 6.6 塞不進去的東西

| | 原因 | 對策 |
|---|---|---|
| 真實擁塞 | 全域性質，綁定實際座標 | `density` 當代理，靠 P&R 迴圈回饋修正 |
| context-sensitive 成本 | 破壞 DP 的局部性 | 多輪迭代：先抽一輪標出關鍵路徑，第二輪調權重 |
| 全域最優 DAG extraction | NP-hard | ILP／貪婪 |

### 6.7 兩檔模式

成本越複雜抽取越慢，而抽取在提煉迴圈裡跑上百次：

- **fast**：純 `tick`（關掉 `wire_est`），egg 原生貪婪抽取。**這個組合的貪婪恰好是最優的**，所以 fast 模式不是「品質換速度」，是「範圍換速度」。供 `reda check-rule`
- **full**：完整複合成本 + e-boost 的 DAG-aware 抽取。供正式 benchmark

### 6.8 工程預算：重心在 extractor，不在 rewrite rules

**extraction 佔 equality saturation end-to-end runtime 的 89.30%**（e-boost 實測），Coward 博論在三個章節重複「ILP solving dominated the runtime」。

而 §5.3 把 technology mapping 併進抽取，等於把最貴的那一段**再乘上「每個邏輯功能有幾種實體實作」**。這條路可行（EqMap 已證明），但它決定了工程預算的分配：**效能優化的力氣應該約 80% 花在 extractor，不是 rewrite rules 或 saturation。**

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

**一個實質的經驗證據偏向解析式**：唯一在遊戲裡做出大規模自動佈線的 LogicLoom 用的正是 **force-directed placement（解 `Ax = f + c`）+ FLUTE rectilinear Steiner tree** —— 恰好就是 PERSHING 在 future work 裡建議的兩件事（放棄格點佈局、用 MRST 取代純 Lee）。它沒有公布數字，所以不能當定量證據，但「這條路走得通」這件事有了實例。

它同時示範了兩個不能照抄的地方：

- **它把 cell 當成點質量**（原始碼裡就寫著 `// TODO: add size for x and z`），尺寸只在 spreading force 與 legalizer 裡出現。紅石的 cell 有實體 3D 體積，這個簡化在我們這裡不成立。
- **它用稠密 LU 每輪每軸從頭重解**，那是硬性的規模天花板。走解析式就必須用稀疏 CG。

它的 legalizer 是 Tetris 式的（往四個方向擴散找第一個合法位置），但**只在 force loop 失敗時才跑**，收斂時完全不 legalize，最後若仍有 overlap 就直接放棄。3D 格點 + 元件朝向的 legalization 仍然是這條路最硬的部分。

**待決策**：global placement 走解析式還是離散搜尋。這需要在階段 D 之前以小規模實驗決定，不在本文件定案。§7.2 的 datapath 程式化佈局與 hierarchical 分解則不受此決策影響，兩條路都適用。

**若走解析式，Steiner tree 用 flute3**（OpenROAD Attic，**BSD-3-Clause**），不要用 LogicLoom 移植的那份 —— 那份原始碼是 Attribution Assurance License（要求執行時顯著顯示歸屬），在該專案裡被重新授權成 MIT 且移除了標示。兩者的查表資料逐位元相同，取乾淨的那份即可。

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

### 8.0 前人正是死在這裡

紅石自動 P&R 的既有工具全部卡在 **~60 cells 的 7-segment decoder**：PERSHING 的 router 從未跑完（論文表格標「still running」），續作 dewey 直接把這個電路從 benchmark 拿掉，MinecraftHDL 獨立地也在同一個電路上爆成 83×442 blocks。三個團隊、不同演算法、同一堵牆。

**死因是漸進複雜度，不是實作品質。** PERSHING 論文寫明 3D Lee 是 `O(l·m·n)` per net：「When the number of logic cells and nets triples, the completion time increases by nearly an order of magnitude.」2016 年的 future work 就指出該換 **Mikami-Tabuchi line search（`O(L)`）**，2018 年的 dewey 仍列在 TODO —— **兩代人都知道要換，兩代人都沒換成。**

PERSHING 提出的四項評估指標中，**Feasibility**（router 是否跑得完；跑不完的話設計者要手動修 violation，而那是 intractable）在社群裡從未被討論，但它才是所有既有工具真正失敗的地方。**這一項要進我們的 benchmark 指標。**

### 8.1 PathFinder（negotiated congestion routing）

1. 每條 net 找路（3D 曼哈頓距離是很準的下界）
2. 允許暫時重疊
3. 逐輪提高擁塞區域的通行代價
4. rip-up & reroute 直到無衝突

天然支援時序驅動：關鍵 net 給高優先權與低繞路容忍度，非關鍵 net 讓路。對應「非關鍵路徑愛拉多遠拉多遠」的策略。

**單條 net 的搜尋演算法未定案**，但 A\*／Lee 這類逐格擴展是已知會撞牆的。要評估的候選：Mikami-Tabuchi line search、rectilinear Steiner tree（LogicLoom 用 FLUTE，是目前公開成果最大的紅石 P&R，做到 8-bit 計算器）。**這個決定比「用不用 GPU」重要得多**（見 §8.7）。

### 8.7 GPU 加速：架構上預留，v1 不做

紅石的繞線問題對 GPU 異常友善，理由有四：

- **PathFinder 的輪內無相依** —— 同一輪裡所有 net 各自用當前擁塞代價獨立繞線，輪末才統一更新。平行維度是演算法自帶的。
- **成本均勻，不需要 priority queue** —— 線長成本每步都是 1，均勻成本下 **wavefront BFS 就是最短路**。GPU 做 Dijkstra 最痛的就是 priority queue，我們繞過了。
- **規則的密集 3D 格點** —— 不像真實 IC 的不規則 routing resource，記憶體存取模式友善。
- **整數成本** —— 無浮點，整數吞吐好。

主要困難是 **DRC 檢查會造成 warp divergence**（紅石粉相鄰規則、弱充能、載體要求、方向性都是分支）。對策是**預計算每個格點的可行方向 bitmask**，把繞線退化成查表 + 位元運算 —— 那個預計算本身也是資料平行的。

**但 v1 不做**，理由是 §8.0：前人卡的是 `O(lmn)` 的漸進複雜度，而 **`O(lmn) → O(L)` 是質變，GPU 的常數倍加速是量變**。演算法沒換對，GPU 只是讓撞牆從 60 cells 延到 200 cells。而且沒有正確的 CPU 參考實作，GPU 版本的 bug 無從調起。

**現在該做的零成本準備**：router 的核心資料結構寫成資料平行友善的形式 —— SoA 而非 AoS、避免指標追逐、成本用整數、格點狀態用扁平陣列。這對 CPU 版本本身就是好事（cache friendly、可用 rayon 平行），同時讓日後移植不必重寫。屆時 **wgpu** 是最務實的選擇（跨平台，Windows 走 DX12/Vulkan，不綁 NVIDIA）。

（extraction 那邊不必先動 GPU：SmoothE 的 GPU 方案需要 A100 級的卡，而 e-boost 純 CPU 平行已經比 ILP 快 558×。）

### 8.2 中繼器插入：搜尋狀態的一部分，不是前後處理

**訊號強度必須是繞線搜尋狀態的一部分**，`(位置, 訊號強度)` 一起當 visited key。強度耗盡時就在該處插中繼器並重設為 15，繼續搜尋。

這個設計來自 Redstone-Compiler（已驗證可行，見 §14.5），而它比「事後貪婪補中繼器」好在兩點：

- **前人失敗的模式正是事後補。** PERSHING 在 routing 之後才貪婪插 repeater，論文自承有「pathological cases 導致訊號無法 buffer，必須整條重繞」。把強度納入搜尋狀態就不會產生無法 buffer 的路徑 —— 不合法的路徑根本不會被展開。
- **強度預算與延遲自然耦合**：每插一個中繼器就是 +1 tick，所以搜尋在最小化路徑長度的同時就在最小化延遲，不需要另一套機制。

以下是中繼器本身的性質：

繞線同時要決定中繼器位置。訊號從 driver 出發有 15 的強度預算，用完必須插中繼器（+1 tick）。插入位置在 1~15 之間有自由度，可用於避開擁塞。

**中繼器可設 1~4 tick，是很便宜的可調 delay line** —— 紅石電路經常需要多條訊號同時到達（bus 對齊、進位鏈同步），在 ASIC 需插 delay buffer 花面積，在紅石只需把既有中繼器轉檔位。

但它**不是免費的，也不是純粹的移相**，router 必須把以下代價算進去：

- **波形失真**：延遲設為 N 的中繼器會把任何短於 N 的 on-pulse 拉長到 N，並直接吞掉短於 N 的 off-pulse。所以調高延遲會改變訊號波形 —— 4 刻中繼器過不了 1 刻脈衝。用它做對齊時，必須確認該路徑上的最短脈衝寬度 ≥ 設定值。
- **下限是 1 紅石刻，拿不到 0**；單顆上限 4 刻，更長要串接，每顆再吃一次上述副作用。
- **會把訊號重設為 15 並強充能**（見 §2.1），未來的 analog 路徑上不能亂插。

### 8.3 DRC 兩層

- **第一層（繞線時）**：幾何規則 —— 線間距、載體要求、垂直爬升結構、強充能節點的淨空環（見 §2.2）。擋掉九成問題且快。
- **第二層（繞完後）**：**逐 net 注入-觀測**（§4.6 L5），對耦合類 bug 完備。

兩層都需要。只靠第二層則每輪驗證太慢；只靠第一層則紅石的隱性耦合列不完。

#### DRC 必須數字化，不能寫成分支

方塊屬性是**三個彼此獨立的布林值**（§2.2），加上方向性、機械元件標記、支撐面型別，一個方塊可以壓進單一 `u16` flag word。於是判斷退化成位元運算：

```
can_carry_dust = below.flags & FLAG_SUPPORT_FULL
conductive     = block.flags & FLAG_CONDUCTIVE
```

再進一步，「從格點 P 往方向 d 走是否合法」所牽涉的鄰域狀態是有限的 —— 把鄰域壓成 index，一張查表直接吐出「六個方向各自合法」的 6-bit mask。inner loop 變成查表 + 位元運算，無分支。

這帶來三個超出效能的好處：

1. **規則變成資料。** 1.20 與 1.21 的差異退化成「換一組常數表」，而不是 trait dispatch 或程式碼分支。§4.4 的 `rules` 版本化因此幾乎免費。
2. **DRC 可以進入成本函數，而不只是當守門員。** 違規程度若是數字而非布林，router 就能**暫時容忍違規、逐輪加重代價** —— 也就是把 PathFinder 對擁塞做的事推廣成 **negotiated DRC**。這對前人卡死的那堵牆可能直接相關：硬性禁區會把 wavefront 切得支離破碎，成本化之後 wavefront 保持連通，router 可以先穿過去再協商出來。
3. **無分支 = GPU 友善**，順帶解掉 §8.7 的 warp divergence 問題。

（更遠的一個推論先記著不定案：違規程度若是連續量，就可被鬆弛與微分，那會讓 §7.1 的 global placement 選擇往解析式傾斜 —— DRC 懲罰項可以直接進 analytical placer 的目標函數。）

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

benchmark 套件應早期建立。可用的素材已經查清楚：

**主標尺 —— ORE 2025 Bounty Board 的帶約束電路**（社群目前唯一接近「帶規格的參考電路集」的東西）：

| 電路 | 社群給的約束 |
|---|---|
| Horizontal 8-bit CCA | ≤5 ticks in→out、每 bit ≤2 blocks 寬、≤8 blocks 高 |
| 4-bit ALU | 2-tall，底層須 solid |
| 32-bit Collatz | ≤20 ticks / iteration |
| 5×5 Game of Life | ≤20 redstone ticks / cycle |
| 2-bit 分支預測器 | ≤8 redstone ticks 響應 |

**門檻座標（查證後修正）**：

| 里程碑 | 規模 | 狀態 |
|---|---|---|
| 4-bit counter | 23 cells | **文獻上真正繞完的最大紀錄**（PERSHING，1084 秒） |
| 7-segment decoder | 63 cells | **從來沒有工具繞完過** —— PERSHING 標「routing did not complete」、MinecraftHDL 產出 83×442 blocks 超過載入半徑 |

所以 63-cell 不是「及格線」，是**沒人到達過的線**。**繞完 23 cells 就追平文獻紀錄，繞完 63 cells 就是這個領域的第一次。**

（唯一可能越過的是 LogicLoom —— 遊戲內截圖顯示數百格見方的自動佈線結果，但它**沒有公布任何 gate 數、尺寸或延遲**，也沒有可重現的 artifact，所以無法納入比較。）

**回歸測試集**：Redstone-Compiler 的 `test/*.nbt` + `*.outputs.json`（MIT，可直接取用）。

**參考量級**：ORE 的 Engineer 級門檻是 CPU「至少 10 ticks/cycle」；MPU 1–7 的跨代 clock 序列為 15→10→6→7→7→5→5 ticks。

> **關於「與人類最佳解的差距」這個指標**：社群沒有公開的 size+latency 統一標準（ISA benchmark sheet 有 cycles/ticks/bytes 但**沒有 blocks 欄位**），CHUNGUS 2 甚至連可用的 world download 都找不到。所以 gap analysis 的對照組只能建立在**有明確尺寸/延遲標註的小型電路**上。若我們同時量測 size 與 latency 並公開，那本身就是社群目前不存在的東西。

#### 反向萃取（world → logic）：benchmark 的第二個來源

從 `.litematic` **反解析出邏輯網表**（等同 EDA 的 LVS）是一項獨立能力，它同時解決兩個問題：

- **對照組的來源**：拿真人蓋的優秀電路反解析成 netlist，就得到「同一份邏輯」的兩種實作 —— 人類的與我們的。不必依賴社群是否公布尺寸數據。
- **驗證的另一個方向**：我們自己輸出的世界反解析回 netlist，應該等於輸入的 netlist。這是獨立於 §4.6 各層的交叉檢查。

> **⚠️ 但它有系統性盲點**：反解析器與 placer 若對某個紅石細節有**共同的誤解**（例如都忽略某個時序或強度規則），這個檢查抓不到 —— 它驗證的是「兩個都用同一套假設的模組彼此一致」。所以它是補充，不能取代 §4.6 的 L5（逐 net 注入-觀測，那是真的跑模擬器）。

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

6. **VHDL 前端不是可靠的相依。** ghdl-yosys-plugin 官方自述 experimental，README 的「支援哪些 VHDL 功能」表格至今是 TODO，已知會直接噴 Ada exception 的構造包括 unconstrained/nested array port、部分 generic、VHDL-2008 unary operator、`**`、shared variable。
   **Windows 上更麻煩**：OSS CAD Suite 的 Windows 版**不含 GHDL 也不含 plugin**（issue 開了三年），唯一路徑是 MSYS2 的 `mingw-w64-ucrt-x86_64-yosys`（它把 plugin 編進 Yosys 本體，所以**不要下 `-m ghdl`**），而它卡在 Yosys 0.66 —— 0.67 換 CMake 後 build system 裡沒有 GHDL option。
   **緩解**：§3 已把 REDA 的入口定在 netlist，所以這只影響「我們自己驗證 VHDL 路徑」的開發環境，不影響使用者。但 walking skeleton 仍須真的跑通一次，屆時要釘住 Yosys 版本並記錄平台步驟。
7. **abc 拿不到真正的時序約束**（無 SDC，見 §11.3），時序收斂責任在我們自己。
8. **MCHPRS 的可對照範圍僅限 v1 那批 cell 的邏輯功能**（§4.3）。

### 方法論

9. **§7.1 的 global placement 方法尚未定案。** 原本的否決理由已撤回，需要小規模實驗才能決定走解析式或離散搜尋。
10. **驗證策略見 §4.6。** 規模問題已解決（靠 rewrite 的局部性與逐 net 注入-觀測，不需對大電路窮舉），但仍有三個洞：conditional rewrite 的 **guard 正確性無法被等價性驗證抓到**（只能靠 benchmark regression）、**時序元件的狀態空間**無廉價解（靠限制 cell library 規模繞過）、**生成器的歸納正確性**沒有機器證明（只保證驗過的 N）。
11. **提煉無法自動化**，是持續的人力與智力投入。這是設計上的選擇，換得的是知識而非成品。
12. **v1 不使用 analog 編碼**，密度與延遲不會達到頂尖人類手刻的水準。差距由 gap analysis 量化，作為後續 rule 的來源。

### 事實基礎

13. **本文件的紅石機制描述已經過查核並修正過一輪**，但仍有部分只能以 wiki 與舊版反編譯原始碼交叉驗證，未取得 1.20 逐字原始碼（§4.5）。**最終仲裁者是黃金軌跡的實測結果**，不是本文件。
14. **`fanout` 無法進入 bottom-up 成本函數**（表達力問題，§6.3），只能在 extraction 層處理。
15. **e-graph 在 gate level 的 scalability 有明確前例警告**（§5.2.1）。我們的 gate dialect 是風險最高的一層。
16. **latch 回授會讓模擬不收斂，這是必踩的坑。** 前人（Redstone-Compiler）的「解法」是讓火把燒毀永不恢復 —— 那是讓模擬器偏離遊戲語意來換收斂。**我們不能這樣做**：忠實引擎必須如實建模 burnout 與其恢復，讓振盪呈現為振盪，由驗證層報錯並退回重繞。這意味著模擬器必須有明確的**發散偵測與上限**（事件數、cycle 數），且發散是一種可報告的結果，不是要被消除的現象。

---

## 14. 先行研究與定位

### 14.1 紅石側：前人卡在哪

| 專案 | 成果上限 | 授權 | 死因 |
|---|---|---|---|
| **PERSHING**（2016, MIT SIGTBD） | 4-bit counter，23 cells，1084 秒，0.49 Hz | BSD-2 | 3D Lee `O(lmn)`；7-seg decoder 未跑完 |
| **dewey**（2018，C 重寫） | 同上，快 34× | BSD-2 | 規模零成長；**無 timing 分析**，連 fmax 都算不出 |
| **MinecraftHDL**（2020, McGill） | 2-bit 7-seg | **無授權** | 7-seg driver 83×442 blocks，超過載入半徑；**明說因「wire delay 難以預測、timing 分析超出範圍」而放棄** |
| **LogicLoom**（現役） | **8-bit 計算器**（目前公開最大） | — | force-directed placement + FLUTE Steiner + Dijkstra |
| **Redstone-Compiler**（現役，Rust） | half-adder | **MIT** | README：「Currently, you can use only unittests」 |
| **V2MC** | 無 demo | GPL-3.0 | 最後 commit：「Revamp fitter (brokenly)」 |

**三件事值得記住：**

1. **沒有任何工具做過驗證。** dewey 的 buffer 插入失敗時只印一行警告，`exit(1)` **被註解掉了** —— 它會靜默輸出壞電路。這正是 §4.6 存在的理由，也是為什麼階段 A 要先做模擬器。
2. **沒有任何工具做過 timing-driven routing。** PERSHING 的 router cost 沒有 timing 項，repeater 是事後貪婪補的。**我們的整個目標函數沒有競爭者** —— 而且 MinecraftHDL 是明確因為這件事太難而放棄的。
3. **現有工具「能動」的方式是主動退化** —— 只用 dust + torch + repeater（所以碰不到 QC），等於放棄了紅石大部分的元件與技巧。這正是它們產出比手工大一到兩個數量級的原因。

**自由 3D 沒有被放棄。** PERSHING 一路都是自由 3D，2016 的 future work 甚至是要求**更自由**（「abandoning the grid arrangement」）。被驗證失敗的是 MinecraftHDL 的硬分層 + channel routing。

### 14.2 e-graph EDA 側：我們的架構被驗證了什麼

| 我們的決定 | 先例 |
|---|---|
| 多層 IR + 優化與 techmap 同時做 | **Nextmap**（PLDI 2026）證明方向正確，且 ablation 顯示「同時做」才是收益來源 |
| e-node 可以是實體結構實例 | **EqMap**（ICCAD 2025）的 LutLang 就是這樣，cut enumeration 被 rewriting 完全取代 |
| 成本用實測延遲 | **E-morphic** 的 quality mode 就是跑真 ABC 拿 post-map delay |
| 撞 node/iteration limit | **是常態不是失敗** —— 沒有一個成功系統真的 saturate |
| 多輸出 cell | **BoolE** 的做法可抄：插共享節點 + 投影偽運算，extraction 視為 atomic |

**rule 的來源也有現成方法論**：BoolE 不是手寫 rule，而是「用 ABC 從 template 電路裡**挖出** structural pattern」。這對我們的技巧提煉可能比純人工分析更有效率。

### 14.3 可直接用的資源

| 資源 | 授權 | 用途 |
|---|---|---|
| **MCHPRS `crates/redpiler/`** | MIT | 紅石語意黃金標準，同為 Rust，目標 1.20.4。**關鍵建模：紅石粉不是節點，是邊的權重**（`CompileLink.ss` = 衰減量）；`LinkType::Side` 區分背面/側面輸入；`facing_diode` 決定 tick priority；`SSRangeAnalysis` 對訊號強度做 interval domain；RIL 文字 IR + golden-file pass 測試 |
| **e-boost** | MIT | DAG-aware extractor 首選 |
| **`egraph-serialize`** | MIT | egg → extraction-gym JSON 的橋（約 20 行） |
| **`eqmap` 0.10.0** | Apache-2.0 | 可直接 `cargo add`。分區策略（r2r / arc-set / delay-paths）、truth-table-in-e-node、`check` 等價驗證模組 |
| **extraction-gym `data/rover/`** | MIT | 9 個真實電路 e-graph，現成的 extractor 回歸測試集 |
| **BoolE** | MIT | 多輸出 cell extraction 的參考實作 |
| **Redstone-Compiler `test/`** | MIT | `.nbt` + `.outputs.json` 回歸測試集 |
| **Coward 博論的 ILP 公式** | CC BY-NC（可引用） | delay big-M 約束（Eq. 5.15–5.17），掃 `d` 即得 Pareto frontier |
| **nucleation** | MIT | litematic 讀寫（`mc_schem`、`rustmatica` 是 GPL-3.0，有傳染性） |

**不能碰**：E-Syn / E-morphic **沒有 LICENSE 檔**（預設保留所有權利，只能讀論文學方法）；MinecraftHDL、redhdl、veristone 無授權；ROVER 閉源。RedstoneBuilder 無 LICENSE 檔，且夾帶了來自無授權專案的 `.schem` 檔（經 blob SHA-1 比對確認）—— 它的 Litematica bit-packing 邏輯正確且值得參考規格，但**照格式自己寫，不要複製檔案**。

### 14.4 我們真正新的東西

四個方向都查無先例：

1. **把 placement 幾何回饋進 e-graph。** 最接近的只有 E-morphic 拿 ABC mapping 結果當 cost。而紅石比 CMOS 更適合做這件事 —— `ceil(len/15)` 是 placement 的**直接函數**，不是統計估計。
2. **精確成本模型。** Coward 用 performance fuzzing 量出商用合成工具本身有 **15% 的 noise floor**，「ROVER 的 cost model 不可能捕捉到」；E-Syn 的 XGBoost surrogate R 只有 0.76–0.78；E-morphic 的 GNN 25.2% MAPE。**紅石沒有 noise floor** —— 這批論文所有關於 cost model 誤差的討論，對我們都不存在。
3. **`ceil(len/15)` 階梯型線延遲。** CMOS 的線延遲連續可線性近似；階梯是非凸非線性，ILP 要 big-M 編碼（可做，沒人做過），SmoothE 的可微鬆弛在這裡不適用。
4. **扇出延遲免費 + 3D 佈局。** 所有 CMOS 工作都把 fanout 當延遲代價；e-graph EDA 文獻也全是 2D standard cell 或 FPGA。

### 14.5 Redstone-Compiler 評估結論

**結論：取用觀念 + 少量程式碼。不貢獻、不 fork、不當依賴。**

#### 模擬器不能用，理由是架構性的

| 我們需要 | 它的現況 |
|---|---|
| 精確的 game tick / redstone tick | **只有自創的 "cycle"**，作者明文寫「不是 game ticks 或 redstone ticks」 |
| tick priority | 純 FIFO，零優先級 |
| 正確的 neighbor update order | 硬編碼且與 MC 不同，**而且每 cycle 去重**（MC 不去重，0-tick 電路正靠重複更新） |
| 逐 tick 傳播才能觀察 glitch | **全域 Bellman-Ford 鬆弛到不動點，原子的** —— glitch 在此模型中不存在 |
| QC 建模（雙引擎分歧偵測的核心） | 零 |
| 比較器、觀察者、活塞 | 沒有 / 沒有 / `todo!()` |

它既不能當快速引擎（給不出有意義的延遲數字），也不能當忠實引擎（不忠實）。

#### 它精準標出了我們必踩的一個坑

他們遇到 D flip-flop 的 latch 回授導致模擬不收斂，**解法是讓火把燒毀變成永久的**（`burned_out_torches` 只有 insert 沒有 remove，還有測試明文釘住「不該恢復」）。Minecraft 的火把燒毀是會恢復的。

**這個坑我們一定會踩到，而且不能用同樣方式逃。** 正確做法是如實建模 tick 與 burnout recovery，讓振盪呈現為振盪，再由驗證層報錯 —— 不是讓模擬器說謊。這條寫進 §13。

#### 四個值得採用的觀念（已寫入本文件）

1. **訊號強度作為繞線搜尋狀態，中繼器在路徑合法性中即時插入** → §8.2
2. **雙向 transition 驗證**（從 reset 掃真值表不夠） → §4.6 L4
3. **structural legality 必須是抽取約束**（OR→OR） → §5.3
4. **反向萃取 world → logic 當獨立能力** → §10.3，它同時是 benchmark 的來源

#### 可取用的程式碼（MIT，需標注 attribution）

- `src/nbt/mod.rs` —— NBT ↔ block palette 對應（先確認方塊涵蓋範圍夠不夠，目前只有 8 種）
- `src/world/position.rs` + `World3D::update_redstone_states` —— 紅石粉連線判定規則，當**參考**讀
- `tools/nbt-viewer/` —— TypeScript，完全獨立，可直接拿來檢視我們的輸出

**絕對不取**：`simulator.rs`、`router.rs`、`placer.rs`、`local_placer/`、`verilog/`。

#### 它的實際能力（用來校準這個領域的現實）

**實際編譯過的最大設計是 2-bit counter**，而且該測試是 `#[ignore]` 的。`test/alu.v` 是無人讀取的死檔案且語法不合法；`test/alu.nbt` 是手工蓋的世界，不是編譯產物。CI 不跑任何 Rust 測試，clean clone 下 `cargo test` 因 `include_str!` 指向被 gitignore 的目錄而編不過。

一個人維護、bus factor 1、有過 13 個月空窗、零外部貢獻、fork 數 0。

**唯一的正面佐證**：它的 IR 分層（Logical 帶 width/Add/Mux/Register\<N\> ↔ Routable 純 scalar gate）與我們的 word/gate 分層對得上 —— 這是四層設計在紅石 target 上站得住腳的一個獨立證據。

### 14.6 一個關於「綠色 CI」的警惕

RedstoneBuilder 是最值得記住的反例：它**真的會編譯、CI 全綠、124 個測試通過、零 `todo!()`**，README 的功能表打滿勾。但：

- **PathFinder 的協商機制實際失效** —— `present_cost` 每代開頭被清空，A\* 讀到的永遠是 0，第 2 代以後逐位元重播第 1 代
- **STA 只有 arrival time**，沒有 slack、沒有 critical path；遇到 cycle 靜默回傳空結果然後回報成功
- **timing 在 P&R 之前跑**，router 拿不到任何 slack
- **D 正反器的 Q 恆為 ON**（火把沒有輸入所以永遠亮），而 sequential 在 README 裡標著 ✅
- 招牌設計（8-bit ALU、8-bit adder）**全部在 `#[ignore]` 裡**，CI 實際驗證過的最大電路是 4-bit adder

**EDA 詞彙正確，EDA 行為不正確。** 這正是本文件 §10.7 堅持 `check-rule` 要跑全 benchmark、以及 §4.6 要求逐 net 注入-觀測的理由 —— 綠色的測試套件本身不保證任何事，只有**跑真電路、量真數字**才算數。

我們自己的 benchmark 必須產出可公開查核的數字（gate 數、blocks 尺寸、tick 延遲、以及 §8.0 的 Feasibility），而不是通過/失敗的布林值。

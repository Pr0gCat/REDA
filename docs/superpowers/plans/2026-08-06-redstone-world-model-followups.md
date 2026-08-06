# A1 遺留事項

A1（世界模型與格式 I/O）已合併。最終 whole-branch review 判定 **ready with follow-ups**，四個 Critical 全部修復並經獨立驗證。以下是刻意留下的事項。

驗證方式：reviewer 自行實作了一個非跨界（1.16+）的打包器，重播全部向量測試 —— 新加的 `raw_long_layout_pins_the_cross_long_convention` 確實失敗，證明它真的能區分慣例。另外對 34 個 A1 測試未涵蓋的真實 1.20 blockstate 做了 `save → load → emit` 的磁碟往返，32 個逐鍵逐值一致，兩個失敗的已在合併前修掉。

---

## A2 開始前必須先做

這三項都會被三個模擬引擎依賴，越晚做越貴 —— 它們會改變 `BlockState` 的相等性，連帶改變 palette 的識別。

### 1. Block entity 支援

`Region` 目前沒有 `TileEntities` 欄位，`load` 直接丟棄方塊實體。

**擋住的東西**：比較器的輸出強度存在方塊實體的 `OutputSignal` 裡，不在 blockstate 上。`power_emitted_by` 現在對已充能的比較器硬編 `strength: 15`，忠實引擎沒有它就重現不了比較器行為。

**附帶損害**：`load → save` 會靜默清空社群藍圖裡的箱子、告示牌與命令方塊。

### 2. 把元件的子狀態從字串提升為型別欄位

以下屬性目前只以字串形式存在 `extra_properties` 裡，round-trip 無損，但程式無法查詢：

| 屬性 | 誰需要 |
|---|---|
| 比較器 `mode`（compare / subtract） | 忠實引擎 |
| 中繼器 `locked` | 忠實引擎 |
| 活塞 `extended` | 方塊層級引擎 |
| 拉桿／按鈕 `face`（floor / wall / ceiling） | 三者皆需 —— `state.facing` 單獨沒有意義，附著方向是 `face` × `facing` |

### 3. 逐面（per-face）查詢 API

`PowerOutput` 刻意回傳「最強的輸出」而不帶方向；`flags_of` 只回答頂面與側面的概括問題。QC 引擎需要知道**哪一面**被充能、火把附著在**哪一面**。

這是在現有 API 之上新增，不是重寫。但有一個陷阱：**`SUPPORT_FULL` 目前被超載成「紅石粉放得上去」而非「這一面是完整實心面」**（漏斗的特例被折進去了）。未來的逐面 sturdiness 查詢**不能**重用這個 bit。

---

## 可延後

### 對稱性與一致性

- **`Button` 沒有把 `facing` 列為結構化欄位，`Lever` 有** —— 兩者的屬性集合完全相同（`face`、`facing`、`powered`）。不丟資料，但呼叫端問 `state.facing` 時，結構相同的兩個方塊一個有答案一個是 `None`。
- **`flags_of` 是雙軌的**：`Glass`/`Piston`/`Observer`/`RedstoneBlock`/`Lamp`/`Target` 走 `state.kind`，`Other` 走 `state.name`。同一個名稱若 `kind` 被設成 `Other`，玻璃、發光石、TNT、活塞等會拿到零旗標。今天無害（只有 `parse_block_name` 建構狀態），但沒有建構子強制這個不變式 —— 而佈局程式碼正是會手動組 `BlockState` 的地方。
  **建議**：在 `world::block` 加一個 `BlockState::from_name(&str)`，目前唯一的這種路徑在 `formats::litematic` 裡。
- **`flags_of` 忽略活塞的 `facing` 與 `extended`** —— 伸出的活塞與收回的活塞回報相同的支撐旗標。

### 資料表

- **`minecraft:bricks` 與 `minecraft:terracotta` 沒有底線前綴**，`FULL_BLOCK_SUFFIXES` 的 `_bricks` / `_terracotta` 抓不到，兩個很常見的完整方塊因此拿到零旗標（fail-safe 方向）。
- **`"minecraft:carpet"` 不是合法的方塊 ID** —— 地毯是 `<color>_carpet`，所以 `SUPPORTS_NOTHING` 實際只涵蓋 `white_carpet`。

### 測試強度

- **NBT round-trip 測試沒有 gzip 鑑別力** —— 若未來兩端同時改成 zlib 或未壓縮，所有測試照樣通過，而產出的每個 `.litematic` 都會變成 Minecraft 讀不懂的東西。加一行 `assert_eq!(&bytes[0..2], &[0x1f, 0x8b])` 即可。
- **`required_longs_matches_what_pack_produces` 是套套邏輯** —— 兩邊用同一條公式，共同的 off-by-one 抓不到。（不過它在慣例探測中意外地也失敗了，所以目前有兩個獨立的鑑別器。）
- **沒有任何測試載入非 REDA 產生的 `.litematic`** —— 未驗證的包括：負的 `Size`、多 region 選擇、對 `TileEntities`/`Entities`/`PendingBlockTicks` 的容忍，以及反方向的「Litematica 會不會接受一個沒有 `TileEntities` 欄位的 Region」。**一份由 Litematica 產生的 fixture 就能一次補上大半。**

### 格式細節

- **多 region 檔案被靜默截斷成字典序第一個 region** —— 有註解，但沒有警告、沒有錯誤、呼叫端無從偵測。
- **`save` 可能寫出重複的 palette 項目** —— `block_state_to_entry` 是有損的（未建模的欄位不影響輸出），所以不同的 `BlockState` 可能塌縮成相同的 `PaletteEntry`，浪費 `bits_per_entry`。
- **`volume` 與 `count` 用 `as i32` 無防護** —— 超過約 21 億方塊會截斷。純 metadata 欄位。
- **`TimeCreated` / `TimeModified` 硬編為 0** —— Litematica 介面上會顯示一個無意義的日期。
- **`lit` → `powered` 的退回對 1.21 銅燈是錯的** —— 已在程式碼中標註。1.20 沒有這個方塊。

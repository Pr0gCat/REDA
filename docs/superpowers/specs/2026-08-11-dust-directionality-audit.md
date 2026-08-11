# Minecraft Java 26.2 紅石粉定向弱充能稽核

## 結論

在 REDA 已建模的方塊範圍內，現行 redstone-dust「連通形狀 → 水平弱充能」模型是完整的；沒有發現需要修正的行為差異，也因此**沒有新增測試或修改程式碼**。方向判定集中在 `redstone::simulator::connectivity::dust_sides` / `dust_powers_block_toward`，實際的弱充能輸出則由 `propagate::dust_power_toward` 提供給 `block_signal_at`；編譯器的定向 dust terminal 也呼叫同一個 geometry-only 判定。

本結論只涵蓋穩態、靜態幾何與 REDA 現有的方塊分類；不涵蓋 repeater 的動態轉態。後者不可由此 RCON 路徑驗證，見下方限制。

## Java 規則（本稽核採用）

令 `C(d)` 是該 dust 在方向 `d` 的已連接 side；它必須是原版計算完單邊補線後的 blockstate 形狀，而不是只看另一格是不是 dust。

| 形狀／方向 | 水平弱充能規則 |
|---|---|
| 孤立單格（dot） | 沒有 `C(d)`，不弱充能任何水平鄰塊。 |
| 單邊 stub | 原版把另一側補成同軸 line；因此仍沿該軸兩端弱充能。 |
| 直線（含 2 格以上各端點） | 每格沿自己的軸兩端弱充能；端點不因只有一個真實鄰 dust 而失去輸出。 |
| corner、T、cross | 只要有垂直於欲輸出軸的連接，該格沒有任何水平弱充能。 |
| 一般式 | 水平方向 `d` 有輸出，當且僅當 `C(opposite(d)) && !C(perp1) && !C(perp2)`。 |

這份規則與強度傳播分離：輸出強度是該 dust 的當前 `power`，而且是**弱**充能，不能經由被充能的方塊再餵給另一格 dust。垂直規則不看形狀：永遠弱充能腳下的方塊，永不充能上方的方塊。

一個 side 是「連接」就會參與上表，不必是同層 dust；合法爬階 side 也一樣。元件是否已通電不重要，重點是 wire blockstate 是否結構性地連到該元件：

- repeater 只在本身軸向連接；擺在垂直於 wire 軸的位置會取消該 wire 的定向輸出，轉 90 度、不連接時則不會。
- observer 只在輸出面連接；comparator 的四個水平面都可連接。
- REDA 已建模的一般訊號源（torch、lever、button、各種 pressure plate、redstone block、target、daylight detector）都作為可連接的 side。

這是「dust 指向元件」和「元件讀 dust」兩件事：repeater/comparator 直接讀相鄰 dust 的強度時，不以這個弱充能形狀規則為門檻；corner dust 仍可直接餵進面向它的 diode。不得把那條直接輸入規則誤當成 corner 能弱充能旁邊的方塊。

## REDA 對照

| Java 行為 | REDA 實作／證據 | 判定 |
|---|---|---|
| 形狀從真實 dust、爬升／下降與元件連接共同導出 | `src/redstone/simulator/connectivity.rs`: `dust_side_connected`、`dust_sides` | 符合 |
| 單邊補成 line、直線端點仍有軸向 | 同檔 `dust_sides` 的雙軸補線；單元測試 `a_one_sided_stub_is_completed_into_a_straight_run` | 符合 |
| dot 無水平輸出；corner/T 無水平輸出 | `dust_powers_block_toward` 的 opposite-side / no-perpendicular predicate；對應單元測試 | 符合 |
| 直線的軸兩端、且強度隨 wire `power` | `propagate::dust_power_toward` → `block_signal_at`；`a_dust_run_weakly_powers_the_block_it_points_into` | 符合 |
| 元件 side 能取消方向，但非連接方向不能 | `rules::taxonomy::accepts_dust_connection`；`an_aligned_repeater_connects_but_a_perpendicular_one_does_not` | 符合 |
| terminal 必須真的指向 support block | `src/compile/mod.rs`: `input_socket_feeds_support` 與 terminal 檢查都呼叫 `dust_powers_block_toward` | 符合 |

`power_emitted_toward` 對 dust 的水平面刻意回傳 inert，因為它只有 `BlockState`、沒有世界形狀；這不是漏實作。所有需要世界幾何的 block-power 路徑改用 `dust_power_toward`，而 compiler 的幾何檢查也直接使用 `dust_powers_block_toward`。

## 26.2 證據與限制

- 已存 [26.2 靜態 RCON 結果](../../../conformance/results/26.2-dust-directionality.json) 三組 probe 全數相符：直線／stub、corner、T、dot；爬階 side；以及未通電 repeater 的接／不接兩個方向。最後一組也確認「元件是否 powered」不是形狀判定條件。
- [probe 原始配置](../../../conformance/probes.py) 另驗證 support block 上的 torch 會讀到直線 terminal 的弱充能，分支後保持亮；因此涵蓋實際 gate terminal。
- 26.2 RCON 在本次稽核時未執行（`127.0.0.1:25575` 未監聽），沒有自行啟動 server。既有結果足以回答靜態幾何問題；本次沒有產生新的 conformance JSON。
- 不以 RCON 對 repeater 的 `powered` 狀態下結論。專案已記錄 `/setblock` 後 repeater 不會在 RCON 路徑轉態的限制；動態驗證必須由 26.2 client 實際操作。這份稽核沒有對 repeater 動態行為作任何 Minecraft 宣稱。
- 四向 cross 沒有留下可把 lamp 放在「自由水平面」的直接 probe；不過 T 已在保有觀測面的前提下證明只要一個垂直 side 就會取消定向，與上述 predicate 對 cross 的推論一致。

外部資料僅用來交叉檢查不涉及 Java-specific shape 的基本事實：官方的 [Minecraft Creator Guide](https://learn.microsoft.com/en-us/minecraft/creator/documents/redstoneguide?view=minecraft-bedrock-stable) 確認 wire 每延伸一格衰減 1、repeater 的單向輸出；它是 Bedrock Creator 文件，**不是**本報告 Java shape 規則的依據。Java 26.2 的決定性依據是上列同版本、靜態 RCON 實測。

## 邊界與尚未建模的範圍

本次沒有發現對 REDA 目前支持方塊的行為缺口。唯一需要明確保留的擴充邊界是 `accepts_dust_connection` 是目前 `BlockKind` 的白名單，而非「任一 Java signal-source block」的通用 predicate。未來若把現今歸為 `BlockKind::Other` 的 Java 訊號源（例如 lectern、tripwire hook、sculk sensor、trapped chest）納入模擬，必須同步擴充這個 predicate；否則一個本應接在 wire 垂直 side 的新元件會被誤判為不連接，錯誤保留 terminal 的方向性。這是未支援元件的範圍限制，不是現有編譯器 cell library 的已知錯誤。

## 驗證紀錄

嘗試執行：

```powershell
cargo test --manifest-path C:\Users\LTY\Desktop\REDA-task3-fix\Cargo.toml redstone::simulator::connectivity::tests --lib
```

測試尚未開始，因為現有工作樹的 `src/compile/primitive_graph.rs` 無法編譯：它引用已不存在的 `PrimitiveGraph.stateful_regions`、`StatefulPrimitiveRole` 與 `EdgeKind`。本稽核沒有修改該檔或任何測試；此失敗與 dust 方向性無關，且工作樹另有使用者未提交變更，均未納入本次提交。

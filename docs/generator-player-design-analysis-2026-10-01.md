# 玩家分享實作分析：解碼器與火把矩陣

依使用者指示，本輪改以玩家／紅石社群分享的實體 schematic 為主要研究對象。
已下載、解開 Sponge v3 NBT、逐層檢查實際方塊，並模擬可由現有模型完整讀取的兩份樣本。
原始附件留在 `/tmp`，本記錄保存来源、SHA256、重現工具與觀察結果；未再散布玩家世界檔。
正式生成器與模擬器沒有修改。既有 `rom_family_physical_probe.rs` 的未提交波形工作不屬於本輪。

## 實際研究的玩家分享

- [Logical Redstone Archive：DECODE003，水平到水平解碼器](https://mattbatwings.github.io/lra/archives/DECODE003-Decoder-horizontal-to-horizontal-2-versions/)。下載附件 `DECODE003_decode_h2h.schem`，分析其中三格間距的版本；網站列為社群推薦、mattbatwings 背書，不能因此把他記成已確認的設計作者。附件也包含另一個兩格間距版本，本轮沒有測試其動態輸入。
- [同一社群：ENCODE003，水平到水平編碼器](https://mattbatwings.github.io/lra/archives/ENCODE003-Encoder-horizontal-to-horizontal/)。下載附件 `ENCODE003_encode_h2h.schem`，就是上層共用字線、側面火把、下層收集位線的結構。
- [同一社群：DECODE002，垂直到水平解碼器](https://mattbatwings.github.io/lra/archives/DECODE002-Decoder-vertical-to-horizontal/)。已下載並查看全部方塊；它使用染色玻璃作垂直階梯。現有模型未識別 `minecraft:lime_stained_glass` 的支撐分類，原始結構的模擬明確拒絕，沒有換成石頭或普通玻璃來宣稱原樣通過。
- [mattbatwings 本人分享的 Control ROM 世界](https://www.planetminecraft.com/project/redstone-control-rom-from-let-s-make-a-redstone-computer/)。確認作者分享了系列第四集的控制 ROM、ALU 與暫存器檔整合世界；本輪未取得世界內容，沒有把页面介紹當成接線證據。

## 從實際座標提取生成規則

DECODE003 三格版本的輸出列位於 `x=23,26,29,32`，收集線在 `y=1`，
四條地址線在 `z=4,6,8,10`，通常位於 `y=3`。
由實際方塊推導的可參數化 stencil 如下，其中 `x` 是一列、`z` 是一個地址位：

| 期待地址值 | 控制與輸出結構 | 需要保留的幾何 |
| --- | --- | --- |
| 1 | `(x,2,z-1)` 的 north 火把讀取 `(x,2,z)` 支撐，上方地址灰塵控制它，下方收集線接收不匹配訊號 | 火把上方與鄰列不能新增可造成旁路的導體 |
| 0 | `(x+1,2,z)` 的 east 中繼器向 west 支撐 `(x,2,z)` 輸出，強充能再送到下方收集線 | 中繼器輸入從 east 讀取；上層地址線需跨過這個入口，不能讓輸出灰塵爬回輸入 |

零值取訊號點的地址灰塵經 `(x,4,z)`、`(x+1,4,z)` 跨越，
回到 `(x+2,3,z)`；中繼器從其下方輸入支撐讀取。
這是具体原始座標的分析，不是把普通灰塵直接接進一個支撐就假定等效。
整條不匹配線最後反相，輸出該地址樣式是否匹配。

ENCODE003 原始資料矩陣的字線是 `y=3, z=3,5,7,9`；
資料火把在 `y=2`、朝 north，位線在 `y=1, x=0,2,4,6`。
可生成的規則是由每列資料遮罩決定各交點是否放火把，字線和位線各自共用。
保持沒有下層位線到字線的額外導體，不能只憑火把單向性推斷整個世界已隔離。

## 原始樣本的實體觀察

三格解碼器的原始四列分別編程為地址 `5,10,5,10`；這是重複樣式的展示模板，
不是完整 4-to-16 one-hot 解碼器。編碼器原始模板在所有資料交點均放火把，
所以其四個輸出都等於四個高有效輸入的 OR，尚未測試任意編程資料矩陣。
以上預期函數由取訊號方塊推導後獨立計算，沒有用模擬輸出反推答案。

每個樣本先穩定，然後遍歷 16×16 組 from/to，各執行 setup 與 target，
總計 512 階段；不重置實體歷史、不插入 hold、不修改火把語義。
所有階段 driver 與 lamp 穩態皆符合預期。兩個樣本各自重跑，完整 JSON 位元一致。
沒有源 netlist／封裝物理證明，因此結果始終標為 `certified:false`。

下表只描述這兩個原始樣本，不是和 REDA stock 的同邊界品質比較。
各項按 256 個 target 階段統計，毛刺按每個公開標籤分開算；完整十二指標見 summary JSON。

| 指標，game ticks | DECODE003 三格版本 | ENCODE003 |
| --- | ---: | ---: |
| 最差穩定時間 | 32 | 19 |
| 總穩定時間 | 1222 | 1442 |
| driver 毛刺通道／轉換數 | 10 | 88 |
| lamp 毛刺通道／轉換數 | 10 | 4 |
| driver 總錯誤脈衝時間 | 72 | 224 |
| lamp 總錯誤脈衝時間 | 102 | 44 |
| driver 最長錯誤脈衝 | 24 | 14 |
| lamp 最長錯誤脈衝 | 27 | 11 |

解碼器 `12→1` 的 target：lamp0、lamp2 在 tick 5 錯亮，tick 32 才滅。
矩陣 `12→4`：四燈在 tick 8 錯滅、tick 19 恢復。
密集切換與火把歷史可能相關，但本輪沒有作火把事件的因果介入，不能把此推測寫成已確認根因。
社群頁面標示的「2 ticks」不等於這種保留歷史的最差邊界時間，也不能拿來替換實測上限。

## 本輪自行推導的混合矩陣：拒絕

在改以玩家原樣接線為主之前，建立了獨立的混合取訊號原型。
單一同相中繼器 tap 與兩火把同相 control 各通過四組 from/to（含 setup）；
target 穩定時間分別 `[0,3,6,0]` 與 `[0,5,8,0]`，並驗證單火把反相 tap。
最初手放原型的燈端方向與中繼器朝向錯誤，修正後才得到上述單點結果；不能以最初錯誤 control 宣稱改善。

接成四列、三位完整矩陣後，全 4096 字表 control 穩態均正確，candidate 僅 8 表正確，
全部為常數表；另外 4088 表錯誤。這是本輪原型失敗，沒有證據說玩家設計失敗。
正式生成器從未使用此候選，既有品質上限未改動。

字表 `[1,0,0,0]`、初始地址 0 的失敗見證：兩条原始地址線在 `(2,5,3)`、`(8,5,3)`
意外有 power 9，兩個正相取訊號中繼器因此亮起，導致字表 0 的輸出錯低。
現有連通性函數確認了下列灰塵旁路：

`(4,3,4) → (4,4,3) → (3,5,3) → (2,5,3)`

也就是下層收集線沿放置時留下的階梯直接爬回原始地址線，繞過中繼器。
單點測試沒有相鄰共用收集線，因而沒暴露這個生成幾何錯誤。
通解要求禁止下層收集線連通到上游入口；下一個原型應使用玩家的中繼器入口與跨越 stencil，
重新證明完整局部電氣隔離，不能只標記「有中繼器」或「不同 owner」。

## 重現與後續

`tools/inspect_player_schematic.py` 使用 Python 標準庫解 NBT，保留方塊名稱、所有 palette 屬性與座標，
檢查資料長度並記錄來源 SHA256。`player_schematic_probe` 用現有 litematic 方塊解析器載入，
保留未知名稱的分類規則，模型不支援的支撐方塊拒絕。

```sh
python3 tools/inspect_player_schematic.py /tmp/reda-player-decode003.schem /tmp/reda-player-encode003.schem
source tools/env.sh
RUST_MIN_STACK=33554432 cargo run --release --example player_schematic_probe -- /tmp/reda-player-decode003.schem.json decode003
RUST_MIN_STACK=33554432 cargo run --release --example player_schematic_probe -- /tmp/reda-player-encode003.schem.json encode003
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --matrix
RUST_MIN_STACK=33554432 cargo run --release --example mixed_decoder_tap_probe -- --matrix-witness
```

完整拒絕掃描會 exit 1；`--matrix-witness` 僅輸出因果定位資料，其 exit 0 不是驗收通過。
已凍結原始樣本 driver/lamp 波形、十二指標、完整混合矩陣對照掃描及階梯旁路見證。
沒有以方塊數或體積評分，也沒有為了玩家標稱速度調整模擬器。

下一輪：由玩家三格入口 stencil 生成任意字表候選，保留本輪固定 control，
先排除地址回灌，再逐字表驗證穩態、十二項波形指標、決定性與來源邊界等價。
只有完整物理契約與固定同邊界品質對照通過，才可接進正式生成器。

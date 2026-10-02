# 預設編譯入口的逐項品質選擇

`compile()` 現在對兩個輸入、三個公開輸出的組合電路，自動比較 player matrix 與原始 planner/channel 候選。其他邊界及 DFF 繼續原有路徑。方法依輸入／輸出結構及實測品質選擇，不依函數編號、來源名稱、方塊數或體積。

## 問題與原因

上輪固定合格 player 對照仍有部分函數可由原始編譯器降低延遲，但直接使用舊對照會接受錯誤電路或合併公開輸出。舊診斷中 33 個較快候選有 6 個合併了別名輸出的燈，不能滿足三個獨立公開輸出的契約。另有 Unified3d 候選已在建立 observations 時產生完整 manifest，卻未保留，因而無法事後沿相同嚴格流程認證。

第一版保留 manifest 後僅選出 9 個備選。原因是認證重新生成最小 bounds，刪除了原始世界外圍的空氣 padding，違反完整世界身分檢查。現在沿既有結構、ownership、bounds、電氣認證流程保留原始 emission envelope；每個 BlockState 與世界尺寸必須完全相等。過小 bounds 仍拒絕。沒有更動 verifier 或 simulator 規則。尺寸相等只是身分契約，不是品質標準。

## 通解與契約

兩個候選都必須通過來源等價、完整公開邊界和嚴格物理認證。每個候選從初始世界連續執行 16 組 setup/target，共 32 階段；每階段檢查全部實作訊號及公開 driver/lamp 真值。只清除 observer 記錄，不清除 simulator 歷史，不添加等待階段。

setup 和 target 分別比較 12 項：最差／總穩定延遲，以及 driver、lamp 各自的毛刺、單輸入毛刺、最後變化時間、總錯值脈衝、最長錯值脈衝。備選須在全部 24 項不劣且至少一項更好才取代 player；相同或互有取捨時保留 player。任一候選未合格就不參選；兩者都失敗則返回錯誤。

原始 `compile_without_rewrite()` 保留供候選與獨立控制使用。消費端以 sealed implementation netlist 和 source boundary bindings 解讀改寫，不用原始 gate 名稱猜實體位置。原始 planner manifest 保留在私有欄位中。獨立舊 probe 明確呼叫原始入口，避免預設切換污染控制。

## 完整實驗

實際預設入口經 Native frontend 生成全部 4096 個兩輸入／三輸出函數。固定品質控制為上輪 `generator-independent-negative-literal-experiment-2026-10-02-family.jsonl.gz` 的合格 candidate，原始控制資料逐字保留，沒有重新呼叫新預設來製造控制。

- 4096 個來源等價、嚴格認證、每階段全部內部真值及公開輸出通過。
- 選擇 4069 個 player、27 個合格備選。
- 對固定合格對照：27 個改善、4069 個持平、0 個退步。
- 2744 個不可變既有上限全部通過。
- 全部 setup 指標不退步，setup driver/lamp 毛刺為 0。
- 全部 4096 個 Rust target/setup 指標由獨立 Python 依保留的完整波形重新計算一致。
- 例如 `[1,0,0,2]` target 總穩定時間 96 → 86 ticks；`[4,1,0,3]` 108 → 92 ticks。不是僅比較加總：各別指標均未退步。

完整 family、摘要在同名 `-family.jsonl.gz`、`-summary.json`。family 保存每階段公開波形、選擇、全部品質指標及完整 BlockState/世界 bounds 指紋；來源等價、實作訊號真值及 partition 由 producer 強制檢查。

## 重現與限制

```sh
source tools/env.sh
export RUST_MIN_STACK=134217728
cargo run --release --example quality_selection_probe -- docs/generator-independent-negative-literal-experiment-2026-10-02-family.jsonl.gz > /tmp/default-family.jsonl
python3 tools/compare_shipping_matrix.py docs/generator-independent-negative-literal-experiment-2026-10-02-family.jsonl.gz /tmp/default-family.jsonl --candidate-control > /tmp/default-quality.json
```

這是 simulator 與嚴格結構／電氣認證結果，沒有宣稱已在 Minecraft 客戶端驗證。只對上述完整兩輸入／三輸出家族保證逐項品質；更大 ROM／Mux 不在本次自動選擇邊界內。編譯期間額外生成、認證並模擬兩個候選，會增加生成時間；選擇不依牆鐘時間，也不宣稱生成速度改善。

## 回歸與目標核對

本輪回歸：selector 3、player 17、fragment API 14、sealed physical views 6、equivalence 9、world partition 5、evaluation 4、timing 22、viewer 24、dump 2、Python conformance 7，全數通過。Native／fresh Yosys player source fixture suite 通過（89 個來源 fixture）；Native／Yosys DFF power-on、edge/hold differential，以及禁止 DFF legacy fallback 的各 1 項回歸均通過。沒有宣稱整個歷史測試套件全綠；既有與本輪無關的品質失敗未降低門檻。

本次目標核對：先前提交已完成共享矩陣條件／極性、隔離扇出、供電恢復、匹配訊號到達時間，以及 direct product／反相級消除的通解。sealed consumer views 在 `99a6f51`，dump/conformance 在 `7761ca0`。本輪補上最後的預設自動選擇：來源／實作契約、獨立控制、所有連續 setup/target 指標、嚴格認證、內部真值、既有上限、DFF 與消費端回歸均有證據。更大邊界及遊戲客戶端驗證是已明示的適用限制，沒有由此次完整 4096 家族結果外推。

三次實際預設完整生成各 4096 筆，全部 family bytes 相同，SHA-256：`e91fb1bb40a166035c58d2202cbf726d400d873e24a974c789eee8cad0a68b20`。指紋覆蓋 world bounds 與完整 BlockState；完整紀錄亦包含所有階段波形，沒有只比較最後真值。

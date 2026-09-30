# MUX 共同高項實驗

基準 4846520。現行 MUX 3 → 7（a = b = 1，只切換 s）輸出应保持高，實際驅動在 tick 4 關閉、tick 6 恢復。此是單一選擇輸入靜態缺口，與前面多輸入 functional hazard 不同。

原型將所有已完成常數／等臂折疊的原生 MUX 表達成三項 OR：(!s & a) | (s & b) | (a & b)。共同高項与 s 無關，布林真值仍為原 MUX。使用既有 table_and／table_or／table_not 建立 primitive，沒有辨識 top、訊號名或案例；hold mux 也套用相同規則，未刻意避開 DFF。原型沒有整合原有 LogicNode::Mux 及其來源結構介面，不構成正式可採用 lowering。

目標 3 → 7 確實沒有燈或驅動跳變，但完整原生 MUX 燈毛刺 4 → 6，固定矩陣門檻拒絕。DFF 實體測試更早在編譯時被原有 `SignalStrengthViolation { net: g1, sink: GateInput { gate: g6, support: (24,1,41) } }` 拒絕；沒有略過強度檢查、調低門檻或對 DFF 排除規則。

第一個合併測試命令在 DFF 失敗後停止，所以候選主品質與 systemverilog_dff 沒有跑完；另單獨執行矩陣定位 MUX 回歸。不能宣稱候選全面驗證或 DFF 可用。矩陣在原生 MUX 拒絕後，後續 Yosys MUX、majority 尚未執行。

相鄰 JSON 保存原型、獨立基準、完整 MUX 轉換軌跡、目標見證與拒絕原因。布林共同項能消除特定靜態缺口，但由 graph 層直接展開會改变其他路由與 hold mux 的實體結構，整體不合格。原型全部撤回，保留 4846520 生成器；沒有新增生成器優化。

下一個方向可在保留 MUX 語意與來源介面的 lowering 階段研究共同項或等價的穩定路徑，同時先定位 DFF 新分支的強度不足，而非只看修好的目標轉換。

還原後主品質、形狀矩陣、五個毛刺見證及 12 個 DFF 測試全部通過。矩陣世界指紋、全部信號指標、最差與平均時序逐欄等於已採用的 4846520 checkpoint。此提交僅保存失敗實驗證據。

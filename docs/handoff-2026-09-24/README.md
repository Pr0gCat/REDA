# 交接證據包

先讀 `../recursive-synthesis-claude-handoff-2026-09-24.md`。

`evidence/` 是實際測試／診斷輸出與 g19 固定路徑 replay；`pre-input-fanout/`、`pre-guidance-height/` 是比較用快照，不是 live implementation，不能直接覆蓋現在的 source。

本包在2026-09-24由 `/tmp` 已存在的證據複製保存，避免重開機遺失。原始工作樹含大量其他未提交工作；這些快照不是整個 repo 的乾淨基底。

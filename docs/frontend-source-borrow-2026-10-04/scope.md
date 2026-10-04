# Borrow the native frontend source slice

The native-first selector now uses std::slice::from_ref for its single source, both during compilation and when rendering diagnostics. SourceInput derives Copy and holds only immutable name/text strings; copying it and borrowing it provide the same source values. No fallback predicate, invocation, public API, source map, name, span or diagnostic content changes.

Current-worktree offline native library cargo check passes in8.16 seconds. Fresh strict library clippy reports17 remaining findings, removing the four clone/single-reference-slice findings from selection.rs, with no findings in that file. The full lint gate still exits101; its complete diagnostics are retained. This is a lexical borrowing change; no new tests or redundant runtime runs are introduced, and no fresh runtime qualification is claimed. It does not complete native HDL, optional Yosys or whole-project requirements.

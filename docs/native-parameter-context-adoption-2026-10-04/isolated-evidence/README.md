# Native typed parameter initializer context

Implementation baseline: `9903b8c06a25dbb2158a61fac5863eed04865220`, exported directly from Git into this isolated tree. Shared working source was not copied or modified.

The original typed eight-bit initializer `4'hf + 4'h1` produced zero because four-bit arithmetic truncated before declaration conversion. The fix determines the complete expression type first and carries the declaration width through arithmetic before evaluating operations. Named parameters keep their own declaration context and cached values.

Instance overrides remain self-determined in the parent scope. A narrow override expression therefore remains zero when assigned to an eight-bit child formal. Overriding only the child width parameter changes the default initializer context and yields sixteen. Independent Yosys observations establish this boundary; there is no deferred override AST evaluation in child context.

Independent Yosys evidence: frozen SystemVerilog sources, output JSON, and final logs in this directory. Run from this tree with relative paths because YoWASP mounts `/tmp` as private scratch. Interpreter: `/home/seith/REDA/.venv/bin/python`; launcher: `/home/seith/REDA/.venv/bin/yowasp-yosys`; cache: `/tmp/reda-parameter-yowasp-cache`.

Native qualification: `native-regression-final.log` contains the five selected integration binaries (20 tests). Existing typed, hierarchy, physical switching, MIN and signed-32 negative controls are preserved. `native-build-manifest.json` binds source and test binaries before this logged qualification run. `source-manifest.json` records the clean baseline file, proposed source and focused tests.

Patch: `parameter-context.patch` modifies only `src/frontend/elaborate.rs` and adds `tests/systemverilog_parameter_context.rs`. No cap increases, simulator changes, ignored/deleted tests, golden updates or new support rejection were introduced.

Two additional independent wide MIN controls both produce `0xffffffff80000000` in Yosys, with either `64'h0` or `64'sh0`. The original MIN special case is retained. `minimum-extension-regression.log` records an exploratory zero-extension hypothesis; it is superseded by independent Yosys evidence and `minimum-extension-final.log`.

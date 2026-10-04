# Public inout loss in the Yosys bridge

The original source is `inout_original.sv`. Independent Yosys output retains all three public ports: input a, inout bus, output y; all alias net 2. Before repair, both forced Yosys and Auto return success but omit bus from the artifact. `forced-auto-baseline.log` and `baseline-output-artifact.json` preserve the exact public result. `inout_original-yosys.json` and its Yosys log preserve the standard synthesis output.

The minimal bridge patch validates each public port direction before extracting input/output bindings. Directions other than input/output fail explicitly by port name and direction. A negative test reproduces the original alias JSON and also checks unknown direction. Existing eight bridge unit controls remain unchanged; all nine pass.

Both real Yosys paths now report bus/inout unsupported instead of silently dropping the port. This is a responsible transport fix, not implementation or completion of top-level inout support. The observer process exits successfully after printing its expected errors; the artifact-level errors are recorded in `forced-auto-frozen-fixed.log`.

Isolated source baseline is recorded in baseline.txt. Root-owned independent synth.py path repair was copied precisely, with its hash in root-driver-sha256.txt, to permit actual sandboxed YoWASP runs. No shared source was modified. `bridge-public-port.patch` contains only yosys_json.rs. The owned observer example is evidence tooling and is not part of the proposed production patch.

Frozen manifest binds proposed source, driver, observer and test binaries before final verification. qualification.json records all nine tests, two actual Yosys calls and zero source/binary drift. The separate read-only inout semantic audit is `/tmp/reda-inout-readonly-audit-20261004.md`.

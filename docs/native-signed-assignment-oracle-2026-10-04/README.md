# Native signed assignment standard-oracle controls

Clean committed source c1bc590 matches Icarus Verilog12.0 for ten signed/unsigned assignment contexts across all256 input rows:20,480 public output bits. Cases cover signed widening, complement, same-type and mixed-type bitwise operations, conditional branch widening, self-determined concatenation, equality and logical negation. No source changes are needed for these cases. This does not cover general arithmetic, procedural contexts, physical generation or game behavior, and does not close the native HDL gap.

The source/oracle manifest binds127 files; a post-run check detects no drift. Native execution exits0 with the retained terminal output. The test lives in an isolated clean export and preserves shared user changes. Compiler stack configuration follows the already established2GiB build recipe, without changing circuit budgets or physics.

To reproduce, export c1bc590 into an isolated tree, copy fixture.sv and oracle.csv to evidence/, and signed_assignment_oracle.rs to examples/. Run `cargo run --offline --example signed_assignment_oracle`. Regenerate the standard oracle with Icarus `-g2012 -s tb` using fixture.sv and oracle_tb.sv, then run the generated executable with vvp. The official tool/package verification is retained in ../inout-standard-oracle-2026-10-04/.

# Logical operand self-determination

The unchanged10,000-expression independent fuzz oracle finds case73 returns true instead of false. Binary elaboration applied a common width before selecting its operator. For example2'b01 && ~1'b1 incorrectly widened the inversion to2 bits and produced a nonzero high bit before truth reduction.

Logical AND/OR now retain each operand's own width and then use the existing truth reduction. Bitwise/arithmetic/comparison context handling is unchanged. Four focused sources verify false/true AND/OR and all8 output bits; the original case73 remains in the complete unchanged fuzz suite.

All four fuzz tests pass:10,000 expressions with exhaustive rows and deterministic netlist/debug comparisons,10,000 arbitrary UTF8 inputs, spread-of-shapes and hand-checked oracle controls. The36 context/compiler controls also pass, including512 physical phases. No simulator, physical limits, original source seeds or tests change. Original full-run panic details were hidden by the concurrent UTF8 test's process-global panic hook; isolated rerun exposes the retained counterexample.

Owned patch excludes all unstaged user arithmetic. Full-project gate and parameter-expression destination contexts remain incomplete.

# Actual implementation dump and conformance contract

Reference: ece6f1d. Automatic default selection remains disabled.

mc_dump previously printed actual rewritten gate output positions alongside
source netlist gate definitions. Those gates have different identities and
can use different primitives. The Python conformance reader also accepted
only NOR/merge and could neither place nor read comparator stages.

GATE records now describe the implementation graph in checked topological
order. Actual internal names use CompiledCircuit's collision-free labels;
primary inputs resolve through proved source-ordered bindings. GATEOUT adds
an actual component kind for rewrites. INPUT/OUTPUT retain public source
names, while OUTPUTPORT preserves every source declaration's ordinal,
implemented signal, separate physical lamp and per-occurrence public label.
Repeated source names consume matching cosmetic labels separately, preserving
each public alias label. Source aliases are not fake
gates. BLOCKPROP preserves comparator mode. Legacy record spellings and
ordering remain unchanged apart from explicit retained block properties.
The dump validates its sealed source/world/observation view before interpreting
rewrite metadata.

The reader evaluates actual Buf and AndNot stages and applies output port
bindings afterwards. Ordered output keys retain alias multiplicity. It places
subtract comparators before dust, reads comparator/repeater powered state,
reads actual floor/wall torch lit state and reads wired merge dust power.
A rewrite requires matching gate definitions, observations and actual block
kinds, unique topological drivers and contiguous valid output ports. Missing
or mismatched records are rejected rather than treated as source gates.

Validation commands:

```
source tools/env.sh
REDA_DUMP_FIXTURE_DIR=/tmp/reda-dump-fixtures RUST_MIN_STACK=134217728 cargo test --release --bin mc_dump
REDA_DUMP_FIXTURE_DIR=/tmp/reda-dump-fixtures python3 -m unittest discover -s conformance -p 'test_*.py'
```

Two Rust tests generate six actual certified rewrite dumps. Seven Python
tests pass, including all four input vectors per real fixture, constants,
positive and negative aliases, comparator AndNot, Buf repeaters, merge dust,
colliding source names, removed source cells and deliberately permuted input/
output bindings. Invalid arity, port ordinal, source binding, property and
observation metadata are rejected. Typed component reads are tested with
mocked RCON replies; no real-server outcome is asserted. All six complete
fixture dumps regenerate byte-identically. An actual ordinary and4 dump
parses and evaluates correctly for all sixteen input vectors and produces
placement commands without rewritten bindings.

This changes serialization and consumer interpretation, not circuit generation,
physical certification, simulation rules, test holds or histories. The ece6f1d
4096-function quality evidence remains the circuit control. Next requirements:
qualified default selection and fallback with stock internal truth, unchanged
compile error/DFF contracts, actual default full family/repeat, relevant
public/constant/consumer/timing/DFF regressions, and final completion audit.

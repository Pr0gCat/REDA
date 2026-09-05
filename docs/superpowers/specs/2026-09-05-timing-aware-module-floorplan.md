# Timing-aware module floorplan

## Goal

Keep module compilation and whole-world certification unchanged while making
hierarchical placement spend its budget on module connections rather than on
flat gate identities the parent cannot move.  The concrete gate is the
programmatic hierarchical `ripple_adder8`: at most 474 observed settle ticks
and 100,615 non-air blocks.

## Measured baseline

The old 608-versus-474 comparison was not same-netlist: the programmatic
hierarchy contains 200 lowered gates because each module output is exposed by
two extra NOR buffers, while the flat reference contains 168 gates.

Fresh same-netlist budget-zero measurements on `addfe03` are:

| design | flat | hierarchical |
|---|---:|---:|
| programmatic, 200 gates | 620 ticks / 132,561 blocks / static 672 | 608 / 70,603 / static 678 |
| Verilog, 120 lowered gates | 358 / 69,757 / static 442 | 366 / 48,467 / static 522 |

Hierarchy therefore already wins size on both cases and is within 12 ticks of
the same-netlist flat result.  The 474 target remains an absolute optimisation
target, not evidence of a 134-tick hierarchy regression.

The budget-zero programmatic layout places the eight `full_adder` blocks in
successive X levels, but each carry output is 36 cells above its own carry
input and successive block origins drift downward.  The seven `cout -> cin`
connections are consequently offset by 36--52 Z cells instead of being
straight.

## Design

### Candidate family

Flat synthesis remains byte-for-byte unchanged.  Hierarchical synthesis gets
one private block-placement proposal stream.  It reuses
`run_budgeted_proposals`, `SynthesisBudget`, `QualityKey`, the existing router,
and complete certification.

The stream enumerates parent block-to-block edges in stable order:

1. lower structural slack first;
2. source `InstanceId`;
3. sink `InstanceId`;
4. sink input index.

Each edge is represented explicitly as
`BlockEdge { source_block, source_port, sink_block, sink_input, slack }`.
`source_port` comes only from the assignment driver's single
`PrimitiveId.node`, after checking that its `logical_owner` and primitive
instance both equal `source_block`. Both endpoint IDs must resolve through
`InstanceGraph::block`; flattened gate indices and instance names are never
used for this mapping.

For one proposal, move the sink block laterally so that the selected source
output port and sink input port have the same Z coordinate.  The displacement
is computed from the compiled blocks' existing port tables and the incumbent's
accepted block offsets.  X placement and fixed east-facing orientation do not
change.  Proposals are cumulative only when their fully certified candidate
strictly improves `QualityKey`.

The budget runner's candidate is a private hierarchical wrapper containing
the flat `CertifiedCandidate`, the relative block-placement overrides used to
compile it, and the realised block offsets returned by parent planning. The
wrapper implements the existing `SearchCandidate` trait by forwarding quality
and fingerprint to the certified candidate. Because `run_budgeted_proposals`
replaces the entire wrapper only on strict improvement, a refused or
non-improving move cannot leak into the next proposal.

### Smallest representation

Do not add a generic optimiser or a new placement framework.  Keep ordinary
gate placement overrides unchanged and add a separate block-offset map to the
hierarchical compiler.  Separation is required because flattened gate IDs can
numerically collide with parent block IDs.  `place_blocks` applies the offset
before claiming occupancy and registering source/target geometry, so all
existing collision, reservation, routing, verification, equivalence and
simulation checks see the moved block normally.

### Budget semantics

`Evaluations(n)` consumes exactly the first `n` completed proposals, giving a
deterministic prefix-compatible quality staircase.  `Time(d)` uses the same
ordered stream and checks the deadline only between complete proposals.  It is
deterministic for any completed prefix, but wall-clock variation may change how
many proposals fit in the duration.

### Electrical boundary

Input and output boundary repeaters remain unchanged.  They currently provide
signal-strength refresh and diode isolation, and the block port contract does
not certify enough information to remove them generally.  Repeater sharing is
only the next experiment if the complete block-alignment stream exhausts
without meeting the latency target.

## Verification

- A small unit test proves block offsets move exactly the selected block and
  its ports while preserving east-facing orientation and deterministic
  fingerprints.
- Proposal-stream tests prove stable ordering, exact evaluation prefixes,
  monotone returned quality, and zero flat-gate/duplicate proposals.
- Release measurement runs programmatic hierarchical `ripple_adder8` at
  budgets 0, 1, 2, 4 and stream exhaustion, recording observed ticks, blocks,
  static delay and fingerprints.
- The target harness has a real oracle: success requires one certified best
  candidate to satisfy both `observed_settle <= 474` and
  `non_air_blocks <= 100_615`. If it does not, the run must end with
  `ProposalStreamExhausted` and report a bounded negative result; printing
  metrics alone is never a pass.
- The winning candidate must pass unchanged physical verification,
  equivalence proof and simulator certification.
- Existing flat large/extra suites, pinned IO, channel safety, architecture,
  reference and terminal-handover suites remain green.

If the finite alignment stream exhausts above 474 ticks, report that bounded
negative result without weakening the target.  The next smallest design is
single-consumer block-output-to-block-input repeater sharing, retaining the
source output repeater and falling back unless geometry, direction, ownership
and strength constraints all hold.

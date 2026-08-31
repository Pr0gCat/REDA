# Timing-Directed Fragment Synthesis

**Status:** Proposed for implementation

**Date:** 2026-08-31

**Target branch:** `codex/timing-directed-fragment-synthesis-v2`

## 1. Intent

Replace REDA's production placement-and-routing generator with a new, deterministic
generator whose first objective is low observed redstone latency and whose second
objective is a small physical circuit.

The generator is not a repair loop around the current algorithm. It owns its own
seed construction, search state, and optimisation schedule. It reuses REDA's
trusted cell-topology library, routing primitives, world realiser, physical
verifier, and simulator because those components define what a valid Minecraft
circuit is; it does not inherit the current generator's placement policy or
fallback chain. A cell topology is consumed as a signal-flow graph of primitive
nodes and internal edges, not collapsed into one opaque gate-cost number.

The old generator may remain temporarily as a differential oracle during the
migration. The shipping path must not call it once the replacement gate in
section 13 is met.

## 2. Why this design

A move-only optimiser cannot reliably reduce delay because the useful change is
often larger than moving one gate. For example, a high-fanout signal may need its
driver duplicated, two consumers reassigned, the copies faced differently, and
all affected wires rerouted. None of those individual changes is necessarily a
valid or better intermediate circuit.

The unit of search is therefore a **fragment transaction**: a bounded logical and
physical subgraph is replaced atomically, realised, verified, simulated, and
either committed as one change or discarded without changing the current best.

This preserves a simple invariant:

> Every candidate visible outside a transaction is a complete, physically
> verified circuit, and the reported best is always one of those candidates.

## 3. Goals and non-goals

### Goals

1. Minimise worst-case observed settle latency across input transitions.
2. Among equal-latency circuits, minimise non-air block count and then occupied
   bounding volume.
3. Accept an evaluation budget for deterministic tests and a time budget for
   interactive use.
4. Return a certified baseline even when no optimisation succeeds.
5. Support absolute pinned inputs and outputs without moving or occupying the
   caller-owned cells.
6. Permit multiple physical instances of one combinational logical gate when
   duplication reduces fanout routing or latency.
7. Make a larger budget extend the exact proposal sequence of a smaller budget.

### Non-goals

- No fixed routing corridors, regions, timing islands, or routing-macro library.
- No new cell topology, logical optimisation rule, or physical truth rule as part
  of this project. The existing implementation library remains the authority;
  this project may add validation and query APIs needed to instantiate and time
  its existing structure, but does not replace that structure with generator
  policy.
- No stateful synthesis in this replacement milestone. A netlist containing a
  stateful gate returns `UnsupportedStatefulTopology` before `InstanceGraph`
  construction; clock, initial-state, and sequential-equivalence semantics require
  a separate design before support is enabled.
- No probabilistic acceptance, wall-clock assertions, or unverified preview
  candidate presented as a result.
- No promise that a numerically low packing ratio is good. Empty padding can make
  that number look better while making the circuit larger and slower.

## 4. Pipeline boundary

The new production lifecycle is:

```text
source Netlist
  -> existing assignment-aware lowering
  -> InstanceGraph
  -> deterministic cell-topology instantiation
  -> SparseSeedBuilder
  -> certified PlanCandidate
  -> TimingDirectedSearch
  -> best certified PlanCandidate
  -> existing realise_and_verify
  -> CompiledCircuit
```

`realise_and_verify` remains the authority for whether a candidate can exist in
Minecraft. The simulator remains the authority for functional correctness and
observed settle latency. The new generator must not add a second world emitter or
a weaker verifier.

Topology instantiation is a pure boundary between logical and physical identity.
It expands each selected `ImplementationKey` into primitive instances, topology
input landings, output semantics, and internal routing obligations. It
assigns no coordinates or facings. `SparseSeedBuilder` and fragment transactions
embed that expanded graph; they never infer a cell's primitive connectivity from
the current placer.

Lowering remains owned by the caller, as it is today. The generator accepts the
exact lowered netlist whose gate identities appear in timing, viewer annotations,
and the returned circuit. A front door that starts from a source netlist must
lower first and carry source provenance alongside the lowered design; the
generator never silently lowers its `Netlist` argument.

The initial public entry point is conceptually:

```rust
pub fn compile_fragment_synth(
    input: SynthesisInput<'_>,
    budget: SynthesisBudget,
) -> Result<FragmentSynthesisResult, CompileError>;

struct SynthesisInput<'a> {
    lowered: &'a Netlist,
    source_provenance: Option<&'a [usize]>,
    pins: Option<&'a PortPlacements>,
}
```

The exact ownership and error types may follow the surrounding compile API, but
the returned result must contain the compiled circuit, metrics, budget use, and a
reason the search stopped.

## 5. InstanceGraph

The current code often treats a logical gate index as a physical object index.
That prevents safe duplication and makes driver selection implicit. The new
generator introduces explicit physical identity before changing search policy.

```rust
struct InstanceId(u32);

struct Instance {
    id: InstanceId,
    logical_gate: GateIndex,
    role: InstanceRole,
    implementation: ImplementationKey,
}

enum ImplementationKey {
    Library(LibraryEntryId),
    Merge { isolation_mask: InputMask },
}

struct TopologyNodeId(u16);

struct PrimitiveId {
    instance: InstanceId,
    node: TopologyNodeId,
}

enum ConnectionId {
    External {
        instance: InstanceId,
        input_index: u16,
    },
    Internal {
        instance: InstanceId,
        edge_index: u16,
    },
}

enum PhysicalEndpointId {
    PrimaryInput(PortId),
    DeclaredOutput(PortId),
    PrimitiveOutput(PrimitiveId),
    Landing(ConnectionId),
    Junction(InstanceId),
}

enum InstanceRole {
    Canonical,
    Duplicate { ordinal: u16 },
}

struct InstanceInput {
    instance: InstanceId,
    input_index: u16,
}

enum InstanceDriver {
    Primitive {
        logical_owner: InstanceId,
        terminals: NonEmpty<PrimitiveOutputRef>,
    },
    Junction {
        logical_owner: InstanceId,
        contributors: NonEmpty<JunctionContributorRef>,
        observation: JunctionObservationRef,
    },
}

enum PhysicalDriver {
    PrimaryInput(PortId),
    Instance(InstanceDriver),
}

enum PhysicalSink {
    InstanceInput(InstanceInput),
    DeclaredOutput(PortId),
}

struct SinkAssignment {
    sink: PhysicalSink,
    driver: PhysicalDriver,
}
```

### 5.1 Topology instantiation

`Instance` selects a technique; it is not itself one placeable primitive. The
cell library exposes one pure operation conceptually equivalent to:

```rust
fn instantiate(
    gate: &Gate,
    instance: InstanceId,
    implementation: &ImplementationKey,
) -> Result<ExpandedInstance, TopologyError>;

struct ExpandedInstance {
    instance: InstanceId,
    implementation: ImplementationKey,
    topology: ValidatedTopology,
}

struct ValidatedTopology {
    fingerprint: Fingerprint,
    primitives: Vec<PrimitiveSpec>,
    connections: Vec<ConnectionSpec>,
    output: OutputSpec,
}

enum ContributorSpec {
    Landing(ConnectionId),
    Primitive(PrimitiveId),
}

enum OutputSpec {
    Primitive(PrimitiveId),
    Junction {
        logical_owner: InstanceId,
        contributors: NonEmpty<ContributorSpec>,
    },
}

struct PhysicalState {
    placements: BTreeMap<PrimitiveId, PrimitivePlacement>,
    connections: BTreeMap<ConnectionId, RealisedConnection>,
    junctions: BTreeMap<InstanceId, RealisedJunction>,
}
```

`instantiate` returns an `ExpandedInstance` whose `ValidatedTopology` assigns
dense, deterministic `TopologyNodeId`s, retains `TemplateNode` as semantic
metadata, and contains primitive specs, external and internal connection specs,
output semantics, and a topology fingerprint. It validates before any placement
that node IDs are unique and that every input, connection, output, and timing arc
resolves to an existing node.

For an ordinary combinational library entry, every `(TemplateNode, Primitive)`
becomes one `PrimitiveId`; every declared topology input creates one external
`ConnectionId`; every ordered `Template::internal_edges` entry creates one
internal `ConnectionId`; and `Template::output = Some(role)` maps the instance
output to that primitive. `Template::output = None` creates no fictional
primitive and instead requires explicit junction output semantics.

`ImplementationKey::Merge` is the library-owned parameterised form of the merge
rule the current compiler already uses. Its `isolation_mask` says which logical
input branches instantiate an isolating repeater. The all-clear and all-set masks
correspond to today's bare and isolated entries; mixed masks represent the
existing per-branch expansion exactly. A clear bit is legal only when that input
may share the junction without stealing another consumer's signal. The mask and
expanded-topology fingerprint participate in candidate identity and proposal
ordering.

The expanded view is derived from `Instance` plus the immutable library and is
not a second mutable topology database. A candidate stores physical placement,
facing, route, emitted-terminal, and observation state keyed by `PrimitiveId` and
`ConnectionId`. An implementation change discards and re-expands the affected
instance inside private transaction state; a full candidate rebuild prevents
stale nodes, landings, routes, observations, or timing arcs from surviving.

`Template::embedding_hints` may order otherwise legal embeddings. It is a soft
search hint only: it carries no absolute position, creates no rigid edge, and
cannot make an electrically invalid embedding valid. This is deliberately not a
routing-macro system.

The required invariants are:

- every logical combinational gate has at least one physical instance;
- every instance input and declared output has exactly one explicit driver
  assignment, including primary inputs as physical drivers;
- every assigned driver implements the same logical signal as the sink expects;
- a duplicate may only read the same logical inputs as its canonical gate;
- every selected implementation key is instantiated exactly: no validated
  primitive node or internal edge may be omitted, added, or inferred from a
  placer-specific shape;
- every primitive placement, facing, physical variant, primitive timing arc, and
  primitive verification record is owned by a `PrimitiveId`; `InstanceId`
  remains the atomic logical-copy and fragment-ownership boundary;
- candidate primitive and connection sets are exactly isomorphic to their
  `ValidatedTopology`; every external input has one explicit connection contract
  and every internal edge has one sink landing and one realised route record;
- every topology internal edge is a real routing obligation included in
  collision, strength, direction, timing, route-tree closure, and verification;
- logical outputs and viewer annotations can map one logical gate to many
  instances while retaining a deterministic canonical instance for compatibility;
- every electrical source, landing, junction, and declared boundary uses a
  `PhysicalEndpointId`; primitive state additionally uses `PrimitiveId`, while
  logical ownership is carried separately as `Option<InstanceId>` and is never
  inferred from endpoint kind or a position in a logical-gate vector;
- the verifier checks each duplicate's own input ports, cell implementation,
  output terminals, and assigned routes independently;
- a canonical position, facing, or output is a compatibility view only and is
  never used to verify or time all instances of a logical gate.

`InstanceDriver` separates logical ownership from electrical origin. An ordinary
driver's terminals are derived from the selected topology output, not from a
canonical gate position. A junction driver is logically owned by the merge
instance, but its contributors may be upstream primitive or primary-input route
endpoints that the merge does not own. Its verified observation point proves
where those contributors form one physical net. Route-tree closure follows
electrical contributors while fragment and annotation ownership follow
`logical_owner`; neither is inferred from the other. The first duplication
milestone supports only instances whose selected topology has one concrete output
primitive. Merge instances retain one canonical junction until a separate,
verified junction-duplication model exists.

Migration begins with a one-to-one `InstanceGraph`. Duplication is enabled only
after that representation re-emits existing one-to-one candidates byte-for-byte.

## 6. Deterministic sparse seed

If seed construction succeeds, optimisation budget zero returns that complete
certified seed. The new `SparseSeedBuilder` constructs it without calling the
legacy generator. `SeedExhausted` remains a named construction error rather than
an implicit fallback.

### 6.1 Ordering

The builder computes the combinational dependency DAG after lowering and assigns:

- forward arrival level;
- reverse required level;
- fanout count;
- stable logical identity used as the final tie-break.

It places state boundaries and pinned terminals first. Remaining gates are taken
in stable topological order, with gates on longer dependency paths ordered before
equal-level non-critical gates.

### 6.2 Placement

For each instance, the builder first expands the selected topology, then embeds
its primitive nodes in stable topology order. The first primitive enumerates a
deterministic shell of legal anchors and facings around the weighted median of
already placed external neighbours. Later primitives also weight already placed
topology neighbours. Critical predecessors receive the highest weight, followed
by pinned boundary distance, internal-edge distance, and fanout. Topology roles
and internal-edge indices are the final tie-breaks.

The partial embedding exists only inside the seed builder's bounded backtracking
state. A primitive location is not committed merely because its estimated
distance is good: every newly closed external or internal topology edge must
route and pass construction-time local legality checks for collision,
reservation, terminal direction, and signal-strength reachability. Failure may
backtrack within the instance or to an earlier instance.

The shell grows only when every anchor in the current shell is refused. This
produces intentional whitespace where routing needs it without reserving global
corridors or regions. Candidate order is coordinate-, facing-, implementation-,
instance-ID, then topology-role order so the same input always creates the same
seed. Placement is a bounded deterministic depth-first search, not an irrevocable
greedy walk: if the current primitive exhausts its shell limit, the builder
backtracks to the most recent primitive with an untried legal candidate. The seed
limits record the maximum shell radius and backtrack count in the result.

### 6.3 Routing

Each newly closed net is routed immediately against live reservations. This
includes every mandatory signal connection in `ValidatedTopology`; internal edges
are not hidden inside an opaque cell delay. A net is closed when all currently
known sinks whose placement is fixed can be connected. When later fanout adds a
sink, the whole affected tree may be rebuilt; appending a branch is not assumed
safe.

The router is strength-aware. Repeater conduction and refresh use
`BlockState.facing`: a route may enter only the input face and leave only the
output face. A `BlockKind::Repeater` without its state is insufficient evidence of
connectivity or refreshed strength.

If no candidate in a bounded shell routes, the seed builder increases that
shell's radius and retries; after the radius limit it backtracks. The seed has
explicit radius and backtrack limits and returns a named `SeedExhausted` error
containing the instance, attempted radius, backtracks used, and last physical
refusal. It never silently falls back to the old generator.

Construction-time checks are not called physical certification. The existing
`realise_and_verify` requires a complete candidate with every logical gate input
routed, so it runs only after the full seed is materialised. A locally legal
partial seed may still be rejected by that authoritative full-circuit pass and
cause deterministic backtracking.

## 7. Realised timing graph and fragment selection

The search does not use the logical netlist's depth as a substitute for physical
timing. After the seed is certified, it derives an immutable
`RealisedTimingGraph` from that candidate's explicit instances, implementations,
expanded topology nodes and edges, sink assignments, and routed terminal
metadata. This graph is guidance for where to search; simulator-observed settle
time remains the quality score.

### 7.1 Authority and lifetime

`InstanceGraph`, the immutable cell library, and the certified `PlanCandidate` are
the source of truth. A timing graph is derived data and may never write placement
or routing state back into them. The complete graph is rebuilt after every
accepted candidate. Rejected proposals do not alter it.

Full rebuild is intentional. A fragment may add instances, change sink drivers,
change cell implementations, and replace entire route trees at once. Incremental
repair would need to invalidate every one of those relationships and creates a
second mutable graph whose stale edges could misdirect later search. Rebuilding a
DAG from candidate metadata is cheap relative to routing, realisation, and the
transition sweep.

### 7.2 Nodes and arcs

Timing identity is physical, primitive-specific, and landing-specific. A topology
may contain several delayed primitives, so an `InstanceInput -> InstanceOutput`
black-box arc is not an authoritative timing model.

```rust
enum RoutedSinkId {
    Connection(ConnectionId),
    DeclaredOutput(PortId),
}

enum TimingNodeId {
    PrimaryInput(PortId),
    InputNet(PortId),
    Landing(ConnectionId),
    PrimitiveOutput(PrimitiveId),
    InstanceOutput(InstanceId),
    JunctionOutput(InstanceId),
    DeclaredOutput(PortId),
}

struct TimingArc {
    id: TimingArcId,
    from: TimingNodeId,
    to: TimingNodeId,
    kind: TimingArcKind,
    delay_game_ticks: u64,
}

enum TimingArcId {
    InputBinding(PortId),
    Route(RouteId, RoutedSinkId),
    Primitive(PrimitiveId, ConnectionId),
    TopologyOutput(InstanceId),
    Junction(InstanceId, u16),
    OutputBinding(PortId),
}

enum TimingArcKind {
    InputBinding {
        port: PortId,
        pinned_reader: bool,
    },
    Route {
        route: RouteId,
        sink: RoutedSinkId,
        repeaters: u64,
    },
    Primitive {
        primitive: PrimitiveId,
        landing: ConnectionId,
        implementation: ImplementationKey,
    },
    TopologyOutput {
        instance: InstanceId,
    },
    Junction {
        instance: InstanceId,
        contributor_index: u16,
    },
    OutputBinding {
        port: PortId,
    },
}
```

An `InputBinding` arc connects the caller-facing primary input to the routed
input net. It costs one repeater for a pinned input's normalizing reader and
zero for an unpinned input. Route arcs driven by a primary input start at
`InputNet`, because their terminal repeater counts begin after that reader.

A `Route` arc connects one routed physical driver to one concrete signal landing
or pinned declared output. An assigned instance input maps through the
`ValidatedTopology` external connection; an internal topology edge maps through
its stable `ConnectionId`. Both therefore terminate at a `Landing` node. Fanout
creates one timing arc per sink branch, and an internal topology edge creates its
own route arc; neither assigns one delay to a whole logical net or cell. A bare
merge connection has no target primitive: its landing feeds a verified junction
contribution instead of a `Primitive` arc.

Only signal-carrying `ConnectionSpec`s produce timing landings and route arcs.

A `Primitive` arc connects each landing on one topology primitive to that
primitive's output. Multiple declared inputs may land on the same primitive and
therefore produce distinct arcs to one output. A multi-node topology forms a real
alternating path of primitive and route arcs. For example, the two-torch `BUF` is
modelled as:

```text
external landing -> first torch -> internal-edge route
                 -> second landing -> second torch -> instance output
```

`TopologyOutput` is a zero-delay ownership boundary from the primitive named by
the validated topology output to `InstanceOutput`. It lets compatibility and
observation remain keyed by `InstanceId` without hiding the primitive path that
produced the signal. Ordinary downstream route arcs start at `InstanceOutput`;
topology internal-edge routes start directly at their source `PrimitiveOutput`.
Duplicate instances have distinct landing, primitive-output, and instance output
nodes even when they implement the same logical gate.

A bare merge has no output primitive, so its routed contributors feed a
`JunctionOutput` through zero-delay `Junction` arcs. An isolated merge's topology
repeaters appear as `Primitive` arcs before those junction arcs. Merge duplication
remains excluded as specified in section 5. Route metadata and topology ownership
must agree on every emitted repeater so an isolating repeater is charged exactly
once, never once as a topology primitive and again as a route terminal. A
`RealisedJunction` may contain only zero-delay conductive material; any delayed
component on a contributor path must appear as a topology primitive or routed
connection before the junction arc.

An unpinned output lamp is placed directly from its driver and has no route
terminal in today's candidate. A zero-delay `OutputBinding` arc connects that
driver to `DeclaredOutput`; its display delay remains excluded from static
fragment ranking. A pinned output instead reaches `DeclaredOutput` through its
real route arc, whose terminal repeater count includes the delivery repeater, and
does not receive an additional output binding.

Every node and arc has a stable identity assembled only from port, instance,
topology role or edge index, route, sink, and declared input indices.
`TimingArcId` is separate from `TimingArcKind`: repeater count, implementation
choice, primitive kind, delay, coordinates, and map iteration order are payload or
derived data and never participate in identity. Changing an implementation may
add or remove derived identities; the complete graph rebuild in section 7.1 is
what makes that safe.

### 7.3 Delay weights

All graph weights use integer simulator game ticks.

- A route arc costs the sum of delays of its route-owned repeaters. The selected
  sink terminal's metadata is branch-specific and must exclude any repeater that
  is already represented by a topology-owned `Primitive` arc.
- A pinned input binding costs exactly one minimum-delay repeater; this is the
  normalizing reader before the route source and is not present in any downstream
  `RouteTerminal::repeaters`. An unpinned input binding costs zero.
- Dust distance, turns, stairs, and ordinary conductive blocks cost zero game
  ticks in the current model. They remain block-count, strength, congestion, and
  routability costs; they are not converted into invented timing delay.
- A primitive arc reads delay from the realised primitive and its emitted state.
  A torch currently costs `TORCH_DELAY_GAME_TICKS`; a topology-owned repeater uses
  its actual configured delay. Dust and junction arcs cost zero. A future
  primitive with input-specific timing must declare those arcs in the cell
  library rather than inherit a universal one-gate delay.
- `entry_cost` or an equivalent whole-entry summary may order unmaterialised
  proposals, but it is only a lower-bound estimate. Certified timing is rebuilt
  from expanded primitive and actual routed arcs.
- The delivery repeater of a pinned output belongs to its route and is counted.
  Delay from a caller-owned probe or an unpinned display lamp is excluded from
  fragment ranking because the generator cannot optimise it. The simulator's
  final transition metric still includes the benchmark fixture, so this exclusion
  cannot make a slower circuit win.

Route length is never used as timing under another name. It may break an equal
timing score in favour of fewer emitted blocks, consistent with section 10.

The current `critical_path_delay` shortcut adds a generic sink gate cost to a
pinned output terminal even though that terminal's repeater count already
includes its delivery repeater; it therefore overcounts that endpoint by two game
ticks. The realised timing graph's explicit route-to-output arc is the corrected
model. Reconciliation tests must encode the correction rather than preserving
the legacy double count.

### 7.4 Static analysis

The accepted input is combinational, so the graph must be a DAG. Encountering a
stateful gate, combinational cycle, or unassigned port is a named construction
error, not a cut edge or zero-delay fallback.

Forward arrival is a max-plus traversal:

```text
head[v] = max(head[u] + delay(u, v))
```

Reverse traversal computes the longest remaining delay to any declared output:

```text
tail[u] = max(delay(u, v) + tail[v])
critical_delay = max(head[declared_output])
edge_slack(u, v) = critical_delay - (head[u] + delay(u, v) + tail[v])
```

An edge with zero slack is in the static physical critical cone. Predecessor
selection compares `head[u] + edge_delay`, not merely `head[u]`. Equal sums use
stable timing identity as the final tie-break.

This distinction is required by the measured `full_adder` regression: `g19` and
`g20` can both arrive at game tick 34, while `g19 -> g21.in[0]` contains three
repeaters and `g20 -> g21.in[1]` contains none. The former gates `g21`; choosing
by arrival alone names the latter incorrectly. The new graph must select `g19`
because its weighted arrival is larger.

### 7.5 Dynamic transition overlay

The static longest path can be a false path that no input transition sensitises.
The simulator therefore supplies a read-only overlay, not another timing model:

```rust
struct TransitionWitness {
    manifest_index: usize,
    settle_game_ticks: u64,
    critical_target: TimingNodeId,
    arrival_game_ticks: BTreeMap<TimingNodeId, u64>,
    glitched_nodes: BTreeSet<TimingNodeId>,
    kind: WitnessKind,
}

enum WitnessKind {
    Active,
    StaticFallback,
}
```

Realisation records one verified observation position for every delayed primitive
output, concrete instance output, and junction output. Primitive observations are
keyed by `PrimitiveId`; `CompiledCircuit` additionally exposes instance and
junction output positions by `InstanceId` for compatibility, so two duplicates
cannot collapse into one observed gate. An ordinary instance output aliases its
selected primitive's observation position, while a merge uses its separately
verified junction observation point.

Observation identity is typed end to end. The observer registration, raw event,
per-transition result, and summary use `TimingNodeId` (or an equivalent typed
`ObservationId` that maps one-to-one to it) as the key. A logical signal label is
display metadata only and may not key or merge timelines. In particular, two
duplicates of one logical gate must produce two independently retained event
sequences even when their human-readable labels match.

The observer records primary inputs, every delayed primitive output, concrete
instance and junction outputs, and declared output probes. Landings remain
unobserved. For dynamic backtrace, an incoming candidate to an observed node is a
complete physical corridor from one observed upstream output through any
unobserved route, landing, and zero-delay ownership arcs. Its score is the
observed predecessor arrival plus the sum of every arc on that corridor. Junction
and declared output nodes use the same rule with their corresponding junction,
route, or binding arcs. Observing internal delayed outputs avoids reducing a
branched or reconvergent cell topology to static fallback while still avoiding
fictional observations at input sockets.

For every transition tied at the current maximum settle time, the selector:

1. takes the latest changed declared output as `critical_target`; if no declared
   output changed, it takes the latest changed instance, junction, or primary
   input; stable timing identity resolves equal arrival ticks;
2. restricts the physical graph to output nodes present in
   `arrival_game_ticks`, while retaining unobserved landing and primitive nodes
   on corridors between them;
3. walks backwards using observed driver arrival plus complete physical hop
   delay;
4. records the resulting active physical cone as one witness.

Witness construction scans the complete transition result set and retains every
index whose settle time equals the maximum. It must not reuse the existing
single `worst_transition_index` compatibility summary, whose tie policy keeps
only one transition.

Glitches and same-tick events can make simulator logs insufficient to prove one
unique causal predecessor. The overlay therefore does not certify causality and
does not alter the measured latency. If no exact active predecessor exists, the
selector keeps the static weighted predecessor and marks the witness as static
fallback. Physical verification, the immutable transition manifest, and the
simulator remain the authorities for correctness and score.

Only `Active` witnesses contribute to active-witness coverage. A
`StaticFallback` edge receives zero active coverage and is considered only after
every hotspot appearing in at least one active witness. If a transition has no
exact active hop at all, its fallback may guide search after the active queue is
empty; it can never make an unsensitised static path outrank an observed path.

Candidate-specific witnesses may rank fragments but may not add, remove, or
reorder transitions in the immutable manifest.

### 7.6 Hotspot ranking

The first hotspot is the highest-ranked timing arc, primitive, or instance under
this stable tuple:

```text
(appears in any active witness, true first,
 number of maximum-settle active witnesses containing it, descending,
 controllable game-tick delay, descending,
 attributable non-air blocks, descending,
 fanout count, descending,
 stable timing identity, ascending)
```

`controllable game-tick delay` excludes caller/display delay and includes only
primitive or routed repeater delay the transaction can change. A previous physical
refusal may suppress retrying the identical fragment-choice fingerprint, but it
does not change timing weights or reorder unrelated choices.

`attributable non-air blocks` is the number of distinct emitted cells in the
hotspot's minimal instance plus touched-route-tree closure. A shared trunk is
counted once in that closure, not once per fanout timing arc.

A fragment starts at that hotspot and expands over physical timing predecessors,
successors, and primitive ownership until one deterministic cap is reached:
instance count, boundary-net count, or Manhattan radius. Selecting an internal
primitive or topology edge always absorbs its owning `InstanceId`; a transaction
never edits half a selected cell topology while leaving the other half frozen.

Expansion uses a priority queue ordered by physical timing distance from the
hotspot, zero-slack before nonzero-slack, controllable delay descending, then
stable timing identity. For each popped item, the selector computes the complete
tentative instance and touched-route-tree closure first. It accepts that item only
if the resulting set satisfies all three caps; otherwise it skips the item and
continues. Expansion stops when the scheduled instance count is reached or the
frontier is empty. Thus simultaneous cap boundaries do not require an implicit
cap priority and cannot depend on collection iteration order.

### 7.7 Route-tree closure and fragment boundary

The current `Route` representation owns one source net's complete route tree, not
independent branches. A fragment therefore closes over whole touched route trees:
changing an instance, sink assignment, terminal, or topology internal edge
absorbs every route tree that touches it into the transaction. Every internal
route owned by an affected instance is included even if it does not cross the
fragment boundary. All cells of those trees are removed and rebuilt atomically.
Instances outside the logical fragment stay fixed and expose frozen source and
sink terminals at the boundary; unrelated route trees remain byte-identical.
There is no branch-level cut or partially retained trunk in this design.

For a junction driver, closure follows each electrical contributor's route tree.
That does not transfer ownership of an upstream primitive to the merge instance:
an upstream instance outside the fragment remains frozen at its source terminal,
while the absorbed contributor route is rebuilt to the new verified junction.

The fragment boundary freezes:

- every source terminal entering the fragment;
- every sink terminal leaving the fragment;
- all pinned terminals and their caller-facing geometry;
- all physical cells owned by instances outside the fragment;
- every route tree not included by the touched-net closure.

Thus a transaction can completely rewrite its interior without allowing changes
to leak into unrelated routes.

Fragment sizes follow the fixed default schedule 1, 2, 4, and 8 instances. Larger
fragments are considered only after smaller alternatives for the same hotspot
have failed or stopped improving. The schedule, instance cap, boundary-net cap,
and Manhattan-radius cap are fields of `SearchConfig` and are copied into result
metrics; there are no additional hidden caps in the search loop.

### 7.8 Required timing-graph regressions

Before fragment search is enabled, non-ignored tests must prove:

- equal observed source arrivals with unequal routed delays choose the weighted
  predecessor (`g19`, not `g20`, in the measured full-adder case);
- two fanout sinks retain different branch-specific repeater delays;
- a pinned input path includes its normalizing reader exactly once, while the
  equivalent unpinned input binding adds no delay;
- an unpinned directly placed output reaches `DeclaredOutput` through a zero-delay
  binding despite having no route terminal;
- a pinned output counts its delivery repeater exactly once and does not preserve
  the legacy sink-gate double count;
- duplicate instances never collapse to one timing node, observation point, or
  event timeline, including when their logical labels are equal and their
  observed arrival ticks differ;
- a direct new-instantiator test of the two-torch `BUF`, independent of the
  shipping lowered corpus, expands to two distinct primitive outputs and one
  internal routed landing; its static path charges both torch delays and every
  actual internal-route repeater exactly once;
- a mixed two-input merge with one bare and one isolated branch has the exact
  isolation-mask fingerprint, no fictional output primitive, a non-empty verified
  contributor set, and one charge for the isolating repeater;
- changing a selected implementation key may change the primitive-node and
  internal-edge sets, and a rejected change leaves no derived identity, route, or
  placement behind;
- bare merge traversal adds zero primitive delay and does not double-charge an
  isolated topology repeater as both a primitive and route component;
- rebuilding the same candidate yields an identical graph fingerprint and
  critical predecessor map;
- duplicating the two-torch `BUF` produces disjoint primitive IDs, connections,
  placements, observation points, and event timelines for both copies;
- graph critical delay reconciles with the existing candidate delay model and
  full simulator sweeps on every reference circuit where the path is sensitised;
- two transitions tied for maximum settle time but traversing different active
  cones both produce witnesses, independent of manifest order;
- a maximum-settle transition with no changed declared output chooses its latest
  changed internal or primary-input target without inventing an output event;
- an intentionally unsensitisable static longest path cannot receive active
  coverage or outrank the shorter path a transition actually sensitises;
- an equal-distance frontier that reaches multiple caps produces one stable
  fragment-choice fingerprint across repeated builds;
- after one fragment is rebuilt, every unrelated route tree remains byte-for-byte
  identical to the parent candidate;
- caller fixture delay is present in simulator metrics but absent from
  controllable hotspot delay.

## 8. Fragment transaction

A proposal transaction clones the current certified candidate into private state
and materialises exactly one tuple from a deterministic best-first enumeration of
these choices:

1. cell-library implementation for each included physical instance;
2. zero or more combinational duplicates, subject to a fragment-local cap;
3. explicit assignment of boundary and interior sinks to valid instances;
4. anchor and facing for every affected derived primitive node;
5. complete rerouting of every external net and topology internal edge touched by
   those choices.

The next tuple is ordered by an admissible lower-bound tuple:

```text
(estimated critical latency,
 estimated non-air blocks,
 estimated occupied-volume increase,
 canonical fingerprint)
```

Estimates guide enumeration only. They never replace realisation, verification,
or simulation.

### 8.1 Atomic evaluation

The words transaction, proposal, and evaluation name the same budget unit: one
complete materialised tuple and its terminal outcome. One evaluation performs:

1. expand every selected topology and materialise every chosen primitive,
   external route, and topology-internal route in private transaction state;
2. call the existing physical realiser and verifier;
3. reject on collision, coupling, signal-strength, directionality, pin-halo, or
   connectivity failure;
4. run deterministic screening, then the complete immutable transition manifest
   and functional certification before the candidate can become a parent or best;
5. measure observed latency and physical size;
6. compare against the best certified candidate;
7. commit the entire proposal only if it is strictly better.

A refusal consumes one evaluation and records a structured reason. Partial
placement, partial rerouting, and unverified score improvements are discarded.

### 8.2 Implementation replacement

An implementation choice is an `ImplementationKey`; it never names a placement
macro. The library validates and expands that key, and the transaction enumerates
legal primitive embeddings using the same physical variants and routing
primitives as the seed. It may change primitive count, topology roles, internal
edges, external input landings, merge-isolation mask, and output semantics
atomically. It may not preserve an old primitive or internal route merely because
its coordinates happen to remain usable.

Only keys admitted by the library for the logical gate kind are enumerable.
Existing implementation-specific legality still applies: for example, a clear
merge-isolation bit is illegal when that producer has another consumer. Estimates
may use immutable library costs, but acceptance always uses the fully expanded,
realised candidate.

### 8.3 Duplication

Duplication is proposed only for combinational gates with multiple sinks. It is
useful when separating a fanout tree removes repeaters or shortens a critical
branch enough to pay for the copied cell.

The transaction partitions sinks among the canonical instance and duplicates,
then reroutes all affected input and output nets. It does not copy a routed world
fragment. A duplicate receives a new `InstanceId`; all of its
`PrimitiveId`s, connections, landings, internal edges, placements, and routes are
derived afresh from its selected implementation key and must pass the same
verification as every other instance.

## 9. Certification and best retention

There are two certification levels.

### 9.1 Structural certification on every proposal

This project reuses the existing physical rule implementations, not the old
gate-level candidate shape. `realise_and_verify` receives an
`ExpandedPhysicalCandidate`, or a lossless adapter with equivalent information,
containing the complete `InstanceGraph`, ordered `SinkAssignment`s, primitive
placement and facing, connection routes, junction contracts, observations, and
pin contracts. The candidate's carried `ValidatedTopology` is not trusted as an
authority: for every instance, the verifier independently calls the immutable
library instantiator with `(logical gate, InstanceId, ImplementationKey)`, then
compares the complete expected node, connection, output, and fingerprint sets
against candidate state. Only after that equality succeeds does it emit and run
the existing collision, coupling, strength, direction, and connectivity rules. A
projection that hides internal connections or merges duplicate identities is not
a valid adapter.

For a junction, this comparison and the physical connectivity pass jointly prove
the full transfer closure: every expected ordered contributor reaches the one
verified observation point, no unlisted contributor reaches it, and every outgoing
route tree assigned to that logical signal starts from that junction driver.

Every proposal must pass `realise_and_verify`, including:

- no illegal physical overlap;
- no unintended redstone coupling;
- exact expansion of every selected topology node, primitive kind, declared input
  mapping, internal edge, and output or junction mapping;
- no missing or extra primitive, connection, route, observation, or timing owner;
- every `output = None` instance has a non-empty contributor set and a verified
  junction observation point, with logical and electrical ownership kept distinct;
- exact sink and source connectivity;
- valid signal strength along every route;
- repeater input/output direction;
- pinned IO cell, handover, and halo rules.

Non-ignored corruption tests take an otherwise certified multi-node candidate and
independently remove one internal connection, change one primitive kind, reverse
one primitive facing, add one extra primitive, mismatch an `ImplementationKey`
and topology, reassign one sink, swap a junction's logical owner with a contributor,
and falsify one observation point. Structural certification must reject every
mutation before functional simulation.

### 9.2 Functional and timing certification

A `TransitionManifest` is constructed before seed generation and is immutable for
the whole run. With at most four inputs it contains every ordered pair of distinct
input vectors. With more inputs it contains all-zero, all-one, every one-hot and
one-cold vector, and every single-bit toggle among those vectors. Search-time
screening uses a fixed prefix of this manifest plus manifest entries that were
worst for earlier certified candidates. This may reject a proposal early, but it
may not promote one to `best_certified`.

Before a proposal replaces `best_certified` or becomes the parent of another
proposal, it runs the complete certification set:

- exhaustive truth table for combinational circuits with at most eight inputs;
- for combinational circuits above that limit, a candidate-independent formal
  equivalence proof between the lowered netlist and the verified realised
  instance-and-routing graph; this may be a compositional instance-aware
  extension of the existing structural equivalence checker or a SAT miter, but
  it must prove every input vector rather than sample them;
- every ordered pair of distinct input vectors for circuits with at most four
  inputs (the four-input decoder has 240 transitions);
- for larger input counts, every transition in the immutable manifest;
- every transition settles within the configured cap;
- pinned output handovers pass structural strength checks, and functional output
  is read from an independently installed probe in the caller-owned pin cell;
- external high means signal strength greater than zero, not necessarily 15.

Each measured transition starts from a fresh simulator loaded with the candidate
world. The benchmark-owned fixture applies the source vector as one batch, settles
and checks it, records `start_tick`, then applies the destination vector as one
batch without stepping in between. Latency is the `current_tick - start_tick`
game-tick delta until whole-world quiescence. A previous transition's event queue
or component state must not leak into the next measurement. Divergence is a
certification failure, never a capped numeric score. The result records the
manifest hash, transition count, cap, and lowest manifest index attaining the
maximum. Acceptance benchmarks use a 2,048 game-tick cap.

Candidate-specific static critical paths may guide fragment selection but may not
change the manifest or the transitions used to compare candidates. For more than
four inputs the metric is explicitly named
`max_observed_settle_game_ticks_on_manifest`, not worst-case latency. Candidate
metrics are comparable only when their manifest hashes match.

The transition manifest is a timing workload, not a substitute for functional
equivalence. A circuit above the exhaustive simulation limit cannot become
`best_certified` or satisfy deletion coverage until its formal proof succeeds.

The result owns a `best_certified` candidate from the moment the seed passes.
Timeout, exhausted budget, failed proposals, and internal search refusal cannot
overwrite it. The result separately reports why the loop stopped and whether any
proposal improved the seed; exhausting a budget without improvement is therefore
`StopReason::EvaluationBudget` plus `improved: false`, not an error.

## 10. Objective and metrics

The quality key is lexicographic:

```text
(max observed settle game ticks on the fixed manifest,
 non-air block count,
 occupied bounding volume,
 static routed delay)
```

Functional correctness is a prerequisite, not a score term. A deterministic
candidate fingerprint chooses the canonical representative only when the
complete quality key is equal. A fingerprint-only replacement is not an accepted
quality improvement. This comparator ranks certified candidates during search;
it does not waive the independent replacement conditions in section 13.

The implementation adds one shared emitted-world metrics API and one candidate
metrics record:

```rust
struct PhysicalMetrics {
    non_air_blocks: u64,
    occupied_min: Anchor,
    occupied_max: Anchor,
    occupied_volume: u64,
    blocks_per_lowered_gate: Ratio,
}

struct CandidateMetrics {
    physical: PhysicalMetrics,
    max_observed_settle_game_ticks_on_manifest: u64,
    transition_manifest_hash: Fingerprint,
    realised_timing_graph_fingerprint: Fingerprint,
    static_routed_delay: ExactDelay,
}
```

`static_routed_delay` is the `RealisedTimingGraph`'s `critical_delay` converted
through the existing exact delay type; it is not recomputed from logical depth or
geometric wire length.

The canonical candidate fingerprint covers the complete `InstanceGraph` including
logical owner and canonical/duplicate role, every `ImplementationKey` including
merge parameters, every validated-topology fingerprint, ordered
`SinkAssignment`s, typed endpoint identities, ordered junction contributors,
primitive placement, facing and physical variant, observation identity, pin
contract, and all internal and external routes. The emitted-world fingerprint
remains separately recorded. Two candidates that emit the same blocks accidentally
but disagree about ownership, assignment, or topology are not the same candidate.

`World::size()` is allocated storage size and must not stand in for occupied
bounding volume. Packing ratio may be reported as a diagnostic, but it is not an
objective because adding empty space can improve it artificially.

## 11. Budget semantics

Tests and reproducible benchmarks use an exact evaluation budget:

```rust
enum SynthesisBudget {
    Evaluations(u64),
    Time(Duration),
}
```

For a fixed `SynthesisCaseFingerprint`, search is a deterministic state machine.
The fingerprint covers the lowered netlist and port order, pin placements,
cell-library revision, `SearchConfig`, `CertificationConfig`, simulator and
verifier revisions, and transition-manifest hash.

The requested budget may be read only at the loop boundary to decide whether
another proposal may start. It must not affect seed construction, hotspot
ranking, fragment selection, proposal enumeration, certification, or acceptance.
Every proposal has fixed deterministic internal caps and reaches one terminal
outcome.

Each `ProposalTrace` entry records its index, parent-candidate fingerprint,
fragment identity, choice fingerprint, terminal outcome, certified quality key
when one exists, and whether it was accepted. For independently started
`Evaluations(a)` and `Evaluations(b)` where `a < b`, entries below `a` must be
byte-identical unless both runs exhaust the proposal space earlier. The best
quality at budget `b` therefore cannot be worse than at budget `a`.

`Time(d)` executes the same proposal stream. The optimisation deadline starts
after seed certification and is checked only before starting the next
proposal transaction. A proposal transaction that has started is allowed to
finish certification, so the returned result is never a half-evaluated candidate.
If time mode completes `k` evaluations, its result and trace prefix must equal
`Evaluations(k)`. The deadline is never an input to generation or scoring. Seed
time and overrun by the last in-flight proposal are reported separately.
Wall-clock mode is a soft interactive budget and is not used in regression
assertions.

The result reports:

- evaluations started and completed;
- proposals refused by category;
- accepted improvements;
- seed and final metrics;
- synthesis-case, canonical candidate, and emitted-world fingerprints plus the
  complete proposal trace;
- elapsed time as observation only;
- stop reason.

## 12. Pinned IO contract

Pinned IO remains an abstract caller-owned signal handoff. `toward` names signal
travel direction, not geometric outward facing:

- a pin is an absolute `Anchor` plus a horizontal `toward: Facing`;
- the caller-owned pin cell remains air in REDA's emitted world;
- input handover is the REDA-owned cell at `at + toward`;
- output handover is the REDA-owned cell at `at - toward`;
- only the declared handover neighbour may carry the pin's signal;
- the other five neighbours may contain inert support, floor, or fill but no
  signal-carrying conductor or route;
- pinned terminals never move during seed construction or optimisation;
- viewer geometry and logical-to-physical annotations continue to expose the pin
  and handover distinctly.

These rules are checked at transaction boundaries and again by final physical and
simulator certification.

## 13. Replacement gate

The legacy generator is removed from the shipping path only after all conditions
below pass from a clean checkout with fixed seeds and recorded budgets.

### 13.1 Correctness corpus

The new generator must independently seed, route, realise, verify, and simulate:

- `and4`;
- `verilog:and4`;
- `full_adder`;
- `segment_a`;
- the handwritten `seven_segment` stress circuit;
- pinned `verilog:seven_segment` using the checked-in digit-glyph pin geometry.

Every applicable truth table, ordered transition sweep, pin geometry check, and
physical verifier must pass. An ignored measurement that prints an error is not
acceptance evidence.

The replacement corpus covers every feature class the shipping compile front
doors successfully compile at the immutable baseline commit: pinned and unpinned,
single- and multi-output, handwritten and Verilog, and every supported physical
cell class. A feature that the baseline front door explicitly rejects, including
stateful DFF placement at the time of this spec, is not deletion coverage. If that
support changes before switchover, the manifest and corpus must be extended before
legacy deletion.

### 13.2 Quality gate

Before optimisation tuning starts, the branch commits an immutable
`BenchmarkBaseline` manifest. It records the baseline commit SHA, build profile
and features, generated-world fingerprint, lowered-netlist hash, pin-manifest
hash, cell-library revision, simulator and verifier revisions,
transition-manifest hash, and all baseline metrics. Legacy and new worlds are
measured by the same acceptance evaluator and benchmark-owned fixtures; the
baseline does not move with `main`. Historical block and tick counts are not
copied into assertions.

If the legacy generator has no certified result for a corpus case, that case has
no numeric baseline. The new generator must still certify it, and success is
recorded as new coverage rather than a percentage comparison.

- No corpus circuit may regress in worst observed settle ticks.
- No corpus circuit may regress in non-air block count.
- Occupied bounding volume is recorded and remains a tertiary tie-break, but is
  not an independent replacement failure: intentional whitespace may reduce
  congestion, latency, and emitted blocks.
- For pinned `verilog:seven_segment`, widened integer arithmetic must satisfy
  `10 * new_settle_game_ticks <= 9 * baseline_settle_game_ticks`, and its new
  non-air block count must be strictly lower. A 10% block-count reduction is a
  stretch result, not a deletion requirement.
- Budget monotonicity must hold at 0, 1, 2, 4, 8, and the shipping evaluation
  budget: used evaluations never exceed the budget and quality never worsens as
  the budget grows.
- Every checked evaluation budget runs in at least three fresh processes and in
  shuffled budget order. The acceptance evaluator compares the complete trace,
  certified metrics, canonical candidate fingerprint, and separately recorded
  emitted-world fingerprint, excluding elapsed time. If parallel evaluation is
  added, the same check runs with one worker and the shipping worker count;
  proposal commitment remains index-ordered and byte-identical.

### 13.3 Switchover

Until the gate passes, the new entry point is explicit and the old production
entry point remains unchanged. The deletion commit applies this API migration:

| Existing front door | Replacement behaviour |
| --- | --- |
| `compile(&Netlist)` | Treat the argument as already lowered, use no pins and the fixed shipping evaluation budget, return the result's `CompiledCircuit`. |
| `compile_grown(&Netlist, &PortPlacements)` | Use the same new generator and shipping budget with the supplied pins; retain as a compatibility name only if callers still need it. |
| `compile_fragment_synth(SynthesisInput, SynthesisBudget)` | Remain the advanced API returning the circuit, metrics, trace, and stop reason. |
| `PlannerKind` | Expose the fragment synthesiser; remove variants that select deleted generation policies. Internal routing strategy enums are not public generator choices. |
| `compile_legacy` | Deprecate while both paths coexist, then remove with the legacy implementation in the deletion commit. |
| CLI, baker, and viewer | Use the fixed shipping evaluation budget and pin behaviour recorded in `BenchmarkBaseline`; never use a wall-clock budget for checked-in artifacts. |

The fixed shipping budget is the smallest checked evaluation budget that passes
the replacement gate and is committed to the baseline/acceptance configuration;
it is not selected dynamically at runtime. Once the gate passes:

1. route the normal CLI, library API, checked-in baked artifacts, and viewer build
   through `compile_fragment_synth`;
2. regenerate checked-in artifacts and pinout sidecars;
3. run root, viewer, WASM, physical, and truth-table acceptance;
4. remove the legacy generator and fallback chain in a separate deletion commit;
5. rerun the complete acceptance suite after deletion.

If the gate does not pass, the experiment remains isolated and the shipping path
does not change. "Can produce one good artifact" is not enough to delete the only
generator that covers the rest of the corpus.

## 14. Implementation sequence

1. Add shared world metrics and deterministic benchmark records.
2. Measure and commit the immutable legacy baseline before optimisation tuning.
3. Introduce `ImplementationKey`, `InstanceId`, `TopologyNodeId`, `PrimitiveId`,
   `ConnectionId`, `PhysicalEndpointId`, a one-to-one `InstanceGraph`, and pure
   validated-topology expansion. Prove directly against the new instantiator that
   NOR, two-node `BUF`, bare merge, mixed bare/isolated merge, and fully isolated
   merge expand with exact nodes, input mappings, internal relations, and output
   semantics.
4. Move candidate placement, facing, route ownership, sink assignments,
   verification provenance, and compatibility views onto the derived primitive
   identities; prove byte-identical one-to-one re-emission.
5. Add instance observation positions and the primitive-level
   `RealisedTimingGraph`; reconcile its weighted paths with candidate metadata and
   simulator sweeps before using it to steer search.
6. Fix the strength-aware routing representation so repeater `BlockState.facing`
   survives every planning and verification step.
7. Implement and certify topology-aware `SparseSeedBuilder` without the legacy
   generator.
8. Add evaluation budgets, best-certified retention, and deterministic proposal
   accounting with a no-op fragment enumerator.
9. Implement timing-witness hotspot selection, single-instance fragment
   replacement, and complete rerouting.
10. Add multi-instance fragments and combinational duplication.
11. Run the replacement gate and tune only through explicit configuration recorded
   by the benchmark.
12. Switch front doors, regenerate artifacts, and delete the old generator only if
   every replacement condition passes.

Each step is test-driven and lands as a reviewable commit. Representation changes
must not be bundled with search-policy changes, so a failure can be attributed to
one invariant rather than an entire rewrite.

## 15. Failure reporting

Every refusal names its stage and stable identity:

- seed exhaustion names the instance, topology primitive, and radius;
- fragment materialisation names the fragment and choice fingerprint;
- routing failure names the net or topology edge, source and sink
  `PhysicalEndpointId`s, and last router refusal;
- verification failure names the physical rule and relevant anchors;
- functional failure names the transition and disagreeing outputs;
- budget exhaustion reports the best certified metrics and completed count.

Diagnostics may retain the rejected proposal in an opt-in debug artifact, but the
normal result and checked-in generated artifacts may contain only certified worlds.

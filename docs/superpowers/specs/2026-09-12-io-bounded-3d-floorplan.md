# IO-bounded 3D floorplan

## 1. Purpose

REDA's topology-aware seed currently arranges every primitive and compiled
module in one horizontal placement frame. Routing may climb, but placement
does not: when a level is too wide it becomes another horizontal column.
For a top level whose complete IO is pinned, the result can contain large
empty regions, can grow beyond the rectangle implied by those pins, and
cannot use height to make the constrained design fit.

This design extends the existing seed with deterministic deck folding. It is
not a second generator and it does not replace whole-candidate certification.
The same placement path handles primitive gates and opaque compiled modules.

## 2. User-visible contract

### 2.1 Footprint from IO

When every declared input and output has a `PortPin`, the caller cells define
an inclusive horizontal footprint:

```text
min_x = min(pin.at.x)       max_x = max(pin.at.x)
min_z = min(pin.at.z)       max_z = max(pin.at.z)
```

Both spans must be non-zero. A complete pin set whose pins all share one X or
one Z coordinate is refused; REDA does not guess a margin or invent a board
size. A partial pin set retains today's unbounded behavior because it does not
fully describe a board.

Every coordinate returned by `relocate::anchors_of` for the final candidate
must remain inside this X/Z rectangle. Pin Y coordinates remain exact, but
pins do not set a maximum height. The ground rule remains unchanged and the
circuit may grow upward as far as required.

The footprint is an internal value derived after named pins are bound to
physical endpoints. `PortPlacements` does not gain a separately configurable
board outline.

### 2.2 Pin clearance

Each pinned port keeps the existing exact terminal spine:

```text
caller cell -> handover -> first internal net cell
```

The caller cell remains caller-owned and ships as air. The handover, its
facing, and the first internal net cell retain the existing `PortPin`
semantics.

In bounded mode, each terminal owns one exact two-cell-deep clearance tunnel.
Let `C` be the caller cell, `i` the inward horizontal direction (`toward` for
an input and `opposite(toward)` for an output), `l` the horizontal direction
perpendicular to `i`, and `u` be up. Then:

```text
H = C + i       handover
N = C + 2i      first internal net cell
tunnel = C + a*i + b*l + c*u
         for a in {0, 1}, b in {-1, 0, 1}, c in {-1, 0, 1}
```

The tunnel is intersected with the IO footprint and `y >= 0`. `H` and the
support immediately below `H` are occupied by the required terminal shape and
are exempt from keep-out; `C` remains caller-owned air. `N` is on the open
internal face, outside the tunnel, so the net can continue inward. No unrelated
REDA-owned body, support, route, or floor may occupy the remaining tunnel.
Pin-only tunnel intersections are refused during port validation. Macro
encroachment is checked after primitive and block placement, before routing;
the remaining cells are then inserted into the existing typed reservation map
so routes and floors cannot claim them.

This deliberately strengthens the old rule, which excluded only
signal-carrying cells from five face-neighbours. The stronger promise applies
only when a complete pin set activates bounded mode. Partial pin sets keep the
old terminal-clearance contract.

The clearance is fixed at one cell for this design. No public radius setting
is introduced.

### 2.3 Compatibility

- Unpinned circuits and partial pin sets preserve today's placement path,
  terminal-clearance contract, and fingerprints. Bounded 3D placement is a
  guarantee only for a complete, non-degenerate pin set.
- Pin coordinates, `toward`, handover positions, logical semantics, and
  external strength contract do not change.
- The pinned seven-segment's old candidate fingerprint and metrics are
  intentionally retired: its geometry is the acceptance target of this
  design. Its eleven `(Anchor, toward)` pairs remain byte-identical.
- The final artifact remains one flat candidate. Hierarchy is still only a
  placement and generation aid.
- The existing verifier, equivalence proof, simulator, and whole-candidate
  certifier remain authoritative.

## 3. Geometry model

### 3.1 IO footprint

Bounded mode derives a private `IoFootprint` containing inclusive world X/Z
bounds. It provides only checked projection and containment operations; it
does not own placement policy or become a new public parameter.

When a `PlacementFrame` turns, the same world-space rectangle is projected
into forward/lateral limits. Those limits feed today's `lateral_window`,
`forward_limit`, `confine_laterals`, and fold checks. No parallel region
dispatcher is added: complete pins supply both sides of the existing checks;
partial and unpinned layouts continue to supply today's open-ended limits.

### 3.2 Macro envelope

Every placeable instance exposes one axis-aligned local envelope:

- primitive instances derive width, depth, and height from the selected
  physical variant;
- compiled blocks extend `BlockFacts` with height from their existing
  `BlockBounds`;
- both are represented by the existing `InstanceId` and receive a
  `PreferredInstancePose` with an explicit Y coordinate.

Opaque blocks remain unrotated and internally unchanged. A deck move only
translates the already-certified block through the existing 3D `Offset` and
`translate` machinery.

### 3.3 Decks

A deck is a horizontal placement region inside the IO footprint. Its local
vertical reservation interval is derived once from facts known before routing:
the full primitive/block body and support bounds, their mandatory-air cells,
the existing channel slab, and each local route's existing
`max(source.y, sink.y) + 3` search ceiling. The next deck ground is the first
integer translation whose interval begins above the previous deck's interval.
This is height-aware and deterministic; it does not rerun channel planning for
candidate Y values and it does not assume that `ground + 3` covers a tall
macro.

Decks are assigned from bottom to top. Deck zero keeps today's
`frame.origin.y`; later decks receive the derived translation once. Full
primitive and block Y bounds keep macro collision repair horizontal within its
assigned deck. Materialized typed reservations and final verification check
the derived separation. A conflict is `NoDeckLayoutFits`, never an implicit
retry with a larger magic gap. The existing router caps still bound one
generation attempt, but the IO contract introduces no user-visible maximum
height.

Pins are not assigned to decks. Their absolute coordinates remain fixed and
the router connects them to the chosen macro decks.

## 4. Placement algorithm

### 4.1 Dynamic horizontal spacing

Only bounded mode replaces the current worst-case constants, and only where
the same information is already available:

- neighbouring macro gaps come from their actual conductor/socket envelopes;
- endpoint margins come from the terminal shapes present in that channel;
- turnaround allowance comes from the final channel's actual lane count.

The placer and channel layout derive turnaround from one helper in
`channel_plan`: the channel span comes from its planned lane count and the
placement allowance adds the existing forward margin. The current independent
`TURNAROUND_ALLOWANCE` and `TURNAROUND_CHANNEL` literals are not copied into
the bounded path. The row grid remains because routing and deterministic
tie-breaking depend on it. No continuous solver or new global-search
dependency is added. Legacy unbounded and partial-pin layouts keep today's
literal geometry and remain byte-identical.

### 4.2 Deterministic shelf packing

The existing topology analysis first produces macros in this stable order:

```text
(forward level, legalized lateral position, InstanceId)
```

The usable lateral span excludes a reserved vertical-trunk band. Existing lateral
folding converts an over-wide level into consecutive columns. Columns are then
packed into decks in order:

1. Start at the lowest legal deck.
2. Place consecutive columns until the next column would exceed the projected
   forward footprint. Existing lateral folding has already made each column
   fit the lateral span.
3. Start the next deck and continue with that column.
4. Never move a column back to an earlier deck and never enumerate alternative
   packings during seed construction.

This is a deterministic shelf pack, not bin-packing search. It uses the
minimum number of decks among packings that preserve the existing column
order. A single macro that cannot fit inside the usable footprint is refused
by name.

When bounded mode fits in one deck, every macro stays on the base deck, but
its X/Z position may improve through bounded dynamic spacing. Deck folding
activates only when the complete IO footprint requires it.

A complete footprint with at least one pinned input and one packed column is
direct-frame-only in this experiment. The footprint is the pins' own bounding
box, so a turned frame's rule that all levels start past the whole pin column
has no forward capacity. The existing frame loop remains to preserve the direct
frame's first refusal; this task does not invent a different turned-frame
pin-column contract.

Vertical-trunk demand and macro placement are resolved by one monotonic bounded loop:

1. Start with a zero-width vertical-trunk band.
2. Run bounded lateral legalization and deck packing in the remaining span.
3. Derive cross-deck nets in stable source/sink order. Boundary endpoints count
   as deck zero. The initial trial gives each physical source one stable lane;
   lanes are not reused.
4. Lane centres are row-grid cells in a reserved band immediately outside the
   confined macro extent, considered from the macro-facing edge outward. The
   channel window ends immediately before that band whenever at least one lane
   exists. A three-row corridor must avoid every effective terminal tunnel. If
   the current band exposes too few legal centres, enlarge it to the smallest
   strictly larger row-grid multiple that meets the current demand, or by one
   further pitch when the width already suffices but a centre is blocked, then
   rerun from a fresh pre-fold placement. The band never shrinks.
5. Stop when the current placement's lanes fit, or refuse when the remaining
   macro span cannot fit.

Because the band width is monotonic and the footprint is finite, this loop
terminates without a search over alternative layouts. Reserving the band
before lateral legalization prevents a later vertical trunk from crossing a
macro.

Trunk reachability is a routing-time property. Placement has no final route
anchors, and bounds reconstructed from macro envelopes reject lanes that the
existing router accepts. A lane excluded by the router's real per-sink search
box therefore surfaces as `SeedRoutingContext::VerticalTrunkUnroutable`; no
router API, cap, or refusal category is added.

Every post-plan horizontal movement obeys the same footprint predicate.
`InstancePlacementOverride`, `BlockPlacementOffset`, each candidate from
primitive collision repair, and the existing `move_owner`/`require_move_owner`
repair path are clipped while candidates are enumerated. The check uses the
oriented macro bounds already calculated by the placer, not a second envelope
calculation. If no collision-repair candidate remains, the existing
`SeedError::PlacementExhausted` path is used; an immovable layout repair keeps
the existing `ImmovableRepairOwner` path. The final candidate footprint check
is only an invariant backstop.

## 5. Routing

Each deck retains the existing horizontal channel layout at that deck's
derived ground Y. Nets whose endpoints share a deck use the current path
unchanged.

Cross-deck nets receive deterministic vertical-trunk lanes inside the
footprint. Their band is fixed by the placement loop before any final per-deck
channel layout is materialized:

1. Vertical-trunk demand is ordered by physical source identity and sink identity.
2. Each cross-deck net gets one lane on the existing row grid. Lane reuse is
   deferred until measurement shows that the simpler allocation is inadequate.
3. The existing router builds the staircases and refreshes; a new 3D router is
   not introduced.
4. Every vertical-trunk cell is seeded under its net in the existing
   `ChannelLayout.private` map before that deck layout derives `closed`. The
   channel's existing `ground..=ground+3` closed slab remains a channel-local
   routing rule, not a deck-height rule; vertical-trunk cells are holes through that
   slab. A deck's outer separation still uses the full dynamic union of macro,
   mandatory-air, channel, horizontal-route, and vertical-trunk connection
   reservations from Section 3.3.
5. Vertical-trunk, terminal-tunnel, block keep-out, and channel reservations use the
   existing typed reservation path.

The existing local `riser` closure in `channel_layout.rs` keeps its current
meaning: it reserves closed floor support beneath one deck's staircase. This
design calls the open cross-deck path a *vertical trunk* so the two opposite
reservation roles cannot be confused.

Deck zero owns physical input/output boundaries. On a source deck, a real
source receives one synthetic trunk sink at that deck's closing column; on a
sink-only deck, a synthetic trunk source at the opening column feeds the real
sinks; an intermediate deck receives both. Synthetic ends reuse the physical
source as owner, are marked by the existing flags, and are channel-planning
data only: final router requests still contain only real sinks.

Per-deck channel widening is keyed by `(deck, level)` so equal local levels on
different decks cannot alias. Existing `LayoutRepair::WidenChannel` remains
unchanged for legacy fingerprints; bounded mode uses a separate
`WidenDeckChannel`. Merging deck layouts unions `closed`, `private`, `floors`,
and `departures`; the report-only `lanes` field is keyed by `(deck, channel)`
rather than concatenated positionally. Existing channel margins are clamped
inward to the IO footprint.

`deck` identifies the channel in the current packed attempt; it is not a stable
placement key because widening may repack that level onto another deck. Retry
width therefore accumulates monotonically by the unchanged global forward
level. If that level moves, the canonical `WidenDeckChannel` entry is replaced
with its new deck while retaining the larger width. One global forward level
produces one packed column, so this does not widen an unrelated deck. Deck
layouts are attempted in ascending `DeckId`, preserving a deterministic first
error.

The footprint does not require a new `PhysicalReservations` API. Bounded mode
adds a one-cell-thick closed perimeter immediately outside the four X/Z sides
for the union of every real source/sink leg's existing search interval:
`min(source.y, approach.y)..=max(source.y, approach.y)+3`. Existing
`closed -> KeepOut` handling prevents a route from leaving and re-entering the
footprint. The ground rule closes the bottom; each leg's existing search bound
closes the top. A side that would lie below the growable world's zero boundary
is omitted because the world already closes it; all other perimeter
coordinates use checked arithmetic. The final exhaustive owned-coordinate
check remains authoritative.

## 6. Normalization and density

The initial trial does not add post-route compaction proposals. Dynamic
spacing removes unused worst-case macro and endpoint gaps during construction,
before any routing cost is paid. Shelf packing supplies legal decks when the
bounded footprint cannot hold the ordered columns on one deck.
This covers the observed fixed-spacing waste without duplicating proposal
logic across `FragmentProposalStream` and `HierarchicalProposalStream`.

If the bounded seven-segment still has poor fill after the trial, post-route
compaction gets a separate measured design. It is not speculative scope in
this implementation.

## 7. Acceptance policy

Hard-constrained deck folding is part of finding a legal placement. If a
one-deck layout exceeds the IO footprint, the minimum legal multi-deck layout
is accepted even when its required vertical trunks add delay. Bounds and correctness
are hard requirements.

The committed pinned seven-segment baseline does not certify, so it has no
physical `non_air_blocks / occupied_volume` result to compare. The executable
density gate is therefore plan-only and uses the same macro envelopes on both
sides:

```text
planned_macro_fill = sum(macro envelope volumes)
                     / volume(union bounding box of those envelopes)
```

The bounded plan must strictly improve this ratio over today's legacy 2D plan,
and the bounded candidate must newly route and certify. Its final physical
`non_air_blocks / occupied_volume` is reported and becomes the baseline for
later density passes; it is not compared to a nonexistent old physical result.
Tick and static-delay regressions are permitted for required deck folding, but
are reported rather than hidden. A large regression does not invalidate the
hard-bounds experiment, but the bounded path is not shipped as the default
until the user reviews that report. No numeric tick cap and no existing search
acceptance mode are added in this trial.

## 8. Errors

Failures are typed at the layer that can act on them:

- placement returns `SeedPlacementError::DegenerateIoFootprint` when a
  complete pin set does not span both X and Z;
- preplanning returns `PinRefusal::OutsideIoFootprint` when a pin's handover
  or first net cell lies outside the derived footprint;
- preplanning returns `PinRefusal::ClearanceConflict` when two terminal tunnels
  conflict;
- placement after materialization returns the same named
  `PinRefusal::ClearanceConflict` through `SeedError::InvalidPins` when a macro
  enters a terminal tunnel;
- the existing `SeedPlacementError::LateralWindowTooNarrow` names a macro that
  is too large for the bounded usable span;
- `SeedPlacementError::NoDeckLayoutFits` when no deck/vertical-trunk layout fits;
- `CandidateError::IoFootprintViolation` when final candidate ownership
  escapes the footprint;
- `SeedRoutingFailure::VerticalTrunkUnroutable`, surfaced through the existing
  `SeedError::Routing`, when a vertical trunk fails under existing router limits.

Physical pin overlap discovered only after materialization remains a
`CandidateError`; macro packing failures remain `SeedPlacementError`s. Existing
first-error precedence is preserved.

There is no fallback that widens the footprint, moves a pin, increases router
caps, or silently returns to the unbounded layout.

## 9. Determinism and proof obligations

- Ordered collections and `InstanceId` tie-breaks are retained throughout.
- Worker counts 1, 2, and 4 must produce identical proposal traces, terminal
  errors, metrics, and candidate fingerprints.
- Existing error priority and first-error ordering remain unchanged when no
  complete IO footprint is active.
- `relocate::anchors_of` remains the exhaustive definition of candidate-owned
  coordinates; the final footprint check uses that same walk.
- Union and certification are never skipped because a module was previously
  certified.
- The global seed placement revision is not bumped because that would change
  unpinned and partial-pin case fingerprints. Instead, the bounded-policy
  revision marker enters the case descriptor only when a complete IO footprint
  activates bounded mode. Deck assignments remain derived geometry and enter
  existing plan/candidate fingerprints through their Y poses.

## 10. Trial and acceptance

Implementation is staged so the expensive 3D routing work is attempted only
after the contracts are executable.

### Stage 0: bounded-layout preflight

- record that the committed pinned seven-segment is an uncertified routing
  refusal and preserve its exact terminal error as the legacy baseline;
- run today's legacy placer only through plan construction and record its
  `planned_macro_fill` from the literal macro envelopes;
- run the new bounded placer through plan-only mode, including dynamic spacing,
  lateral legalization, the monotonic vertical-trunk-band loop, and deck assignment;
- report usable forward/lateral spans, order-preserving deck count, cross-deck
  net count, and vertical-trunk-lane count without routing;
- do not start the large routing trial if geometry alone cannot place every
  macro and vertical trunk;
- add one minimal physical two-deck fixture proving that the first
  non-conflicting typed reservation translation prevents accidental vertical
  conductivity and still leaves the intended vertical trunk connected.

### Stage 1: footprint and clearance

- derive the footprint from a complete pin set;
- reject degenerate footprints;
- enforce the one-cell terminal clearance;
- prove every owned X/Z coordinate is contained;
- preserve all existing unpinned and partial-pin fingerprints;
- explicitly activate and measure complete-pin lateral folding, which the
  current seven-segment input directions do not trigger;
- unit-check complete, partial, degenerate, and turned-frame footprint
  projection without running routing.

### Stage 2: primitive deck folding

- add height-aware primitive envelopes and Y poses;
- certify a small flat fixture forced into at least two decks;
- prove the same fixture uses only the base deck and certifies when one deck
  fits;
- add a narrow two-pin fixture that still spans both axes and must use two
  decks.

### Stage 3: block deck folding and vertical trunks

- add block height and `dy` placement;
- certify a small hierarchical fixture forced into at least two decks; its
  pins keep the placement frame facing East so the existing fixed-orientation
  block contract is tested rather than rejected first as `BlockFrameTurned`;
- preserve compile-once/stamp-many and flat-union semantics.

### Stage 4: bounded seven-segment trial

For the pinned seven-segment fixture:

- keep the eleven literal pin definitions in `tests/build_circuit_pins.rs`
  byte-identical, then regenerate the intentionally changed metrics and hashes
  in `tests/fixtures/fragment_synth_shipping.json` and
  `tests/fixtures/fragment_synth_baseline.json`;
- footprint is exactly the inclusive min/max X/Z of its eleven caller cells;
- all sixteen input combinations remain correct;
- all eleven `(Anchor, toward)` pairs remain byte-identical;
- no owned coordinate escapes the footprint;
- if Stage 0 proves the seven-segment requires multiple decks, at least one
  primitive or block body occupies a deck above the base deck; otherwise a
  dedicated forced fixture must certify with at least two decks and contain a
  non-terminal macro-owned cell at `y >= base_ground + 4`, without changing the
  seven-segment pins merely to force height;
- generation terminates under the existing route/search caps;
- the bounded plan's `planned_macro_fill` strictly improves over Stage 0's
  legacy 2D plan and the final physical fill ratio is recorded as a new
  baseline;
- the report records blocks, observed settle, static delay, occupied volume,
  fill ratio, used height, deck count, and wall time against that result.

The existing hierarchical acceptance harness
(`every_hierarchical_circuit_certifies_through_module_floorplan`) proves the
bounded hierarchical candidate still certifies. Worker 1/2/4 identity is
checked by extending the existing certification tests
`certified_candidate_is_identical_at_one_two_and_four_workers` and
`exhaustive_cap_refusal_reports_the_same_lowest_mask_at_every_worker_count`
with the bounded success/refusal cases; no second determinism runner is added.

## 11. Rejected alternatives

- Module-only stacking does not cover flat netlists.
- Generate-flat-then-cut invalidates most routes and makes rerouting the first
  operation rather than a bounded consequence.
- Full 3D global placement search expands the search space before a legal seed
  exists and conflicts with generation-speed goals.
- A separately configured board outline duplicates information that complete
  IO already supplies.
- A configurable clearance radius is deferred until a concrete caller needs a
  value other than one.

# Every edge realisation adds, per circuit, per compile path

**Generated. Do not edit by hand.** Regenerate with

```
cargo test --release --lib -- --ignored compile::coupling::tests::regenerate_the_extras_record
```

and `the_realised_graph_of_every_circuit_is_what_is_recorded` in `src/compile/coupling/tests.rs` fails if this text and the compiler ever disagree -- in **either** direction. A new extra edge fails it; so does an extra edge that stops existing, which is how somebody closing one finds out this file needs updating.

What an extra edge is, and how it is extracted, lives in `src/compile/coupling.rs`'s module doc. In short: a **domain** is one electrical source plus everything the netlist says it drives, and an **extra edge** is a hop that leaves a domain's own territory and lands on a cell some other net owns, with nothing in the netlist joining the two. `contaminating N cell(s)` is how far that one hop then spreads by ordinary conduction; it is the size of the damage, not a second cause.

The extractor is differenced against the `Simulator` on a sweep of three-cell rigs (`docs/derived/realised-graph-extraction.md`), and the findings below are put back through the `Simulator` inside the circuit by `the_extra_edges_are_real_when_the_simulator_runs_the_circuit`, which prints, for each, an input vector where the contaminated net is genuinely low and its own wire reads 15 -- plus a one-block control that puts it back to 0.

* `and4`: 7 gates, 232 blocks, `compile()` takes the **Unified3d** path.
* `full_adder`: 22 gates, 1065 blocks, `compile()` takes the **Unified3d** path.
* `segment_a`: 46 gates, 6416 blocks, `compile()` takes the **Legacy** path.
* `seven_segment`: 84 gates, 16244 blocks, `compile()` takes the **Legacy** path.
* `verilog:and4`: 9 gates, 290 blocks, `compile()` takes the **Unified3d** path.
* `verilog:seven_segment`: 47 gates, 10088 blocks, `compile()` takes the **Legacy** path.

## Summary, computed from the runs below

* **41 extra edge(s) across all of it**, 37 of them in a world `compile` ships today.
* **41 of 41 are mechanism 3** -- component, strongly powered block, foreign dust. That is the mechanism both shipped bugs were, and here it is the *only* one that occurs.
* **41 of 41 cross through a cell that is some gate's own support block**, and in **41** of those the two nets are two *declared inputs of that same gate*. So the shape is one thing, over and over: a NOR's support is strongly powered by one input route's terminal, and re-drives another input route's own terminal dust on a different face of the same block -- and from there back up that route until a repeater stops it. The netlist joins those two nets nowhere; they merely arrive at the same gate.
* **0 of 2 foreign reads land on a cell no route owns.** A support block is owned by no route, so this counts the case `TorchMergeFailure::ForeignNetReachesSupport` already refuses -- an independent confirmation that `verify_torch_merge` is doing its half. Every other foreign read is on a cell some *other net* owns, which means it is downstream of a crossing listed above rather than a new coupling; what it adds is that the contamination does not merely sit on a wire, it reaches a diode that forwards it.
* `and4` and `verilog:and4` are clean on both paths. Every other circuit is not, on every path that can build it.

**NOT MEASURED here: whether any of this changes what a circuit computes.** Every one of these circuits passes its truth table today (`tests/reference_circuits.rs`, `tests/seven_segment.rs`, `tests/verilog_frontend.rs`). An extra edge is a fact about the realised graph; whether a given one is load-bearing depends on where the contaminated run's next repeater is and what branches off it before then, and that was not derived.

## `and4` / relaxation -- **SHIPS TODAY**

232 blocks, 11 domains, 116 cells reached, **0 extra edge(s)** contaminating 0 cell(s), 0 foreign read(s).

No edge the netlist did not ask for.

## `and4` / legacy

472 blocks, 11 domains, 450 cells reached, **0 extra edge(s)** contaminating 0 cell(s), 0 foreign read(s).

No edge the netlist did not ask for.

## `full_adder` / relaxation -- **SHIPS TODAY**

1065 blocks, 25 domains, 541 cells reached, **1 extra edge(s)** contaminating 7 cell(s), 0 foreign read(s).

```
  EXTRA EDGE   g1 at (46, 1, 124) -> g3 at (46, 1, 122) across (46, 1, 123), mechanism 3 (component -> block -> dust)
```

## `full_adder` / legacy

1784 blocks, 25 domains, 1797 cells reached, **4 extra edge(s)** contaminating 63 cell(s), 1 foreign read(s).

```
  EXTRA EDGE   a at (27, 1, 112) -> b at (29, 1, 112) across (28, 1, 112), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   cin at (41, 1, 49) -> g13 at (39, 1, 49) across (40, 1, 49), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g3 at (29, 1, 38) -> g14 at (27, 1, 38) across (28, 1, 38), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g3 at (41, 1, 97) -> g0 at (39, 1, 97) across (40, 1, 97), mechanism 3 (component -> block -> dust)
  FOREIGN READ a reaches (38, 1, 114) (owned by b), read by a repeater at (38, 1, 113), mechanism - (dust -> diode)
```

## `segment_a` / relaxation

This path cannot build this circuit at all: relaxation places it and then fails to route, which is why `compile` falls back.

## `segment_a` / legacy -- **SHIPS TODAY**

6416 blocks, 50 domains, 6390 cells reached, **9 extra edge(s)** contaminating 74 cell(s), 0 foreign read(s).

```
  EXTRA EDGE   g16 at (69, 1, 42) -> g10 at (67, 1, 42) across (68, 1, 42), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g16 at (69, 1, 42) -> g19 at (68, 1, 43) across (68, 1, 42), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g2 at (14, 1, 93) -> g1 at (15, 1, 92) across (14, 1, 92), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g28 at (55, 1, 42) -> g25 at (53, 1, 42) across (54, 1, 42), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g28 at (55, 1, 42) -> g31 at (54, 1, 43) across (54, 1, 42), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g3 at (71, 1, 66) -> g18 at (69, 1, 66) across (70, 1, 66), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g4 at (13, 1, 92) -> g1 at (15, 1, 92) across (14, 1, 92), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g6 at (140, 1, 93) -> g0 at (139, 1, 92) across (140, 1, 92), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g6 at (140, 1, 93) -> g5 at (141, 1, 92) across (140, 1, 92), mechanism 3 (component -> block -> dust)
```

## `seven_segment` / relaxation

This path cannot build this circuit at all: relaxation places it and then fails to route, which is why `compile` falls back.

## `seven_segment` / legacy -- **SHIPS TODAY**

16244 blocks, 88 domains, 16255 cells reached, **24 extra edge(s)** contaminating 187 cell(s), 1 foreign read(s).

```
  EXTRA EDGE   g16 at (69, 1, 71) -> g10 at (67, 1, 71) across (68, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g16 at (69, 1, 71) -> g19 at (68, 1, 72) across (68, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g19 at (110, 1, 72) -> g10 at (109, 1, 71) across (110, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g19 at (110, 1, 72) -> g13 at (111, 1, 71) across (110, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g19 at (152, 1, 72) -> g10 at (151, 1, 71) across (152, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g19 at (152, 1, 72) -> g16 at (153, 1, 71) across (152, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g2 at (84, 1, 157) -> g4 at (83, 1, 156) across (84, 1, 156), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g2 at (84, 1, 157) -> g5 at (85, 1, 156) across (84, 1, 156), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g25 at (165, 1, 71) -> g28 at (167, 1, 71) across (166, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g25 at (165, 1, 71) -> g34 at (166, 1, 72) across (166, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g25 at (193, 1, 71) -> g28 at (195, 1, 71) across (194, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g25 at (193, 1, 71) -> g34 at (194, 1, 72) across (194, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g28 at (138, 1, 72) -> g22 at (137, 1, 71) across (138, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g28 at (138, 1, 72) -> g25 at (139, 1, 71) across (138, 1, 71), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g3 at (155, 1, 130) -> g36 at (153, 1, 130) across (154, 1, 130), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g4 at (139, 1, 156) -> g1 at (141, 1, 156) across (140, 1, 156), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g4 at (139, 1, 156) -> g2 at (140, 1, 157) across (140, 1, 156), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g5 at (57, 1, 156) -> g4 at (55, 1, 156) across (56, 1, 156), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g5 at (57, 1, 156) -> g6 at (56, 1, 157) across (56, 1, 156), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g5 at (155, 1, 156) -> g0 at (153, 1, 156) across (154, 1, 156), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g57 at (111, 1, 16) -> g55 at (109, 1, 16) across (110, 1, 16), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g57 at (111, 1, 16) -> g59 at (110, 1, 17) across (110, 1, 16), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g6 at (154, 1, 157) -> g0 at (153, 1, 156) across (154, 1, 156), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   g9 at (127, 1, 130) -> g33 at (125, 1, 130) across (126, 1, 130), mechanism 3 (component -> block -> dust)
  FOREIGN READ g28 reaches (131, 3, 77) (owned by g25), read by a repeater at (130, 3, 77), mechanism - (dust -> diode)
```

## `verilog:and4` / relaxation -- **SHIPS TODAY**

290 blocks, 13 domains, 145 cells reached, **0 extra edge(s)** contaminating 0 cell(s), 0 foreign read(s).

No edge the netlist did not ask for.

## `verilog:and4` / legacy

480 blocks, 13 domains, 454 cells reached, **0 extra edge(s)** contaminating 0 cell(s), 0 foreign read(s).

No edge the netlist did not ask for.

## `verilog:seven_segment` / relaxation

This path cannot build this circuit at all: relaxation places it and then fails to route, which is why `compile` falls back.

## `verilog:seven_segment` / legacy -- **SHIPS TODAY**

10088 blocks, 47 domains, 10558 cells reached, **3 extra edge(s)** contaminating 13 cell(s), 0 foreign read(s).

```
  EXTRA EDGE   n13 at (67, 1, 103) -> g1 at (69, 1, 103) across (68, 1, 103), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   n13 at (81, 1, 103) -> g14 at (83, 1, 103) across (82, 1, 103), mechanism 3 (component -> block -> dust)
  EXTRA EDGE   n4 at (55, 1, 171) -> n2 at (53, 1, 171) across (54, 1, 171), mechanism 3 (component -> block -> dust)
```

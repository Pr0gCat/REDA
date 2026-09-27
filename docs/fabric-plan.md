# Final plan: Lid Fabric with planned paths

Worktree: `/Users/seith/Desktop/REDA/.claude/worktrees/caveman-full-3b51d3`. I spot-checked the load-bearing facts against the code:
- `anchor_is_free_for_typed` refuses only a Conductor or MandatoryAir below the cell (`routing.rs:2135-2144`). `commit_routed` refuses a floor over any KeepOut the trunk cannot yield (`routing.rs:491-521`). So a floor over a foreign KeepOut passes the search and then fails at commit (LFL critique F2 is real).
- `is_layout_dependent` excludes Certification (`packed_node.rs:1316-1329`).
- The grain loop halves on PinnedSpaceShort / PinnedRegionTooSmall (`packed_recursive.rs:280-297`).
- Unpinned roots have no fallback (`recursive.rs:756-767`). Pinned roots fall back to allocating on Err (`recursive.rs:787-806`).
- `exact_refreshes` is at `routing.rs:3630`. Search is called per branch at `routing.rs:1486-1503`.
- The six gate tests are at `tests/fragment_synth_acceptance.rs:196-203`. The gate is `new <= old` on ticks and on blocks (`benchmark.rs:484-489`).
- `PORTAL_PITCH` = 3 (`allocation.rs:47`).

---

## 1. Chosen architecture

A **second, independent producer** ("Fabric"). The current lanes producer and the allocating producer stay byte-identical, and production selection is keep-better (§2, increments 12-13).

### 1.1 Shape: flat, one fabric node
- The Fabric root is **flat**, not a binary recursion. The netlist is partitioned to leaves with the existing pinned grain machinery: `synthesise_packed_recursive_pinned` / `build_leaf_finer` (`packed_recursive.rs:230-355`), generalised to `pins: Option<_>`. All leaves are packed under **one** fabric node.
- Why flat: it removes the ~+11 height per recursion level that every critique raised, and the O(depth²) climb latency. It also removes the root-pad contract for mixed-mode children, since there are no children-of-children.
- Height of any Fabric circuit = `lid + 9`, where `lid` = the tallest leaf halo top. It depends neither on trunk count nor on netlist size. The only exception is pinned floors (§1.6), where height grows with room-area shortfall.
- **Common y frame.** Every child gets the same translation.y = `-min_child halo.min.y`, so `lid` and every climb height are fixed before placement. This fixes the "packing moves y" fatal: `packing.rs:1291-1296`, the bridge y-alignment around 1226, and normalisation at 905-910 are not used in Fabric mode.

### 1.2 Layers and lattice
| Layer | y (dust) | Floor | Runs along | Carries |
|---|---|---|---|---|
| E | lid+2 | lid+1 | x | pad tops, then escapes along the terminal's own row |
| Z | lid+5 | lid+4 | z | one Z-run per terminal on its private column |
| X | lid+8 | lid+7 | x | trunk spines |

- The canvas top is lid+9. The shell sits at lid+10 (`parent.rs:2098-2109`).
- E sits at lid+2, not lid+1, so that E floors never sit on halo, guard or root KeepOut. Those reach only to `guard_top = lid` (`parent.rs:683`).
- **Separation rule:** conductors of different trunks are at L1 ≥ 3. This is the leaf halo standard (`leaf.rs:526-541`); it lies outside `keep_out_typed` (`routing.rs:2181-2195`) and outside the two-hop ball (`parent.rs:2069`).
- Layers cross only at dy = 3. Tracks on one layer are pitch 3.
- **Via catalogue**, each shape a 3-level stair of 3 cells:
  - V1: E→Z along x.
  - V2: E→Z along z.
  - V3: Z→X along z.
  - V4: Z→X along x. V1+V4 in line gives a straight 6-level stair when the spine row equals the terminal row.

  A fixed table on `(sign(R−z_t), |R−z_t| ≥ 3)` picks the via pair. No spine row is ever forbidden, which removes the "`|R−z_t|≥3` excludes all rows" problem. Every shape is proven in kernel K2 (§3).

### 1.3 Terminal pads: climbs of any height, dust only
- A pad is **part of the trunk's own route path**, not a parent-placed riser:
  - Anchor `A`, then the unchanged forced runway `A, A+f, A+2f` (`PACKED_TERMINAL_RUNWAYS`, `parent.rs:139-146`).
  - Mouth `M = A+3f` and `M+f`, both flat.
  - A straight dust stair along `f` in the terminal's **own row**, with a 3-cell flat landing every 12 levels.
  - 3 flat cells at E.
  - Sinks use the mirror image and end in the existing RepeaterIntoSupport into the handover (`routing.rs:1751-1768`).
- **Refresh.** A landing's middle cell satisfies `straight_flat_repeater_fits` (`routing.rs:3414`). So every 14-cell window holds a legal refresh cell, and the router's own `realise_branch_cells` (LatestLegalCell) places the repeaters. Height is unbounded, and horizontal length is `h + 3·ceil(h/12)`.
- **Isolation.** Rows on one face are ≥ 3 apart (PORTAL_PITCH), so pads never interact.
- **What this removes.** No torch parts, no `realise_riser`, no envelopes, no new reservation owners and no Endpoint-KeepOut release hazards. This fixes the PAD_ENVELOPE-covers-anchor fatal and the lift-slot-collision fatals. The planner never runs a search, so RISER_NODES cannot run out. `riser.rs` stays test-only; torch towers are a later compaction step, gated on the probes in §3.

### 1.4 Routing: planned paths, zero A*
- A new router entry `PhysicalRouter::route_planned(request, &PlannedRoute)` takes, per branch, the exact cell path. `PlannedRoute { branches: Vec<Vec<Anchor>>, min_reads: BTreeMap<Anchor,u8>, no_refresh: BTreeSet<Anchor> }`.
- Inside `route_ordered_attempt`, the `search_path` call (`routing.rs:1486-1503`) becomes `validate_planned(...)`. Everything after it is unchanged: reserve, shared-prefix strength, realise, terminal choice, `certify_path`, ring check.
- `validate_planned` replays, step by step, the same predicates the search applies:
  - `anchor_is_free_for_typed` (`routing.rs:2116`);
  - `staircase_cell_is_blocked` (`routing.rs:2155`);
  - `self_obstructs_batch` (`routing.rs:2044`);
  - the forced runway chains;
  - `route_step_is_legal` (`routing.rs:3289`);
  - one extra check in planned mode only: **a floor over a KeepOut this trunk cannot yield**, the same rule `commit_routed` applies (fixes F2).

  Any mismatch is a typed `RouterFailure::PlannedStepRefused{index, reason}`, never an Overlap at commit. A planner that drifts from the router therefore fails a unit test.
- **A* limits.** They are untouched; Fabric performs 0 expansions, which is stronger than "not raised".
- **Fanout junction fix.** `exact_refreshes` (`routing.rs:3630`) gains per-index floors:
  - `reads(i) >= min_reads[i].unwrap_or(1)` instead of `>= 1`;
  - `no_refresh` cells are added to `ineligible`.

  The planner sets each junction's floor to 1 + the distance to the first eligible cell on every branch leaving it, and marks junction cells `no_refresh`. This fixes "a repeater lands on a junction or the junction is too weak" for fanouts inside an exact path.
- **Sink order.** Sinks are routed outward along the spine from the source junction, sorted by `(|c_k−c_s|, side, endpoint)`. Every branch's path[0] is an already-laid cell, which the validator asserts.

### 1.5 Space budget: reserved at placement, sized by demand
- **Comb per child face**, computed before placement from translation-invariant data (interfaces, `interface_route_direction` `leaf.rs:370`, `uses`) plus the common `lid`:
  `depth(face) = 2 + max_t ramp_len(E − A_t.y) + 3 + 3·m_face + 2`.
  - `m_face` = trunk ends on the face; root ports take a row but no column.
  - The last +2 keeps two adjacent combs' conductors at L1 ≥ 3.
  - Depth grows with **terminals per face and climb height**, never with trunk count elsewhere.
- **Inflated clones.** Each child is cloned with `halo ∪ comb boxes`, where a comb box covers the face's rows ±1, from halo bottom to E+1. Packing sees the clones. The parent reserves the **original** halos (`reserve_packed_children`, `parent.rs:2142`), so comb cells are free for routes.
- **Strip layout (the guaranteed candidate).** Children go in `dataflow_order` (`packed_node.rs:1010`) along x, cursor = previous inflated halo max.x + 1, so combs are x-disjoint.
  - Private columns in each comb: slot k = pad top + 3 + 3k, ordered by row z. No Z-run shares a column, E-escapes stay inside their own comb row, and pads stay inside their comb. That is conflict-free by construction.
  - Spines: left-edge assignment over the intervals `[min col, max col]`, widened ±3 for a V4 via, with a gap of 3. Rows used = interval density, which is optimal. Rows lie over the strip and overflow into z-overhang: the canvas grows in z, never in y.
  - Node-built input-fanout sources and exports (`packed_node.rs:621-749`) get a "port comb" at the strip's east end with the same geometry.
- **Result:** unpinned Fabric completes for any netlist. Area grows with demand; height never does.
- **Compact candidates** (increment 14, optional). Candidates from the existing packer on the inflated clones are tried first. Column conflicts are checked as `(x, z-interval)` items. A conflict is a layout-dependent `FabricCapacity`, and the strip remains the last candidate.

### 1.6 Pinned mode
- **Portal pads.** Every pin's node-built terminal (`parent.rs:546-643`, `753-779`) gets a pad inward in its own lane.
  - E/W pins: the pad runs along x and then escapes on its own row.
  - N/S pins: the pad runs along z and climbs V2 into a **fixed column item** `(x_p, z-interval)`, not a whole column. This fixes three N/S pins on x = 76 breaking Hall's condition.
- **Placement.** `PinnedRoom::place` (`packed_node.rs:1462-1500`) takes a per-pin gap function. The Fabric gap is `pad_depth(E − pin.y) + 2`: linear in the pin's climb and independent of lane count. The lanes gap `PINNED_MARGIN.max(top − pin.y + 7)` is kept for the lanes producer.
- **Layout.** The strip is placed inside the room. Where it does not fit, it extends toward `PinnedRoom` open, unfed sides (`packed_node.rs:1434-1449`); that is the user's rule.
- **Floors (last resort, closed rooms only).** Floor count `F = ceil(strip_len / room_extent)` is computed before placement. Strip segments stack on floors of height `lid+2`. Pads climb through reserved comb shafts to one fabric above the top floor, so pad depth is sized for `E_top`. Height then grows with area shortfall, not trunk count.
- **Grain loop.** `FabricCapacity` never halves the grain (`packed_recursive.rs:280-297`), because more leaves means more demand.

### 1.7 Determinism
- Everything is a pure function of the netlist, chunk ids, interface ids, signals and pins, iterated through BTreeMap/BTreeSet in fixed order:
  - dataflow order;
  - slots by `(z, endpoint)`;
  - left-edge sorted by `(lo, hi, signal)`;
  - trunks in signal order (`parent.rs:644-651`);
  - sinks as in §1.4.
- Leaves are built with `run_indexed` as today. The planner and router run serially.
- Selection depends only on the certified results.

### 1.8 Certification
- `certify_root_world` runs on every Fabric node world (`packed_node.rs:942-951`), and the harnessed root is re-certified (`packed_recursive.rs:580-591`).
- In Fabric mode only, Certification is layout-dependent, bounded by the candidate list, so a coupling bug costs one candidate, not the producer.
- After commit, the two-hop isolation check (`parent.rs:1046-1102`, `two_hop_ball` at 2069) runs on **every** Fabric trunk. A failure is a typed `FabricIsolation`.
- Nothing is shipped uncertified, and keep-better compares only certified products.

---

## 2. Ordered increments

Rollback rule for all of them: until increment 13, production calls are unchanged. Each increment is revertable alone, and the lanes and allocating producers are never edited, except the byte-identical refactor in increment 6.

**1. Physics kernels (no production code).**
- Files: new `tests/fabric_kernels.rs`, reusing the block builders and the `Simulator` of `tests/simulator_circuits.rs`.
- Tests: K1–K6 from §3, e.g. `k2_every_via_shape_ignores_neighbour_tracks_at_l1_three`, `k4_dust_ramp_carries_up_and_down_to_128_levels`.
- Assertion: each lever toggle flips only its own output lamp or dust, in both polarities.
- Acceptance: all kernels pass.
- Rollback: test-only.

**2. Planned-path router entry.**
- Files: `routing.rs`:
  - trait method `route_planned` on `PhysicalRouter` (next to `routing.rs:888`); the default returns `Err(Unsupported)`;
  - the Durable impl, via `route_with_local_policy` (1393) with `PathSource::{Search, Planned}`;
  - the `search_path` call site in `route_ordered_attempt` (1486-1503) switches on it;
  - new `validate_planned`;
  - per-index floors and `no_refresh` in `exact_refreshes` (3630) and `realise_branch_cells` (3491).
- Tests:
  - `planned_paths_replay_searched_routes_byte_identically`: feed the branch paths of existing router fixtures back in and assert `RealisedRouteTree` is equal;
  - `a_planned_step_the_search_refuses_is_refused_typed`, covering 5 cases: a foreign conductor in the ring, a floor over a foreign KeepOut, self-obstruction, an illegal step, path[0] not laid;
  - `a_junction_floor_moves_the_refresh_upstream`;
  - `no_refresh_cells_never_hold_a_repeater`.
- Acceptance: `cargo test --release --lib routing` is green.
- Rollback: the Search path is unchanged. Default floors of 1 give the same `exact_refreshes` output, which an existing-fixture byte test asserts.

**3. Fabric geometry.**
- Files: new `fragment_synth/fabric.rs` with `Layers::from_lid`, `ramp_len`, `pad_path(anchor, f, from_y, to_y, flow)` and `via_cells(kind, at, dir)`; `mod.rs`.
- Tests:
  - `every_fourteen_cell_window_of_a_pad_holds_a_refresh_slot` for h in 0..=256;
  - `pad_path_stays_in_its_own_row`;
  - `layers_are_lid_plus_two_five_eight_top_nine`;
  - re-running K4 on `pad_path` output for h ∈ {1, 12, 13, 40, 64, 128}.
- Rollback: the module is unused.

**4. Fabric planner (pure).**
- Files: `fabric.rs`: `plan_fabric(terminals, trunks, combs, lid) -> FabricPlan`, which does slot assignment, left-edge rows, via choice, branch paths, sink order and junction floors.
- Tests:
  - `height_is_lid_plus_nine_for_1_to_256_trunks`;
  - `foreign_conductors_are_at_least_l1_three_apart` (brute force over all planned cells);
  - `left_edge_rows_equal_interval_density`;
  - `plan_is_independent_of_input_order` (shuffled inputs give an equal plan);
  - `every_unrefreshable_run_fits_max_dust_run`;
  - `every_branch_starts_on_a_laid_cell`.
- Rollback: unused.

**5. Demand model and strip layout.**
- Files: `fabric.rs`: `face_demand(artifact, uses, lid)`, `inflate(&FreeLeafArtifact, &Demand) -> FreeLeafArtifact`, `strip_layout(children, order) -> Vec<Translation>` with the common y shift.
- Tests:
  - `inflated_halo_contains_every_pad_escape_and_column`;
  - `strip_combs_are_x_disjoint_and_two_cells_apart`;
  - `comb_depth_grows_with_face_terminals_and_climb_not_trunk_count`.
- Rollback: unused.

**6. Parent: extract resolution, then add Fabric routing.**
- Files: `parent.rs`.
  - (a) Move `parent.rs:439-715` into `resolve_packed_trunks(...) -> Resolved { trunks, roots, canvas, guard_top }`, and have `route_packed_trunks_with_inputs` (429) call it.
  - (b) New `route_packed_trunks_fabric(packed, requests, inputs, outputs, plan, router)`:
    - reuses `expanded_packed_world` (2111), children, handovers, access, guard and plain-halo reservations (746-816), with lane-None egress (mouth only);
    - reserves root egress (873-903) and the shell;
    - skips `band_plan`, `shared_lanes` and `coil_egress`;
    - sets the canvas top to lid+9 plus the plan's overhang;
    - per trunk, calls `route_planned` with `PACKED_TERMINAL_RUNWAYS` unchanged;
    - commits as at 1014-1045;
    - runs the two-hop check on every trunk.
- Tests:
  - every existing `parent.rs` test green, including `packed_free_leaf_chain_routes_its_boundary_and_simulates_both_cases` (2846) and `a_trunk_from_a_shorter_leaf_climbs_past_its_taller_sibling` (3349), which proves (a) is byte-identical;
  - new `fabric_node_routes_n_parallel_trunks_at_lid_plus_nine_and_certifies` for N ∈ {1, 2, 4, 8, 16, 32}: canvas.max.y − lid == 9 for every N, and all route cells have y ≤ lid+8;
  - `fabric_fanout_eight_certifies`;
  - `a_taller_sibling_forces_a_forty_level_pad_that_certifies`.
- Rollback: the lanes function body is unchanged apart from the extraction.

**7. Fabric node.**
- Files: `packed_node.rs`: new `synthesise_fabric_node(children, room: Option<&PinnedRoom>, ...)`.
  - Candidates: `[strip]` now; compact candidates come in increment 14.
  - Build the requests as in the build closure (532-797), with node-built ports in the port comb.
  - Call `route_packed_trunks_fabric`, then `validate_packed_root_boundary` (1776), then certify **inside** the candidate loop.
  - New errors `FabricCapacity` and `FabricIsolation`; Certification is layout-dependent in this function only.
- Tests:
  - `fabric_node_is_worker_invariant` (fingerprints equal at 1 and 8 workers);
  - `a_fabric_certification_failure_tries_the_next_candidate` (a stub router that corrupts the first candidate);
  - the existing `two_trunks_into_one_leaf_hold_separate_lanes_and_never_touch` (2955) and `shared_input_fanout_is_parent_owned_certified_nested_and_worker_invariant` (2025) unchanged.
- Rollback: new function only.

**8. Flat Fabric producer.**
- Files: `packed_recursive.rs`: generalise the pinned grain partition and leaf build (230-355) into `build_flat_leaves(lowered, root, grain, ...)`, used by both the pinned path and the new `synthesise_fabric_recursive(lowered, root, pins: Option, ...)`. The product is adapted with `adapt_packed_root` (491).
- Tests:
  - `fabric_root_certifies` for and4, verilog:and4, full_adder, segment_a and seven_segment (certify Ok; print ticks and blocks);
  - `fabric_root_is_byte_identical_at_one_and_eight_workers` (assert `peak_workers > 1`);
  - the existing `a_recursive_packed_root_is_worker_invariant_and_three_levels_deep` (1901) green.
- Rollback: the pinned path is byte-identical; a pinned fingerprint test asserts it.

**9. Stress and sweep.**
- Files: new `tests/fabric_stress.rs`, with netlists generated through `src/circuits/netlist_builder.rs`.
- Tests:
  - `height_is_independent_of_trunk_count`: N ∈ {1..64} buffers between two leaves; the max y of the Fabric world − lid == 9 for every N;
  - `random_dags_certify_through_the_fabric`: 40 seeded DAGs, fanout ≤ 8, 20-60 trunks; each certifies and is deterministic on rerun;
  - `a_two_hundred_level_pad_certifies` (with the lid raised by a tall synthetic child).
- Rollback: test-only.

**10. Pinned Fabric.**
- Files:
  - `packed_node.rs`: `PinnedRoom::place` (1462) takes a gap function, with the lanes gap passed unchanged; open-side overhang; room-clipped strip.
  - `fabric.rs`: fixed column items for pins; `(x, z-interval)` conflict checks.
  - `packed_recursive.rs` (280-297): `FabricCapacity` is not a halving error.
- Tests:
  - `pinned_fabric_stays_inside_its_room_or_open_sides` (every block inside the pin rectangle or past an open, unfed side);
  - `three_north_south_pins_on_one_x_share_the_column_by_interval`;
  - `pinned_decoder_fabric_certifies_at_lid_plus_nine` on `viewer/baked/verilog_seven_segment.synth.pins.json`;
  - lanes pinned fingerprints unchanged.
- Rollback: the lanes gap function is identical.

**11. Pinned floors (closed rooms only).**
- Files: `fabric.rs` (F and floor segments, shafts in the combs); `packed_node.rs`.
- Tests:
  - `a_closed_room_too_small_for_one_floor_folds_into_floors_and_certifies`;
  - `floor_count_depends_on_area_not_trunk_count`: same room, 8 against 32 trunks at equal gate area, equal F.
- Rollback: reached only when there is no open side.

**12. Keep-better selection behind a flag, default Off.**
- Files: `config.rs`: `SearchConfig.fabric: FabricSelection { Off, KeepBetter }`. `recursive.rs` at 756-767 (unpinned) and 787-806 (pinned):
  - if the current producer returns Err, try Fabric, then allocating as today (pinned);
  - if the current producer returns Ok, build Fabric as well and ship it **only if it dominates**: it certifies, its ticks are ≤ and its blocks are ≤, both measured on the adapted circuit exactly as `benchmark.rs:478-489` measures them. Otherwise ship the current product unchanged.
  - Dominance, not a lexicographic key, is what guarantees no gate can flip.
- Tests:
  - `keep_better_never_ships_a_dominated_product`;
  - `selection_is_worker_invariant`;
  - `flag_off_is_byte_identical` (all six acceptance fingerprints equal to Off);
  - an acceptance run with the flag On via a test-only config.
- Rollback: the flag is Off.

**13. Final switch.**
- Default becomes `KeepBetter`.
- Run all six `budget_zero_*`, `./check.sh`, and `seven_segment_composition_is_the_same_at_one_worker_and_many -- --ignored` (`recursive.rs:2953`).
- Update the docs at `parent.rs:16-19`, `packed_recursive.rs:11-16` and `recursive.rs:772-774` (the ponytail note is now implemented).
- Rollback: flip the default back.

**14. (Optional, measured) Compact Fabric candidates.**
- Packer candidates on the inflated clones, tried before the strip. Pick among Fabric candidates by dominance.
- Test: `compact_fabric_never_worse_than_strip`.
- Do this only if increment 8 shows strip latency losing keep-better on seven_segment or segment_a.

---

## 3. Physics that must be proven in the simulator first (increment 1)

Every rig is hand-laid with stone, dust, repeaters, levers and a lamp or dust probe, and toggles each driver independently.

| # | Fact | Rig |
|---|---|---|
| K1 | Parallel foreign tracks at L1 = 3 do not couple: dust–dust, dust–repeater, repeater–repeater, repeaters facing along or against | Two 20-cell lines, 3 apart in z; separately 3 apart in y−1/x+2 combinations; toggle each; assert the other's probe is constant, in both polarities |
| K2 | Every via shape V1–V4, and V1+V4 in line, carries, and a foreign track at L1 = 3 on every side (including the case where a weakly powered stone floor is diagonal to foreign dust) is unaffected | One via with 4 neighbour tracks (column c±3 at Z, rows R±3 at X, rows z±3 at E); toggle all 5 in a Gray-code sequence |
| K3 | dy = 3 crossings (dust over dust, stone floor at dy 2, air at dy 1) do not interact | E×Z and Z×X crosses; toggle both |
| K4 | Dust stair in a single row with a landing every 12 levels carries up and down for h ∈ {1, 5, 12, 13, 24, 40, 64, 128} from a 15-strength handover repeater, and the down case drives a RepeaterIntoSupport. The simulator reads ≥ the router's model strength at every cell | Build with `pad_path` output in increment 3; hand-laid in increment 1 |
| K4b | Two adjacent pads at pitch 3 with staggered landings do not couple | Pair of K4 ramps with different h |
| K5 | A spine T-junction with branch descent (V3 down) carries when the junction reads ≥ its floor, and fails when it reads less | Fanout 3 at one junction strength floor; the failure case proves the floor is needed |
| K6 | A route floor on air directly above a foreign flat track at dy = 2 is inert, which confirms that dy = 2 is never needed | Documents that the fabric does not rely on dy = 2 |

- The conformance probes needed are already present: `dust_climbs_when_open`, `dust_descends_when_open`, `dust_powers_the_block_below_it_but_never_the_block_above`, `dust_shape_decides_which_block_it_powers` and the strength probes (`conformance/results/1.20.1.json`).
- One vanilla fact is not probed: "dust does not read a weakly powered block". Add it to `conformance/probes.py` as `dust_ignores_weakly_powered_neighbour_block`; K2 relies on it.
- Torch towers and cascades are **not** used. If they are later used for shaft compaction, three probes come first: a torch tower where dust on a torch-powered block reads 15, a wall torch driving the dust below it, and "a lit torch does not power the block beside it" (the simulator deviates here, `taxonomy.rs:374-386`).

---

## 4. Definition of done (measurable)

1. **Height is independent of trunks.** `height_is_independent_of_trunk_count` shows Fabric world max y − lid == 9 for N = 1..64, and `height_is_lid_plus_nine_for_1_to_256_trunks` passes at planner level. Lanes grows on the same N, which is recorded to show the contrast.
2. **Unbounded climbs.** K4 at 128 levels, `every_fourteen_cell_window_of_a_pad_holds_a_refresh_slot` for h ≤ 256, and `a_two_hundred_level_pad_certifies`.
3. **Space reserved by demand.** `comb_depth_grows_with_face_terminals_and_climb_not_trunk_count`, and the strip plus left-edge rows always plan: 40/40 random DAGs certify.
4. **Real failures.** Unpinned seven_segment certifies through Fabric (`fabric_root_certifies`). The pinned decoder certifies inside its room or open sides at lid+9, against 77 today. Ticks and blocks for both are reported against 98/16,244 and the pinned baseline.
5. **No regression.** All six `budget_zero_*` are no worse than today: every case passing today still passes, and with the flag Off every fingerprint is byte-identical. segment_a (108/6,613 today against 72/6,416) is reported honestly: Fabric ships only if it dominates.
6. **Determinism.** Fabric fingerprints for seven_segment and the pinned decoder are identical at 1 and 8 workers (`peak_workers > 1`), and selection is worker-invariant.
7. **Hard constraints.**
   - `config.rs:27-28` is unchanged, and Fabric routes report 0 search expansions.
   - `certify_root_world` gates every shipped product: a test cuts a Fabric trunk and asserts refusal.
   - No edits under `tests/fixtures/*baseline*`.
   - Lanes and allocating producers are still callable, with their `parent.rs`, `packed_node.rs` and `packed_recursive.rs` tests green.

Skipped on purpose:
- torch-part pads (dust ramps suffice; add after the probes if pad depth hurts quality);
- compact candidates (increment 14 only if measurements demand it);
- per-node mode mixing (flat Fabric makes it unnecessary).
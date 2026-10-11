# Gap 3 cut-recovery re-qualification (2026-10-11)

Re-applied the recovered owned cut-recovery feature (RECOVERY.md, `recovered-owned.patch`,
never qualified) onto the current tree at HEAD `4935eca`, branch
`codex/circuit-generation-optimization`. This is the "new hashes and build results" that
RECOVERY.md line 9 requires before any adoption claim.

## Files (11)

Modified (tracked): `src/compile/mod.rs`, `src/compile/selection.rs`,
`src/compile/fragment_synth/{api,leaf,mod,packed_node,packed_recursive,partition,recursive,topology}.rs`.
New: `src/compile/fragment_synth/physical_sites.rs` (typed `GateInventory`).

The patch was adapted to the current tree, not applied verbatim:
- `physical_sites` inventory is built incrementally through `GateMetadata.inventory`
  (leaf `from_candidate` + `absorb`, packed_node `add_route`/`add_buffer`) and read back in
  `adapt_packed_root`; the current packed path has no `ExpandedPhysicalCandidate` on the
  product, so the patch's root-candidate source does not exist here.
- `api.rs` keeps the current `(4..=6)` input range (the patch base had `(4..=5)`).
- `producer_revision` is NOT bumped: `MinimumCrossing` is accepted only when it strictly
  dominates AND passes the full-site admission, so the default output is byte-identical.
  Bumping would gratuitously repin the baseline fingerprint.

## Build / test evidence (current source)

- `cargo check -p reda`: clean (only pre-existing warnings: `SeedRoutingFailure`, `mut`,
  `rank_zero`, `as_str`).
- New unit tests, all pass:
  - `partition::tests::minimum_crossing_cuts_follow_dependencies_and_preserve_all_interfaces`
  - `partition::tests::minimum_crossing_cuts_match_exhaustive_cut_search`
  - `physical_sites::tests::certified_leaf_and_composition_inventory_match_every_actual_gate_truth`
- No regression: `cargo test --release --lib fragment_synth` 444 passed / 0 failed / 8 ignored;
  `--lib selection` 14 passed / 0 failed.

## Whole-circuit >32-gate probe (the probe RECOVERY.md never ran)

Throwaway probe (since removed): six independent six-NOR cones = 36 gates, 6 inputs, 6
outputs, no sequential gates, no pins — the smallest shape that admits `MinimumCrossing`
(`gates > TERMINAL_GATES`). `compile_fragment_synth`, `Evaluations(8)`.

| proposal | terminal | accepted | settle | non_air | volume | delay |
|---|---|---|---|---|---|---|
| PackedDense (idx 0) | Accepted | yes | 32 | 1134 | 6532 | 32 |
| PackedStandard (idx 1) | NoImprovement | no | 40 | 1350 | 11220 | 40 |
| BalancedMatrix (idx 2) | NoImprovement | no | 52 | 16292 | 87024 | 48 |
| **MinimumCrossing (idx 3)** | NoImprovement | no | 32 | 1134 | **12874** | 32 |

`MinimumCrossing` fires end-to-end (compiles, certifies). It ties the incumbent on
settle/blocks/delay but is worse on `occupied_volume` (12874 > 6532), so
`Quality::dominates` is false and it is correctly rejected. The default output is unchanged.

## Status

The cut-recovery tool is re-applied, compiles, is unit-tested, causes no regression, is
reachable end-to-end, and is correctly gated (rejected unless it strictly dominates on every
retained history). It is safe: it never changes the default output on the probe circuit.

This does NOT close gap 3. The gap-3 research question — finding circuits where cut-based
composition genuinely beats the packed path while preserving original physical-cone paths and
arrival balance — is unchanged: on this circuit the packed path is already optimal and the
cut proposal adds volume without benefit. No QoR improvement is demonstrated; no product
adoption is claimed.

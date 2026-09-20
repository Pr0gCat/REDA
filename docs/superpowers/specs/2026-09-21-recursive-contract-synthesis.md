# Recursive contract synthesis

**Status:** architecture decision; the performance branch proves the first
vertical slice before replacing the certified seed.

This document supersedes the scale strategy in
`2026-08-11-unified-3d-planner.md`, `2026-08-15-routing-at-scale.md`, and
`2026-08-28-failure-directed-generation.md`. Those documents remain useful
measurement history, but their flat whole-circuit retry loops are not the
target architecture.

## Decision

REDA generates a large circuit by recursively dividing its logical graph into
chunks. A parent assigns each child a physical region and an interface
contract. Children generate and certify their private worlds independently;
the parent owns routes between child interfaces and certifies the composition.
The same operation applies at every scale:

```text
solve(logic, contract)
  small enough -> place, detail-route, certify
  otherwise    -> partition
                  assign child contracts and parent trunks
                  solve children independently
                  compose, stitch, certify
```

The global router is therefore a **coordinator**, not another block-level
router. It allocates coarse capacity, portal windows, delay budget, and trunk
ownership. The existing physical router remains the leaf detailed router.

## Branch goal

The first goal is one deterministic two-child vertical slice that proves:

1. a circuit can be partitioned without changing its logic;
2. each child can be generated against a parent-owned boundary contract;
3. the parent can stitch the children and recover one ordinary
   `CertifiedCandidate`;
4. sequential and parallel child execution produce the same candidate
   fingerprint.

The slice reuses the current seed, emitter, verifier, certifier, pinned-port
geometry, and physical router. It does not introduce a second leaf router or a
general distributed scheduler.

## Chunk contract

A contract is immutable during one solve round and contains only information
that affects composition:

- the child's logical nodes and logical boundary signals;
- an allowed physical-region mask plus a keep-out halo;
- for each boundary signal, a portal **window**, direction, polarity, strength,
  and delay budget;
- coarse corridor capacity reserved by the parent;
- the certificate and timing summary returned by the child.

The model is a three-dimensional voxel mask, not a rectangle. The first
partitioner may emit a planar rectangular prism because it is simpler; no
public contract assumes that shape or gives a child the full height above a
two-dimensional footprint. The existing pinned-I/O handover is the leaf form
of a portal and remains the geometry authority.

## Ownership

Every physical resource has one owner at one level.

- A child owns placement and detailed routes wholly inside its region.
- The lowest common ancestor of all consumers owns a fanout trunk.
- The parent routes that trunk between child portal windows.
- A child routes from its assigned portal to local consumers.
- Halos and portal cells are parent-owned. A child receives them as immutable
  exclusions or interface cells; overlapping sibling halos therefore remain
  one parent resource and cannot be won by execution order.

This rule prevents duplicated trunks and makes a failed boundary attributable
to the nearest parent that can change it.

## Deterministic parallel execution

Parallelism is between independent child contracts, not between writes to one
reservation map.

1. The parent freezes contracts and congestion prices for a round. Prices for
   the next round are stable sums of typed refusals in canonical child order.
2. Children build private worlds in any worker order.
3. Results are merged in stable `ChunkId` order.
4. The parent resolves boundary refusals synchronously and starts another
   round only if a contract must change.
5. Root certification remains authoritative.

No worker mutates a sibling's reservations, and no result depends on which
thread finishes first. A one-worker run and an N-worker run must have identical
fingerprints.

`ChunkId` is derived from the parent ID and the canonical logical-node order of
the partition; it never depends on graph declaration order, hash iteration,
discovery order, or worker completion. Partitioning itself uses typed-ID tie
breaks and must return the same contracts when irrelevant input declaration
order changes.

## Refusals

A child returns a typed refusal instead of modifying another region:

- portal window has no legal detailed route;
- assigned region or corridor has insufficient capacity;
- strength or delay budget cannot be met;
- local physical verification failed.

The refusal travels to the nearest ancestor that owns the conflicting
contract. That ancestor may move a portal, resize adjacent regions, reassign
capacity, or repartition its subtree. Unrelated subtrees remain valid.

Every level has a checked round budget. Contract changes follow a monotone
repair order: widen capacity within the parent's region, then move a portal
within its window, then repartition the subtree. A repeated contract
fingerprint or exhausted budget returns a typed refusal to the next ancestor;
wall-clock time is never a correctness or termination condition.

## What is removed

Remove now:

- dead wrappers around the active access-envelope and track-aware facing
  implementations;
- the superseded one-source guard reservation helper;
- unused coordinate helpers and stale module-level dead-code suppression.

Do not implement these previously discussed detours:

- exact-prefix replay cache for the 68-attempt seed loop;
- optimistic parallel routing against a shared reservation snapshot;
- more route-order, branch-order, or local repair heuristics;
- a second raw-block global router.

Remove after the recursive path passes the same acceptance corpus:

- whole-circuit schedule/layout repair retries (`RouteBefore`,
  `ExclusiveGuardedTrack`, `EarlyTreeSinkAndEscape`, `SeparateOwners`);
- their repair budgets and full-attempt rebuild loop;
- trace code that exists only to diagnose that loop.

The repair loop remains a temporary fallback because it is currently required
to certify the seven-segment cases. Deleting it before replacement would be a
correctness regression, not simplification.

## Delivery gates

1. **Composition:** a checked small fixture certifies through two child
   contracts and one parent stitch.
2. **Determinism:** 1-worker and multi-worker results have identical candidate,
   plan, route-tree, pin, and transition-manifest fingerprints, including when
   irrelevant gate declarations are reordered.
3. **Replacement:** all six budget-zero acceptance cases certify within their
   existing quality limits and preserve the checked pinned glyph.
4. **Deletion:** disable the old repair fallback, rerun the six cases, then
   delete the fallback and its diagnostics in the same change.

Until Gate 3, the public synthesis API and default producer remain unchanged.

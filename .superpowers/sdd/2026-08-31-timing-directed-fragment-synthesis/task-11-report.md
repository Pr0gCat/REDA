# Task 11 report

Implemented deterministic combinational duplication inside the certified fragment search.

## Design delivered

- `DuplicateRequest` names one canonical instance, monotonic duplicate ordinal, and a typed `PhysicalSink` partition.
- Duplicate instance, primitive, connection, observation, and route identities are derived deterministically after canonical IDs.
- Duplicates retain the canonical gate's logical inputs and implementation while selected output sinks are rebound to the duplicate physical driver.
- Only instances with one concrete primitive output are eligible; junction/merge duplication and foreign sink partitions are named refusals.
- Single-instance and duplicate proposal counters interleave without exhausting no-fanout streams.
- Duplicate placement uses a logical-stage-local y/z lane and North facing. Canonical placement and pinned IO geometry remain unchanged.
- Every proposal rematerialises, routes, emits, physically verifies, functionally certifies, and enters best only on strict `QualityKey` improvement.

## Measured unit fixtures

- Far two-consumer fanout: observed settle improved from 44 to 38 ticks and the budgeted search accepted the duplicate.
- Compact two-output fanout: duplicate fully certified but did not improve `QualityKey`, so it remained non-committing.

## Verification

- Fragment tests: 9 passed.
- Instance-graph tests: 10 passed.
- Search tests: 5 passed.
- Primitive graph equivalence: 5 passed.
- Public API tests: 3 passed.
- Full independent seed suite including pinned IO: 12 passed in 191.25s.
- Targeted rustfmt check, `git diff --check`, and clippy with only the known unrelated `clone_on_copy` lint allowed: passed.

Manual review additionally excluded duplicate instances from canonical implementation/facing proposals, avoiding invalid override work after a duplicate has been accepted.

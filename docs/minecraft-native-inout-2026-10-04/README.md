# Native shared-inout fixture qualification

The final real-game run covers 128 raw control states of `fixture.sv`, using
the public native physical compiler API. All 37 actual gates, 12 lamps,
two actual injection pads, the complete channel, readback and external pad
are independently reconstructed from 654478 raw RCON command/reply pairs.
The portable audit passes, including 3282 original occupied positions cleared
to air and all 104 test-region chunks released.

Checked outcomes are 38 Value, 18 Contention, eight Floating and 64
ProtocolViolation. Invalid states never count as Boolean source success.
The independent offline audit compares all 64 declared states against the
8192 endpoint rows in the frozen standard HDL oracle. Actual-game coverage
is settled states of this one fixture; it does not establish all 4096 game
transitions, a waveform guarantee, circuit quality, browser integration or
closure of the broad shared-inout gap.

Reproduce the retained lightweight audits with:

```sh
python3 audit-expected.py
python3 audit-game.py
```

`protocol/` contains the frozen final protocol, including support placement
and the canonical source observation deadline guard. `run-final.py` adds
independent channel and injection-pad reads without weakening the canonical
settling conditions. `root-final-independent-audit.json` preserves the original
root result; `final-independent-audit.json` is the portable rerun.
The source-reference record identifies the existing main-inout archive;
no duplicate compiler source archive is included. Producer source records
its original workspace dependency/include paths rather than claiming this
documentation folder is a standalone compiler checkout.

Earlier evidence remains explicit: the initial incomplete-support run has
92 mismatched vectors; the all-supports run before physical relay observation
has three. Reader waveform samples and their negative results remain.
The strong-reader two-state test and corrected reader-route targeted test
succeeded within their narrow scopes. The earlier qualified run was
interrupted with exit 130 because direct injection-pad observations were
missing; its partial raw log is retained and does not count as qualification.

Raw logs use streaming gzip. `raw-log-sha256.json` records their original
decompressed hashes and sizes; `artifact-manifest.json` binds retained files.
No server properties or credentials are included.

The retained adjacent transition `0110100 -> 0110101` fails with gate-only
quiescence, although the isolated destination state passes. Immediately
after that premature return, the last two reader repeaters are still low.
They then become powered, the n11 torch turns off, and y becomes correct
about0.72 seconds after the first retained sample. The canonical fix observes
all actual repeater powered/locked and comparator powered states while
keeping the original five-poll count,0.25-second poll interval, floors and
5.8-second circuit ceiling. It also refuses a completed slow scan past the
original deadline. This sampled diagnostic proves the observation defect;
it is not a complete tick waveform or generator QoR measurement.

Canonical offline controls pass24 tests; two preexisting rewrite tests skip
because their generated fixtures were not provided. No new skip is added,
and no full project gate is claimed. The server is stopped normally after
cleanup. The observed server cgroup limit is4GiB, swap0; its retained peak
is1359888384 bytes.

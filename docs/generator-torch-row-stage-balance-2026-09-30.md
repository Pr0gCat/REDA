# Aligned strength-restoration boundaries in torch matrices

Status: accepted improvement to the physical prototype; production compilation
is unchanged. Parent prototype evidence remains in aefee60 and the prior reports.

## Observe and locate

The complete two-address/three-output family had 48 multi-input driver glitches.
Focused replay establishes that exactly 48 data tables require six shared cube
rows; each produces one driver glitch in the previous geometry. The example
[3,6,6,1] is retained with complete before/after driver and lamp traces.

Six row locations are z=4,7,10,13,16,19. Literal rails need a second strength
restoration repeater at z=14. Output rails previously restored at z=18. The row
at z=16 therefore crosses both restoring stages: literal then output. Early
rows cross only output restoration; the last row only literal restoration.
The extra 2 game ticks cause otherwise aligned row signals to reconverge late.

## General solution and rejected experiment

An attempted solution added delay to the faster row-selection repeaters. For
[3,6,6,1] it removed the glitch, but total world settling across 16 transitions
regressed 189 -> 201 ticks. That experiment was rejected and its extra delays
removed. The code snapshot was retained temporarily outside the repository;
this record is the rejection evidence, not a production change.

The accepted geometry instead places the output restoring stage at z=15,
immediately after the literal restoring boundary at z=14. Programmed data
torches sit one cell after the control row. Consequently early rows cross only
output restoration, late rows only literal restoration, and every complete path
crosses exactly one extra stage. Selection-repeaters retain delay 1. No signal
strength limit is reduced and neither required restoring stage is deleted.
The rule derives from the relative literal/data tap positions; it has no
case-name or data-word condition. Shorter matrices have no second stage and
remain unchanged.

## Independent fixed comparisons

The existing builder and its z=18 output restoration remain the control. All
48 tables were replayed with old/new geometry and each of nine scalar quality
metrics compared separately against a frozen independent JSON reference.
Replaying the control must equal that JSON, so a shared builder edit cannot
silently move acceptance limits. Candidate improvements may reduce those caps.

| Across the 48 six-row tables | Previous geometry | Aligned boundaries |
| --- | ---: | ---: |
| Driver glitches | 48 | 0 |
| Maximum world settling (game ticks) | 18 | 16 |
| Sum world settling over 768 transitions | 8994 | 8616 |
| Mean world settling (game ticks) | 11.7109375 | 11.21875 |

Every table separately holds or improves worst/total world settling, driver and
lamp glitches, single-input glitches at both sites, last driver-change time,
and worst/total wrong-target driver pulse widths. All new lamp glitches are 0.
Block/gate counts play no part in comparison. The earlier fixed [1,6,3,5]
reference against the existing production generator is retained unchanged.

## Exhaustive verification and scope

Both previous and aligned builders were independently replayed over all 4096
data tables. Per builder this covers 65536 measured and 65536 setup transitions
in persistent history, without added holds/cooldowns. All final driver and lamp
truth matches literal data. All aligned worlds reproduce their canonical
fingerprint. Aligned aggregate: zero driver glitches, zero lamp glitches,
zero single-input driver glitches, maximum world settling 16 game ticks.
The old aggregate remains 48 driver glitches and maximum 18 game ticks.

Command: `cargo test --release --test torch_row --test torch_row_decoder`.
Nine tests passed (3 row probes, 6 decoder/cover tests); both full-family replays
finished in the final decoder test run of 31.83 seconds. `git diff --check`
passed. The preexisting packing.rs unused_mut warning remains.

These probes sample end-of-game-tick changes and apply address changes together;
they do not establish arbitrary input skew, Minecraft runtime conformance,
unlimited ROM dimensions, or maximum clock frequency. Production generator and
its independent main/matrix quality caps were not changed or bypassed.

Next: express the shared-rail realization within production topology/mapping,
retain truthful per-net observation sites, and run the physical verifier
pipeline and existing timing/DFF/frontend regressions before adopting it.

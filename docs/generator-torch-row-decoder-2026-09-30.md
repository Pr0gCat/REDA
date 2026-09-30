# Decoded torch-row ROM experiment

Status: physical prototype evidence, not a production optimization.

Observe: a programmed ROM column which is 1 for every address can drop during
selection. Locate: direct and inverted select paths disable the outgoing row
at tick 2 and enable the incoming row at tick 4. The all-one column receives
no power between those events. This is a static hazard, independently of the
previous rapid-history burnout failures.

General solution tested: intersect all programmed words to identify always-high
columns, omit their selected-row torches, and power those columns independently.
Always-low columns already emit no torches. This derives from data rather than
case names. The fixture has one real address lever, a physical inverter, two
programmed rows, three shared output columns, and output indicator lamps.
Words are 101 and 110. All four address transitions, including unchanged input,
run in persistent simulator history without extra hold or simulator changes.

The reference compiles y0=~addr and y1=addr through the current frontend and
physical generator. The frontend rejects exported constant outputs, so the
third reference driver is an explicitly added isolated wall torch. This is a
composite physical reference, not an entirely compiler-generated ROM. Actual
driver values are checked, and lamp loading is present on the changing outputs
of both worlds. Reference and experimental builders are not interchangeable.

Results with indicator loading:

| Version | Worst settling (game ticks) | Sum across 4 transitions | Glitched driver-column transitions |
| --- | ---: | ---: | ---: |
| Existing generator plus isolated constant reference | 8 | 14 | 0 |
| Raw decoded torch matrix | 8 | 14 | 1 |
| Matrix with constant columns folded | 8 | 14 | 0 |

The raw matrix fails signal quality. Folding fixes its 2-tick static pulse,
but only reaches parity with the existing generator on this small example.
An earlier unloaded probe reported 4-tick settling for the matrix; that is
not a fair comparison to a compiler world with lamps and is not acceptance
evidence. The committed results use output lamps.

Both matrix variants reproduce identical canonical world fingerprints on
reconstruction. Four physical tests passed with
`cargo test --release --test torch_row --test torch_row_decoder`.
The reference baseline is frozen in the test and the accompanying JSON;
no existing quality threshold or production generator was modified.

Next required experiment: a multi-bit address decoder with shared rails,
independent ROM data truth, comparable output loads, complete transitions,
actual driver and lamp timing, then integration and existing quality regressions
if it improves quality. Rapid-input burnout from the prior probe remains
unresolved; this four-transition test cannot establish high-frequency behavior.

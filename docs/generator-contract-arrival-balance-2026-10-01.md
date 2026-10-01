# Contract-aware endpoint selection and candidate arrival balancing

Reference d5074bf. The previous generic weak-endpoint/retiming experiment
passed the combinational matrix but violated sequential geometry, observed
output isolation, and a frozen inverter-receiver pulse metric. This round
fixes the contracts instead of weakening the controls or selecting by case
names. No block/gate count or volume participates in acceptance.

## Root causes and final rules

Weak NOR entry and retiming now use the same topological eligibility set:
shared primary-input support, equal graph depth, or a layer of direct inputs
and their single NOR inversions. Graph depth counts every gate, including
merge nodes; intrinsic physical merge delay remains zero in arrival estimates.
A receiver with disjoint support and a deep inversion chain retains ordinary
automatic terminal selection rather than losing a discretionary input delay
without any matching synchronization. Selection depends on the graph, not
signal/module/fixture names. The naming-invariance selection test also checks
that a deep receiver is excluded while complementary literals are included.

Public-output owners retain the original repeater landing contract, and pinned
terminal consumers retain their original route policy. No upstream relocation
of a public-output diode is accepted. The unused relocation experiment and
WeakRepeater variant are removed; the new DirectedDustRunway contract forces
a straight final dust approach and refuses using that final cell for required
signal-strength refresh. Such refresh belongs upstream on the recorded route.
Existing branch repeater accounting is preserved. Sequential graphs refuse
arrival balancing and keep the original placement cell footprints. The new
footprint test checks every body of a graph containing a DFF against the
original cells, including a neighbouring multi-input NOR.

Retiming clones the complete route candidate transactionally. It reads actual
recorded relay states, inserts delay-1 repeaters only on unique straight dust
cells with recorded solid floors, and updates affected terminal repeater counts.
Insufficient insertion geometry discards the complete clone. The original
candidate realization/spacing/world verification runs afterwards. No world is
mutated after verification. Nominal route delay is an estimate, not a universal
proof of effective path dominance; complete physical histories remain required.

## Physical results and independent controls

Native and fresh-Yosys XOR each improve from the fixed mean 6.375 to 5.25
game ticks, world worst 12 to 10, driver/lamp last changes 8/12 to 6/10.
Driver/lamp extra transitions and wrong pulses are zero. The manual native
probe verifies every internal gate and an independent XOR expectation in all
16 setup and 16 target phases with retained history. Two complete runs are
byte-identical; repeated base compilation is deterministic and final emitted
world equals its candidate ledger without diagnostic mutation.

Native and fresh-Yosys Mux each measure mean 5.5 and world worst 10, passing
all unchanged per-case timing, glitch and pulse caps. The complete frontend
and generator suites pass 1312 target transitions plus setup histories,
including independent function expectations, all individual metrics and
deterministic generation. Native AND4 remains mean 6.56640625; fresh-Yosys
AND4 is 5.72265625 with no output glitches. No improvement elsewhere offsets
a regression. The original frozen controls and checkpoint files are untouched.

The inverter quality suite passes all five tests, including complete frozen
parent/receiver/fanout controls. The initial scoped-but-uncoupled policy failed
the receiver pulse cap. Its last printed passing tuple was arity 3, length 3
(18 pulse ticks equal to its cap), not the failing tuple. The next tuple,
length 4, failed before printing metrics. The prior document's attribution
to length 3 is corrected. The new receiver3 manual probe is a retained-history
positive control, not claimed as the previously failing fixture. Its 64 setup
and 64 target phases verify internal gates and independent output truth.

All six ROM shipping cases pass complete independent output histories and
all nine scalar metrics equal the independently frozen shipping comparison.
The test now enforces each unchanged frozen cap separately; it does not
regenerate the controls. Automatic torch-row selection stays disabled, and
this does not expand the explicit torch-row boundary or certify all stock ROMs.

## Validation and test geometry

- Six integration binaries pass: constant drivers (4 tests), DFF physical (1),
  frontend quality (1), generator matrix (1), inverter quality (5), observed
  output isolation (1).
- All 55 relaxation and 68 routing unit tests pass, plus the selection test.
- Six-case ROM shipping test passes with 96 target transitions per arm and
  independent per-case frozen metric assertions.
- git diff --check passes. Simulator behavior, retained history, hold intervals,
  proof/physics budgets and acceptance caps are unchanged.

The projection's exact-margin rounding witness originally hardcoded two
socket cells, so the new input runway made its initial placement illegal.
Its fixture now derives centre separation from the actual footprint extents.
Required degree-three clearance remains 3.75; the legal/just-illegal checks,
exact tested pair, opposing rounding and exact 0.75-cell loss assertions all
remain. This adapts geometry without relaxing clearance or the invariant.
The initial failure and final passing runs are preserved.

Complete reports, terminal/relay histories and test logs are adjacent. Material
statistics in existing reports remain diagnostic only. No full repository
green-suite, arbitrary circuit timing guarantee, or Minecraft certification is
claimed. The ongoing optimization goal remains active.

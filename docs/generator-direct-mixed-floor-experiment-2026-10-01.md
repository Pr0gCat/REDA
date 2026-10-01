# Direct input floors for an independent mixed product

Fixed certified control: beb8bbf, generator-weak-nor-fanin-experiment family.
The independent 00, 11 and mixed full-cube outputs retained matched source
restoration. The mixed product still passed through source restoration,
local mismatch restoration and the final torch, adding unnecessary latency.

The generator now places both public levers beside a subtract comparator:
the positive input strongly powers its rear support; the negative input
strongly powers its side support. Separate raw dust arms still drive the
old matched source repeaters for the other two products. Their restoration
and the weak-side NOR remain intact. Input order follows product polarity;
output order follows the original product bindings. Recognition uses full
cube structure and one independent term per output, never function IDs or
source names. All twelve polarity/output permutations are exercised.

An initial prototype restored both comparator operands with private repeaters.
Strict certification rejected ComparatorElectricalContract: the existing
contract requires a primary lever floor at rear and accepts a primary lever
floor or certified inverter comparator at side. The repeater prototype was
reverted before quality measurement. The accepted layout instead satisfies
the existing electrical contract directly. No adapter, verifier, simulator,
hold duration or history reset was changed. The selected library declares
existing Buf, Nor2 and comparator entries; forced generic fixtures select
the corresponding library without invoking unrelated product specializations.

All 4096 source functions pass equivalence, strict physical certification and
every implemented signal over 32 continuous setup/target phases. Twelve
functions improve and 4084 tie, with zero regressions on the twelve separate
target metrics. Example [4,0,1,2] target total settling falls from 103 to 89
ticks; worst settling remains 10, driver last change 6, lamp last change 10,
and all glitch/pulse metrics remain zero. All 2744 immutable prior caps pass;
setup has zero regressions and zero glitches. The complete repeat is byte
identical, SHA256 af7ce60a93b31420bf6d24a13c6ec3286753d316c2c9dca017c8ce010c4963cd.
Both producer exits are 1 only because 27 unchanged shipping controls remain
incorrect. Seventeen player tests, 77 Native/fresh-Yosys source fixtures,
five physical rewrite consumer tests and fourteen API/snapshot tests pass.

The prospective target-only policy now chooses 4060 player and 36 shipping
cases, with zero retained-cap conflicts. This remains a diagnostic: setup
and the actual default entry have not qualified that policy. Default selection
is still disabled; consumer contracts, fallback selection and actual compile()
family/repeat verification remain required. Block count and volume are not
acceptance criteria.

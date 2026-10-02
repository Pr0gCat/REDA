# Independent AND, NOR and negative literal outputs

Reference: 99a6f51. Fixed certified circuit control: the 79c0b28 direct-mixed-floor
family, unchanged by the intervening consumer-only commit. The default diagnostic
had three incomparable choices. Shipping had smaller total target settling but
worse worst settling and driver/lamp last change, also worse on those setup
metrics. The common structure is independent AND, NOR and a negative literal.

The generator recognizes complementary full-cube products plus one independent
negative literal. Three variable outputs each have one product; recognition
uses the minimum cube cover, not function IDs, names or a whitelist. This
extends weak-side NOR fan-in to that structure. The independent unary inverter
reads its already-restored source line directly and drives a private descending
rail and lamp, removing the extra local restoration before inversion. Both
shared source repeaters remain matched. The existing mixed full-cube family
continues to use its directly supplied comparator; it is not reclassified as
an unary inverter. The actual implemented graph and observations match these
stages and their source-ordered public bindings.

The initial NOR-only experiment passed the strict sample but changed total
settling 108 to 104 ticks for [6,4,0,1], insufficient to resolve shipping's 103.
Retained internal events located the remaining repeat-restoration/inversion
on the negative literal. Removing only that redundant local stage yields 92
while worst settling stays 10, driver last change 6 and lamp last change 10.
The retained 3->1 target events show source restoration at tick 2, local
restoration at tick 4 and unary driver at tick 6 in the intermediate layout.
The accepted layout has source restoration at 2 and unary driver at 4; its
lamp changes at 5 instead of 7. AND driver/lamp remain at 6/10. This isolates
the redundant stage without changing the simultaneous AND path.
All driver/lamp glitch and wrong-pulse metrics remain zero. The complete
experiment covers both unary input choices and all six output permutations.

All 4096 functions pass source equivalence, strict physical certification and
every implemented signal after every one of 32 continuous setup/target phases.
Twelve improve, 4084 tie and none regress on the twelve separate target metrics.
All 2744 immutable prior caps pass. Setup has zero regressions and zero glitches.
The final-tree repeat contains all 4096 records and is byte-identical, SHA256
1be06a66d86244ac0d730f7b1205eab38771b2893f159e8b825920b3bec19a81.
Both producer exits are 1 solely because 27 unchanged shipping controls are
incorrect. Seventeen player tests include all twelve new internal-truth
permutations; 89 Native/fresh-Yosys source fixtures, six rewritten consumer
tests and fourteen API/snapshot tests pass. Consumer checks exercise both
new input choices through equivalence and total world partitioning.

The prospective target policy now chooses 4063 player and 33 shipping cases.
There are zero retained-cap conflicts, zero target tradeoff choices and zero
setup regressions/glitches for selected shipping cases relative to the player
candidate. Reproduce with probe_default_selection_tradeoff.py and
probe_default_setup_tradeoff.py against this frozen family. These are measured
public driver/lamp diagnostics, not proof of actual default integration or
stock internal truth. Automatic selection remains disabled. The mc_dump /
conformance actual implementation schema, qualified runtime fallback contracts,
actual-default full family/repeat and DFF regressions remain required.

No simulator/verifier/adapter, settling hold or history-reset policy was
changed. Block count and volume were not used as acceptance metrics.

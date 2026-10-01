# Selective arrival balancing passes combinational caps but breaks DFF routing

Reference 15667f9. Retain the weak NOR endpoint candidate and move selective
arrival balancing into the candidate route ledger before existing world
verification. Selection uses shared primary-input support, equal topological
depth, or a layer of direct primary inputs and their single NOR inversions.
Names do not select the rule. Retiming is transactional on a clone, using
private straight solid-backed dust cells and updating all affected branch
repeater counts. Insufficient insertion geometry discards the whole clone.

The first experiment counted merge depth as zero, confusing logical layers
with zero intrinsic merge delay; native AND4 still totaled 2271 ticks against
cap 1681. Counting every graph gate as one topological layer separates those
concepts. The corrected support/equal-depth variant passes the frontend
physical suite, but XOR retains one lamp glitch: n0->n2 rises at tick 2 while
b->n2 falls at tick 0 nominally. Those sources have disjoint primary support,
so shared-support recognition alone misses the complementary-literal hazard.
The native AND diagnostic exposes the actual intermediate OR chain; it passes
all 256 setup and 256 target phases with independently expected conjunction
and every internal gate, total target settling 1631. Bit order is a,b,c,d
least-significant first, explicitly recorded separately from frontend profiles.

Adding direct/single-inversion literal-layer recognition passes both unchanged
frontend and generator physical quality suites, covering 1312 target transitions
and their setup history, deterministic generation and individual timing/pulse
caps. Native XOR totals 84 ticks (mean 5.25, worst 10) with zero output glitches
and wrong pulses. Two complete diagnostic runs are byte-identical; base and
final plan/world fingerprints match without post-emission mutations.
The 68 routing unit tests pass. No block count ranks acceptance.

Appropriate regressions reject the candidate. The initial pure-combinational
arrival analysis panics for a native constant-register input. An explicit
sequential-network refusal corrects that: all four constant-driver tests then
pass. But native/fresh-Yosys DFF physical qualification still refuses a safe
local route from (41,1,58) to (42,1,59). Disabling the retiming analysis alone
does not undo the generic multi-input NOR approach-space reservation, which
also influences layouts around sequential primitives. Additional regressions also reject it: the multi-input receiver inverter
fixture exceeds its independently frozen total-driver-pulse cap (observed 18
ticks at arity 3, chain length 3). Four other inverter quality tests pass.
Observed-output isolation refuses a DirectedDustIntoSupport landing that its
unchanged contract requires to be one-way. Upstream relocation of a relay
does not itself satisfy this support-side isolation contract. These are material
regressions, not excused by combinational wins. Candidate patch and logs are
retained in adjacent files. Full repository success is not claimed.

All three production files are restored exactly to HEAD after recording the
experiment. The only diagnostic change retained adds and4-native to the
existing manual observer, keeping independent lamp truth and complete internal
truth checks. Next separate weak-entry approach reservation from unrelated
sequential geometry, then repeat all fixed controls and DFF histories before
any production acceptance. Simulator qualification is not Minecraft proof.
Goal remains active.

After withdrawal, all six selected integration binaries pass: frontend and
generator (1312 target transitions), constant drivers (4 tests), DFF physical
(1), inverter bypass quality (5), and observed-output isolation (1). The added
native-AND manual diagnostic passes every internal and independent output
expectation for 512 phases against unchanged production, with two identical
complete runs and deterministic base compilation. The full independent control
trace is retained in the JSON. No full repository pass is claimed.

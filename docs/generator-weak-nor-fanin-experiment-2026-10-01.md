# Weak side fan-in for an independent NOR product

Fixed certified control:9c85ba1, generator-opposite-alias-experiment family.
Three independent minterm outputs can include complementary00/11 products.
Matched source stages are still needed. Removing only one source stage was
rejected: example[4,0,1,2] total settling111 became104, but driver/lamp glitches
rose0 to3 and wrong pulses appeared. That asymmetric source experiment was
reverted. Source stages remain matched in the accepted implementation.

The00 product now uses two independent input dust landings on opposite sides
of one torch support. They weakly power the support without repowering the
other input line; the torch supplies the NOR function directly. This removes
the00 condition's two internal restoration stages while retaining the shared
source restoration. Recognition requires three full-cube products, one term
per output, and the complementary control structure. It does not inspect
function IDs or source names. Other families retain their qualified layouts.

The first compact side layout inadvertently joined the two input lines and
was rejected by DuplicateTerminalCell. Separated source tracks fixed that.
Moving those tracks exposed an observation-owner collision at the old control
junction; a separate junction observation fixes it. Complete output permutations
then exposed farthest-tap power dropping to zero. The control span is shortened
and output lamps now terminate their separate rails inline. These are actual
reachability and isolation changes; no verifier or simulator rules were changed.

The typed matrix library declares the existing standard two-input torch NOR.
For a torch without the usual dust-above-support input, the manifest producer
chooses the reachable side dust terminal of its owned support. The physical
adapter/verifier still certifies the actual paths, source identity, support,
weak landing and isolation. Previously supported top landings retain their
original path. This is manifest generation, not relaxed acceptance.

Final4096 source functions pass strict certification and internal implemented
truth over32 continuous setup/target phases. Twelve tables improve,4084 tie,
0 regress on the twelve separate target metrics. Example[4,0,1,2] total target
settling111 becomes103, with worst settling10, driver last change6 and all
pulse/glitch metrics unchanged. All2744 immutable caps pass; setup has0
regressions and0 glitches.17 player tests include all12 independent-product
output permutations and each internal signal;77 Native/fresh-Yosys source
fixtures,5 rewrite consumer tests and14 fragment API/snapshot tests pass.
The full second run is byte-identical, SHA256
fa203e2648f4991ba752d920ab9e20db06d44a569dfe0e1b191e383a4ee9953e.
Producer exit1 still reflects27 unchanged incorrect shipping controls.

Default selection is still disabled. The unqualified target-dominance diagnostic
now chooses4059 player and37 shipping tables, with1 remaining retained-cap
conflict at[4,0,1,2]. Setup has not qualified that policy. The independent mixed
product still has unnecessary restoration before inversion; that path and the
remaining default consumer/fallback contracts require further work. Block
count and volume remain diagnostics, never acceptance metrics.

# Opposite-polarity output aliases

Fixed certified control:3760241, generator-opposite-literal-sum-experiment family.
The old generator refused repeated opposite unary outputs in a shared positive
OR family because two public aliases could terminate at one physical driver
cell. The earlier shared prototype was rejected by DuplicateTerminalCell;
retaining the generic matrix was safe but left redundant OR restoration.

The private inverse now has one producer and a fork with two separated carrier
arms. Each ordered public output has its own driver and lamp coordinate. A
geometry offset supplies enough room on either side of the primary inputs;
the offset applies only when the cover actually has repeated inverse outputs.
Single inverse outputs, prior polarity sums and all unrelated families retain
their qualified geometries. Recognition depends on cover structure and output
occurrences, not names or function numbers. Unary restoration remains intact.

Example[6,7,1,1] total target settling79 becomes67; worst settling8 and driver
last change4 are unchanged, with no pulses/glitches. Both alias ports are
observed independently; they do not share an output terminal or event stream.
All66 positive-OR/opposite-unary combinations now have retained-history unit
coverage. Full4096 source functions, physical certification and internal signal
truth pass over32 continuous setup/target phases. Six tables improve,4090 tie,
0 regress on all twelve target metrics. All2744 fixed caps pass; setup has
0 regressions and0 glitches.17 player tests,77 Native/fresh-Yosys source fixtures
and5 physical rewrite consumer tests pass. Two raw full outputs are identical;
SHA256 e49577a1e966513cb82c489af4d74fc327f9685a4cb3b45f9c7bfc829bb547b4.
Producer exit1 reflects27 unchanged incorrect shipping controls. No simulator,
verifier, prior caps or settling history is modified.

The separate, unqualified prospective default diagnostic chooses4056 player
and40 shipping tables and still violates caps for3 cases, all permutations of
three independent minterm outputs. Setup was not evaluated for that policy.
Default compile selection remains disabled pending the remaining generation
tradeoff and complete default/fallback qualification. Blocks/volume are not
acceptance metrics. The archived full waveforms and summary retain the evidence.

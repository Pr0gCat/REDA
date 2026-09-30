# Internal-only isolation experiment (rejected)

Control: `31859ac`. Inspecting the actual native `xor2` route ledger shows all six NOR-input landings are directed dust, including both primary-input fanouts. Its only terminal repeaters are the merge inputs. All 32 setup/target history phases have correct internal gate drivers and output lamps. Thus the previous purely logical hazard inference was conservative for this actual geometry; no strongly powered NOR support exists here.

The diagnostic now accepts `xor2` to compile the exact native source used in the quality matrix. It also declares only one physical observation per driven signal, preserving aliased public signals by sharing their physical observation instead of submitting duplicate declarations to stock compile.

Experiment: require diode terminals for every internally produced net with multiple consumers, preserving the existing public-output rule and leaving primary input fanout automatic. No case names are inspected. This passes the fixed 1312 production quality transitions, four constant-driver tests, native/fresh-Yosys DFF test, and the observed-output metamorphic test. All nine metrics of each of the six frozen ROM shipping comparisons remain unchanged.

The complete stock family scan rejects it at encoding 2716, words `[4,3,2,5]`, after encodings 0 through 2715 pass output history truth and deterministic recompilation. Transition 0 to 1 has output bit 1 driver `(32,1,24)` low instead of high. The manual diagnostic first finds `n3 = NOR(g0,b)` off instead of on at `(29,1,36)`.

At address 1, `n0=a` is true, and its newly required repeater `(34,1,37)` strongly powers the `n5=NOR(n0,b)` support. Its other input `b` still has a bare dust landing `(36,1,37)`. That shared primary-input route also lands at `n3` through bare dust `(30,1,37)`. Power returning along `b` therefore disables `n3`, even though the input lever declares false. The route ledger exactly matches the emitted world. Primary input fanout cannot categorically be excluded from isolation.

The production change and temporary family test are withdrawn. No accepted tests, metric caps, or physical observation intervals are altered. Production code is restored exactly to the control. The diagnostic improvements and rejected experiment record are retained for review.

Next hypothesis: base isolation on actual strong support power and reverse-path reach, including primary input routes. A blanket requirement on either all fanout or all internal fanout is insufficient to preserve both truth and accepted performance.

# Balanced four-input matrix: physical premise evidence

The player-style torch matrix now has a reproducible four-input generator prototype, bounded to one through seven outputs. The verified cases are the complete seven-segment decoder and the 16×7 ROM. The same generator consumes each complete source truth table. It does not select by module, signal, or function name.

The measured problem was unequal arrival time through literal rails, decoder rows, and collection rails. The premise compensates the positive and negative input paths, compensates columns that cross different row refresh stages, and aligns literal-rail and output-rail refresh boundaries. Rows whose entire output word is zero need no programmed output and are omitted. A final common output delay stage was unnecessary: removing it preserves alignment and eliminates the remaining slower public arrivals.

The frozen seven-segment reference has a 44-game-tick worst target phase and lamp glitches in 83 of 256 target transitions. The final premise has a 22-game-tick worst target phase and no driver or lamp glitches. The ROM reference has an 82-game-tick worst target phase and lamp glitches in 157 of 256 target transitions; its premise measures 26 game ticks and no driver or lamp glitches. Block count and volume do not decide acceptance.

Each circuit runs the original 512 setup/target phases in three separately exported and loaded worlds. Complete records and canonical world fingerprints match across repeats. Independent replay checks every recorded primitive and intermediate merge signal: 213 observation sites for seven segment and 309 for ROM. All implementation primitives are covered by an explicit inventory assertion in the producer. There are no extra internal transitions, slower public phase/channel arrivals, slower settlement phases, or regressions in any retained aggregate setup/target metric.

Earlier arms are retained separately. The full-row premise measures 28 ticks for seven segment but has 814 slower public arrival comparisons and two slower settlement phases. Omitting empty rows reduces it to 24 ticks and 84 slower public arrivals. The final output-stage correction removes those remaining regressions. These are experimental premises, not repinned reference circuits.

The auditor independently replays the unchanged reference source, implementation, and landing events before using its truth table. The original ROM reference has ordinary compiler verification rather than an expanded certificate; that distinction remains recorded. The auditor never promotes a premise into a certified candidate. Its runnable control also rejects a missing event, an incorrect internal sample, a false certificate claim, and a truncated protocol.

Run the retained evidence check with:

```sh
python3 tools/test_audit_shared_matrix_premise.py
python3 tools/audit_shared_matrix_premise.py \
  docs/shared-matrix-balanced-output-2026-10-03-seven-repeat1.log.gz \
  docs/native-module-instances-2026-10-02-seven-native.jsonl.gz
```

Exact source, linked-library and executable hashes, frozen SDK source, raw phase logs, all comparisons, canonical world fingerprints, and representative litematic artifacts are listed in `shared-matrix-balanced-output-2026-10-03.json`. The generator is `shared-matrix-four-input-prototype-2026-10-03.rs`; its experimental arguments are `prune-empty short-output` after the source path and top module.

Expanded physical certification and source/implementation consumer bindings remain missing. No production generator or default compiler path changes. This is simulator premise evidence, not Minecraft conformance, a full-project pass, or completion of any of the eight goal items. The next required step is to express the physical stages through the existing typed candidate and library owners and pass the unchanged strict certifier.

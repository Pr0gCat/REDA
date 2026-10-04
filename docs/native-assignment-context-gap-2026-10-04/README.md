# Native assignment context gap

Clean committed58b4366 rejects three required source-context cases: signed4-bit parameter -1 assigned to8 bits (expected255), unsigned4-bit parameter15 assigned to8 bits (expected15), and8-bit parameter171 assigned to4 bits (expected11). All three fail at the elaborator's exact-width refusal; they do not reach physical compilation. The first harness build had a map-index borrow error and is retained separately; the corrected diagnostic has terminal101 with all three refusals captured.

Parameter declaration/override coercion already exists, but assignment and hierarchy contexts do not reuse typed width/signedness semantics. Source RTL presently drops a parameter's signedness when converting it into a constant expression. The responsible extension must preserve integral expression types and apply context widths consistently across continuous/procedural/clocked assignments and hierarchy input/output bindings; a parameter-only special case would not satisfy gap5. Self-determined concatenation/selections and comparisons/logical expressions need their language-defined sizing rules. No simulator change or support claim is made. User-owned arithmetic changes remain untouched.

This isolated test is diagnostic evidence, not an adopted production test or an accepted implementation. All eight broad rows remain open.

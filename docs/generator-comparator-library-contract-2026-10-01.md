# Comparator inverter library declaration

The implementation declaration is registered against a typed `LibraryEntryId`, independently of diagnostic names and candidate placements. A declaration binds the comparator role, the rear constant torch role and the single external side input. Registration requires a NOR1 entry with exactly those two primitives, one constant-to-comparator internal edge, the external input on the comparator, and comparator output. Invalid registration leaves the library unchanged.

The library revision descriptor includes the bound roles, side input identity, required subtract mode, rear strength 15 and side high strength 15. The descriptor omits the optional declaration for ordinary entries, preserving their existing serialized representation. This is an authoritative declaration of required conditions, not proof that a candidate satisfies them.

Comparator primitive Boolean equivalence remains rejected. No ROM generator selection or simulator semantics change in this step. Remaining work: propagate the declaration through selected topology authority; validate physical mode, rear constant ownership and side/rear full-strength source paths; reject foreign injection and every invalid cell from the frozen mutation study; only then admit conditional comparator Boolean semantics and run the complete independent ROM quality family and replay.

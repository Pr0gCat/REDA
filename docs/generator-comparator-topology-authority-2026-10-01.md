# Comparator requirements in selected topology authority

Parent: `54dca12`. Library-bound comparator inverter requirements now resolve to the instance comparator primitive, constant rear primitive and external side connection. The selected requirements include subtract mode and 15-strength rear/side boundaries. They enter the topology fingerprint; absent contracts are omitted from serialization, preserving ordinary topology payloads.

Independent library re-instantiation reconstructs these requirements. Structural comparison checks the requirements directly as well as the stored fingerprint. A negative regression attempts to invent requirements on an ordinary topology while retaining its original fingerprint; certification rejects it as ImplementationTopology corruption. Candidate declarations cannot substitute for selected-library authority.

Validation: 11 structure-verifier tests, 10 topology tests and 4 player generator regressions pass. No comparator Boolean semantics have been enabled. The physical mode, source ownership and full-strength boundary checks remain required before a comparator inverter candidate can qualify, and the complete frozen ROM family/replay remain mandatory before changing generation.

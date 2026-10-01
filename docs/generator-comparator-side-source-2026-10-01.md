# Comparator side source binding

Parent: `9c12b68`. Structural certification now requires the declared side connection to originate at a primary input boundary. That boundary must own a floor lever and its solid support, and the support must be an actual side neighbour of the selected comparator. This gives the intended strong 15-strength input when the declared lever is on. Direct side levers, missing supports and displaced supports reject.

Validation: 13 structure-verifier tests and 4 player regressions pass. These source geometry checks do not establish the zero-state boundary in the presence of foreign drivers, nor the isolation of the constant torch support or the unused comparator side. Those world-level checks are still required. Comparator Boolean semantics and ROM generation remain unchanged; no complete inverter certificate is claimed.

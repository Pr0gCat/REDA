# Actual terminal-policy caller trace

Independent actual-source probes: original 57ed241^, pre-arrival 32df849^, and clean fd61dcfd0d4145b8b1b8f4c815aa1f7c6913d0d6. Each probe executable was frozen before Cargo target reuse; source-specific actual compilation and raw route/world dumps are retained. No production or guard was modified. Original exact guard passes; fd61dcf fails with z extent 62.

Production has no automatic_gate_terminals=true caller. The true lay_net wrapper is test/reference compatibility. Ordered route_in_order uses qualified typed_legacy_request_parts, TerminalStairs and DistanceOnly search, then applies balance_candidate_arrivals. Negotiated and grown portfolios use automatic_gate_terminals=false, TotalStairs and the goal-height discount for DistanceOnly; negotiation does not apply the arrival-balancing postpass. Strict fragment and seed paths use separate placement pipelines.

relaxed_placement calls relax::relax/build. Growth constructs and rebuilds the same BodyGraph contract model. cells_for_graph feeds placed_cells/lift_above_bound. The old graph-wide no-DFF/multi-input placement predicate does not encode each actual receiver policy.

Original and current netlists and all ten terminal signatures are identical; original/pre-arrival anchors agree. Current moves g0 z54->53, g1 z54->55, g3 z47->46, g4 z39->38, and primary b (34,1,58)->(34,1,59). The previously reported primary c label was incorrect: it is b. Raw coordinate states, anchors, drivers, inputs, outputs and actual route/terminal fields are included so bbox alone is not the comparison standard.

The follow-up route-owned-air candidate is rejected by a real internal-gate history counterexample. See ../terminal-owned-air-placement-2026-10-04/proposal.md. Caller localization explains the historical placement mismatch but does not establish a safe general repair.

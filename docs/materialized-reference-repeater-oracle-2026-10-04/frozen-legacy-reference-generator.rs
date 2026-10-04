fn legacy_lay_net_reference(
    signal: &str,
    source: Anchor,
    consumers: &[NetConsumer],
    netlist: &Netlist,
    candidate: &PlanCandidate,
    reservation: &mut Reservation,
    prices: &Prices,
) -> Result<Route, Box<RoutingFailure>> {
    let signal = signal.to_string();
    let source_strength = merge_source_strength(netlist, candidate, &signal, source);
    let mut route = Route::new(signal.clone(), Vec::new());
    route.owner = Some(signal.clone());
    for consumer in consumers {
        // Where this branch must end, and where the search may aim to get
        // there. A gate socket has exactly one lawful approach cell -- the
        // one collinear with socket -> support, because a terminal component
        // only drives the support it faces and only reads from directly
        // behind itself; the legacy router guarantees that with a dedicated
        // approach column, and here the search is aimed one cell further out
        // and the socket appended, the same guarantee stated as geometry.
        // An output terminal is the same shape: its handover is a delivery
        // repeater, which reads only from directly behind, so the pin leaves
        // exactly one approach -- the net cell the pin itself names. That is
        // the price of the pin naming the handover rather than the compiler
        // choosing it, and the repeater hands it straight back: its rear is
        // an ordinary net cell, reachable bent, branched or nearly spent
        // (measured, `tests/terminal_handover.rs`).
        //
        // `try_move` asks the gate question the same way since Task 10:
        // `route_endpoints` threads each branch's `RouteSink` out and
        // `declared_socket` reads `input_directions(facing)[input_index]`
        // off it. The geometric guess `terminal_socket` makes survives only
        // for a route whose sink the netlist never declared.
        let (socket, approaches) = match consumer {
            NetConsumer::Gate { gate, input_index } => {
                let (_, socket, approach) = candidate
                    .input_connection_at(*gate, *input_index)
                    .expect("a declared gate input has a physical landing");
                (socket, vec![approach])
            }
            NetConsumer::Terminal {
                handover, net_cell, ..
            } => (*handover, vec![*net_cell]),
        };
        // What this branch's failure is called, in the error's own words.
        let sink_label = match consumer {
            NetConsumer::Gate { gate, input_index } => {
                format!("{}.in[{input_index}]", netlist.gates[*gate].output)
            }
            NetConsumer::Terminal { port, .. } => format!("output `{port}`'s terminal"),
        };
        // The search-time tree rule for THIS branch, rebuilt per branch
        // because the tree it must not re-join is whatever the earlier
        // branches laid. `Prices::RipUp` always answers `Off`, so the
        // shipping router's search is untouched.
        let own_join = OwnJoinCheck::for_branch(prices.own_join(), &route, reservation);
        // The laid trunk's cells and what stands in each -- the strength-
        // aware searcher rides them at the shared-prefix arithmetic
        // (repeater restores, dust decays) along its own path, which is
        // exactly the walk the accounting below applies to a ridden line.
        let trunk: BTreeMap<Anchor, BlockState> = route
            .anchors
            .iter()
            .zip(&route.realisation)
            .map(|(anchor, block)| (*anchor, block.clone()))
            .collect();
        let mut found: Option<Vec<Anchor>> = None;
        let mut aimed_at = approaches[0];
        for &approach in &approaches {
            let attempt = match prices.search() {
                SearchModel::DistanceOnly | SearchModel::DistanceWithGoalHeightDiscount => deterministic_astar(
                    source,
                    approach,
                    socket,
                    &signal,
                    reservation,
                    &own_join,
                    prices,
                ),
                SearchModel::StrengthAware => strength_aware_astar(
                    source,
                    approach,
                    socket,
                    &signal,
                    reservation,
                    &own_join,
                    prices,
                    &trunk,
                    source_strength,
                ),
            };
            if attempt.is_some() {
                found = attempt;
                aimed_at = approach;
                break;
            }
        }
        let approach = aimed_at;
        let mut path = match found {
            Some(path) => path,
            None => {
                // A pinned terminal has one lawful cell and no second way in,
                // so a search that fails here is the caller having
                // over-constrained the board -- refused against **that pin**
                // rather than reported as an anonymous corridor the router
                // might have taken elsewhere. The failure still travels
                // through the negotiation loop, so rip-up and re-ordering get
                // their turns first; this is the sentence the loop ends on.
                let error = match consumer {
                    NetConsumer::Terminal {
                        port, at, handover, ..
                    } => PlannerError::InvalidPortPin {
                        port: port.clone(),
                        at: *at,
                        refusal: PinRefusal::UnreachableHandover { cell: *handover },
                    },
                    NetConsumer::Gate { .. } => PlannerError::NoLocalRoute {
                        from: source,
                        to: approach,
                    },
                };
                return Err(Box::new(RoutingFailure {
                    blocked: signal.clone(),
                    corridor: (source, approach),
                    reservation: reservation.clone(),
                    charge_outright: Vec::new(),
                    is_decay: false,
                    error,
                }));
            }
        };
        path.push(socket);
        reserve_path(reservation, &signal, &path);

        // How much of this branch the trunk already laid, and what the
        // signal is worth by the time it gets there. Planning the whole
        // path from full strength would put refreshes on trunk cells that
        // keep the first branch's blocks, so they would be planned and
        // never built.
        let shared = path
            .iter()
            .take_while(|anchor| route.anchors.contains(anchor))
            .count();
        let mut carried = source_strength;
        let mut previous_cell = source;
        let mut trunk_repeaters = 0u64;
        for anchor in &path[..shared] {
            let index = route
                .anchors
                .iter()
                .position(|laid| laid == anchor)
                .expect("the shared prefix is by definition already laid");
            if route.realisation[index].kind == crate::redstone::world::block::BlockKind::Repeater {
                carried = crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
                trunk_repeaters += 1;
            } else {
                carried = carried.saturating_sub(1);
            }
            previous_cell = *anchor;
        }

        // An output terminal's last cell is a delivery repeater, which is
        // what an ordinary branch's budget already plans for when it needs a
        // refresh there -- so no styling: the terminal arm below simply forces
        // the repeater the contract requires, and a repeater is always at
        // least as strong as the dust the budget would otherwise have laid.
        let laid = realise_branch_from(previous_cell, carried, &path[shared..]);
        if !laid.carries {
            return Err(Box::new(RoutingFailure {
                blocked: signal.clone(),
                corridor: (source, approach),
                reservation: reservation.clone(),
                // Every cell this branch put above the gate plane. Nothing
                // else is at fault: the climb is what left the refreshes
                // nowhere to stand.
                is_decay: true,
                charge_outright: path
                    .iter()
                    .copied()
                    .filter(|cell| cell.y > PLANNER_Y)
                    .collect(),
                error: PlannerError::PhysicalInvariant(
                    compile::CompileError::CandidateMetadataViolation {
                        item: signal.clone(),
                        reason: format!(
                            "the route to {sink_label} decays to nothing before it \
                             arrives, and no cell along it can hold a refresh"
                        ),
                    },
                ),
            }));
        }
        // Whether the strength budget already needs the socket cell to be
        // a refresh. If it does, no terminal-style preference may take it
        // away: dust that cannot reach is not a cheaper terminal, it is a
        // dead one.
        let budget_needs_repeater = laid
            .blocks
            .last()
            .is_some_and(|block| block.kind == crate::redstone::world::block::BlockKind::Repeater);
        for ((anchor, block), floor) in path[shared..].iter().zip(laid.blocks).zip(laid.floors) {
            if route.anchors.contains(anchor) {
                continue;
            }
            route.anchors.push(*anchor);
            route.realisation.push(block);
            route.floors.push(floor);
        }

        let predecessor = path
            .get(path.len().saturating_sub(2))
            .copied()
            .unwrap_or(source);

        match consumer {
            NetConsumer::Gate { gate, input_index } => {
                let (gate, input_index) = (*gate, *input_index);
                let (support, _, _) = candidate
                    .input_connection_at(gate, input_index)
                    .expect("a declared gate input has a physical landing");

                // A branch whose every sink is the same wire merge joins that
                // merge's own dust, not a gate's support block: dust meets dust
                // and nothing has to drive anything. The same condition
                // `merge_branch_is_bare` states, read off the netlist.
                let bare_merge = netlist.gates[gate].is_merge()
                    && consumers.iter().all(|other| {
                        matches!(other, NetConsumer::Gate { gate: sink, .. } if *sink == gate)
                    });

                // EVERY terminal into a merge is a repeater (2026-08-29). A merge's
                // outbound strength used to be whatever its feeders happened to
                // deliver -- the two-lens microscope read g13's pin at 9 where every
                // model assumed 15, and the six phantom cells killed the branch.
                // With a repeater at every inbound socket the junction always
                // receives full strength, so what a merge-sourced net inherits
                // becomes pure geometry (see `merge_source_strength`). Game-legal --
                // it is the isolated-entry pattern applied uniformly -- and paid for
                // honestly: two game ticks per feeder, on the delay term.
                let into_merge = netlist.gates[gate].is_merge();
                let kind = if bare_merge {
                    if let Some(index) = route.anchors.iter().position(|anchor| *anchor == socket) {
                        route.realisation[index] = compile::repeater(compile::direction_from(
                            Position::new(predecessor.x, predecessor.y, predecessor.z),
                            Position::new(socket.x, socket.y, socket.z),
                        ));
                    }
                    RouteTerminalKind::BareMergeRepeater
                } else {
                    let style = if budget_needs_repeater || into_merge {
                        TerminalStyle::RepeaterIntoSupport
                    } else {
                        terminal_style(&TerminalApproach::new(
                            predecessor,
                            socket,
                            support,
                            laid.strength_before_terminal,
                            terminal_is_isolated(
                                reservation,
                                &signal,
                                predecessor,
                                socket,
                                support,
                            ),
                        ))
                    };
                    if let Some(index) = route.anchors.iter().position(|anchor| *anchor == socket) {
                        route.realisation[index] = match style {
                            TerminalStyle::RepeaterIntoSupport => {
                                compile::repeater(compile::direction_from(
                                    Position::new(predecessor.x, predecessor.y, predecessor.z),
                                    Position::new(socket.x, socket.y, socket.z),
                                ))
                            }
                            TerminalStyle::DirectedDustIntoSupport => compile::dust(),
                        };
                    }
                    style.into()
                };

                // A terminal has to stay a straight line into its support, and a
                // net is otherwise free to run alongside itself -- so the next
                // branch of this very route would happily pass beside this
                // terminal and turn it into a corner that drives nothing. Claim
                // the cells around it under a name nobody routes as.
                let guard = format!("terminal:{}.in[{input_index}]", netlist.gates[gate].output);
                for neighbour in horizontal_neighbours(socket) {
                    if neighbour != predecessor && neighbour != support {
                        reservation.insert(neighbour, &guard, Occupancy::Solid);
                    }
                }

                route.terminals.push(RouteTerminal {
                    sink: RouteSink {
                        gate: netlist.gates[gate].output.clone(),
                        input_index,
                        anchor: socket,
                    },
                    kind,
                    repeaters: trunk_repeaters + laid.repeaters,
                });
            }
            NetConsumer::Terminal {
                port, at, toward, ..
            } => {
                // The contract's one delivery repeater, whatever the
                // shared-trunk overlap did: aimed along `toward` so its output
                // lands in the caller's cell, and written here so the plan says
                // so at the handover even when the trunk already owned it.
                // `place_output_terminal` writes the same blockstate.
                if let Some(index) = route.anchors.iter().position(|anchor| *anchor == socket) {
                    route.realisation[index] = compile::repeater(*toward);
                }
                // A diode reads only its rear, so the same straight-line rule a
                // gate socket gets applies here: claim the cells flanking the
                // handover under a name nobody routes as, or a later branch of
                // this very net could run alongside and lock the repeater.
                let guard = format!("terminal:output:{port}");
                for neighbour in horizontal_neighbours(socket) {
                    if neighbour != predecessor && neighbour != *at {
                        reservation.insert(neighbour, &guard, Occupancy::Solid);
                    }
                }
                route.terminals.push(RouteTerminal {
                    sink: RouteSink {
                        gate: port.clone(),
                        input_index: 0,
                        anchor: socket,
                    },
                    kind: RouteTerminalKind::OutputTerminalRepeater,
                    repeaters: trunk_repeaters + laid.repeaters,
                });
            }
        }

        // THE RING RULE. A branch that closes a cycle through one of this
        // route's own repeaters has built a latch, and a latch is a different
        // circuit: pre-lit repeaters (`emit` places every one lit) keep it
        // powered at every input vector. Game physics, so a refusal under
        // both routers -- see [`ring_closed_in`] for the measured case and
        // the edges walked. Checked after every branch so the branch that
        // closed the ring is the one refused and charged.
        if let Some((repeater, ring)) = ring_closed_in(&route, reservation) {
            let mut charged: Vec<Anchor> = path[shared..]
                .iter()
                .copied()
                .filter(|cell| ring.contains(cell))
                .collect();
            if charged.is_empty() {
                // The closure always involves cells this branch laid, but if
                // bookkeeping ever disagrees, charge the whole branch rather
                // than nothing: an uncharged refusal repeats forever.
                charged = path[shared..].to_vec();
            }
            return Err(Box::new(RoutingFailure {
                blocked: signal.clone(),
                corridor: (source, approach),
                reservation: reservation.clone(),
                charge_outright: charged,
                is_decay: false,
                error: PlannerError::PhysicalInvariant(
                    compile::CompileError::CandidateMetadataViolation {
                        item: signal.clone(),
                        reason: format!(
                            "the branch to {sink_label} closes a ring: the repeater \
                             at ({}, {}, {}) reaches its own input cell through this net's \
                             own cells, and a route that feeds its own repeater input is a \
                             latch, not a wire",
                            repeater.x, repeater.y, repeater.z
                        ),
                    },
                ),
            }));
        }
        route.branch_paths.push(path);
    }
    Ok(route)
}

/// Historical compatibility control through the production typed adapter.
/// Ordinary generation chooses its own reserve explicitly at its call sites.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]

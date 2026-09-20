use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
};

use thiserror::Error;

use crate::compile::fragment_synth::identity::PhysicalEndpointId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetObligation<T> {
    pub target: T,
    pub promoted: bool,
    pub structural_slack_ticks: u64,
    pub forward_distance: u64,
    pub key: (u8, u32, u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteObligation<T> {
    pub source: PhysicalEndpointId,
    pub must_precede: BTreeSet<PhysicalEndpointId>,
    pub boundary_escape: bool,
    pub structural_slack_ticks: u64,
    pub fanout: usize,
    pub level_span: u64,
    pub targets: Vec<TargetObligation<T>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScheduledRoute<T> {
    pub source: PhysicalEndpointId,
    pub targets: Vec<T>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteSchedule<T> {
    pub routes: Vec<ScheduledRoute<T>>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub(crate) enum RouteScheduleError {
    #[error("duplicate route obligation for endpoint {endpoint:?}")]
    DuplicateEndpoint { endpoint: PhysicalEndpointId },
    #[error("route {route_source:?} must precede unknown endpoint {referenced:?}")]
    UnknownEndpoint {
        route_source: PhysicalEndpointId,
        referenced: PhysicalEndpointId,
    },
    #[error("route {route_source:?} cannot precede itself")]
    SelfPrecedence { route_source: PhysicalEndpointId },
    #[error("route precedence cycle leaves blocked endpoints {remaining:?}")]
    PrecedenceCycle { remaining: Vec<PhysicalEndpointId> },
}

impl<T> RouteSchedule<T> {
    pub fn build(mut obligations: Vec<RouteObligation<T>>) -> Result<Self, RouteScheduleError> {
        obligations.sort_by_key(|route| {
            (
                Reverse(route.boundary_escape),
                route.structural_slack_ticks,
                Reverse(route.fanout),
                Reverse(route.level_span),
                route.source,
            )
        });

        let mut source_indices = BTreeMap::new();
        for (index, route) in obligations.iter().enumerate() {
            if source_indices.insert(route.source, index).is_some() {
                return Err(RouteScheduleError::DuplicateEndpoint {
                    endpoint: route.source,
                });
            }
        }

        let mut outgoing = vec![Vec::new(); obligations.len()];
        let mut indegree = vec![0usize; obligations.len()];
        for (source_index, route) in obligations.iter().enumerate() {
            for referenced in &route.must_precede {
                if *referenced == route.source {
                    return Err(RouteScheduleError::SelfPrecedence {
                        route_source: route.source,
                    });
                }
                let Some(&target_index) = source_indices.get(referenced) else {
                    return Err(RouteScheduleError::UnknownEndpoint {
                        route_source: route.source,
                        referenced: *referenced,
                    });
                };
                outgoing[source_index].push(target_index);
                indegree[target_index] += 1;
            }
        }

        let mut ready = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, degree)| (*degree == 0).then_some(index))
            .collect::<BTreeSet<_>>();
        let mut topological_order = Vec::with_capacity(obligations.len());
        while let Some(index) = ready.iter().next().copied() {
            ready.remove(&index);
            topological_order.push(index);
            for &target_index in &outgoing[index] {
                indegree[target_index] -= 1;
                if indegree[target_index] == 0 {
                    ready.insert(target_index);
                }
            }
        }

        if topological_order.len() != obligations.len() {
            let remaining = indegree
                .iter()
                .enumerate()
                .filter_map(|(index, degree)| (*degree > 0).then_some(obligations[index].source))
                .collect();
            return Err(RouteScheduleError::PrecedenceCycle { remaining });
        }

        let mut ranks = vec![0usize; obligations.len()];
        for (rank, index) in topological_order.into_iter().enumerate() {
            ranks[index] = rank;
        }
        let mut ranked = obligations.into_iter().enumerate().collect::<Vec<_>>();
        ranked.sort_by_key(|(index, _)| ranks[*index]);
        let routes = ranked
            .into_iter()
            .map(|(_, route)| {
                let mut targets = route.targets;
                targets.sort_by_key(|target| {
                    (
                        Reverse(target.promoted),
                        target.structural_slack_ticks,
                        Reverse(target.forward_distance),
                        target.key,
                    )
                });
                ScheduledRoute {
                    source: route.source,
                    targets: targets.into_iter().map(|target| target.target).collect(),
                }
            })
            .collect();
        Ok(Self { routes })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{RouteObligation, RouteSchedule, RouteScheduleError, TargetObligation};
    use crate::compile::fragment_synth::identity::{
        InstanceId, PhysicalEndpointId, PortId, PrimitiveId, TopologyNodeId,
    };

    fn source(instance: u32) -> PhysicalEndpointId {
        PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(instance),
            node: TopologyNodeId(0),
        })
    }

    fn route(
        source: PhysicalEndpointId,
        pinned: bool,
        slack: u64,
        fanout: usize,
        span: u64,
    ) -> RouteObligation<u32> {
        RouteObligation {
            source,
            must_precede: BTreeSet::new(),
            boundary_escape: pinned,
            structural_slack_ticks: slack,
            fanout,
            level_span: span,
            targets: vec![target(0, slack, span, (0, 0, 0))],
        }
    }

    fn target(value: u32, slack: u64, distance: u64, key: (u8, u32, u16)) -> TargetObligation<u32> {
        TargetObligation {
            target: value,
            promoted: false,
            structural_slack_ticks: slack,
            forward_distance: distance,
            key,
        }
    }

    fn promoted_target(
        value: u32,
        slack: u64,
        distance: u64,
        key: (u8, u32, u16),
    ) -> TargetObligation<u32> {
        TargetObligation {
            promoted: true,
            ..target(value, slack, distance, key)
        }
    }

    fn schedule<T>(obligations: Vec<RouteObligation<T>>) -> RouteSchedule<T> {
        RouteSchedule::build(obligations).expect("test schedule should be valid")
    }

    #[test]
    fn route_priority_is_pinned_then_slack_fanout_span_and_source_id() {
        let obligations = vec![
            route(source(5), false, 2, 9, 40),
            route(source(4), false, 2, 9, 40),
            route(source(3), false, 2, 9, 20),
            route(source(2), false, 2, 3, 20),
            route(source(1), false, 8, 3, 20),
            route(PhysicalEndpointId::PrimaryInput(PortId(9)), true, 99, 1, 1),
        ];

        let schedule = schedule(obligations);
        let expected = vec![
            PhysicalEndpointId::PrimaryInput(PortId(9)),
            source(4),
            source(5),
            source(3),
            source(2),
            source(1),
        ];
        assert_eq!(
            schedule
                .routes
                .iter()
                .map(|route| route.source)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn one_precedence_constraint_moves_only_what_dependency_requires() {
        let pinned = PhysicalEndpointId::PrimaryInput(PortId(9));
        let mut repaired = route(source(1), false, 8, 3, 20);
        repaired.must_precede.insert(pinned);
        let schedule = schedule(vec![
            route(source(5), false, 2, 9, 40),
            route(source(4), false, 2, 9, 40),
            repaired,
            route(pinned, true, 99, 1, 1),
        ]);

        assert_eq!(
            schedule
                .routes
                .iter()
                .map(|route| route.source)
                .collect::<Vec<_>>(),
            vec![source(4), source(5), source(1), pinned],
        );
    }

    #[test]
    fn precedence_constraints_accumulate_into_a_chain() {
        let mut first = route(source(1), false, 9, 1, 1);
        let mut second = route(source(2), false, 8, 1, 1);
        first.must_precede.insert(source(2));
        second.must_precede.insert(source(3));

        let schedule = schedule(vec![
            route(source(3), false, 0, 1, 1),
            second,
            first,
            route(source(4), false, 4, 1, 1),
        ]);

        assert_eq!(
            schedule
                .routes
                .iter()
                .map(|route| route.source)
                .collect::<Vec<_>>(),
            vec![source(4), source(1), source(2), source(3)],
        );
    }

    #[test]
    fn available_routes_keep_the_canonical_priority_tie_break() {
        let mut constrained = route(source(1), false, 8, 1, 1);
        constrained.must_precede.insert(source(3));
        let schedule = schedule(vec![
            route(source(2), false, 2, 1, 1),
            route(source(3), false, 0, 1, 1),
            constrained,
            route(source(4), false, 1, 1, 1),
        ]);

        assert_eq!(
            schedule
                .routes
                .iter()
                .map(|route| route.source)
                .collect::<Vec<_>>(),
            vec![source(4), source(2), source(1), source(3)],
        );
    }

    #[test]
    fn fanout_targets_use_slack_then_decreasing_forward_distance_then_key() {
        let obligation = RouteObligation {
            source: source(0),
            must_precede: BTreeSet::new(),
            boundary_escape: false,
            structural_slack_ticks: 0,
            fanout: 4,
            level_span: 30,
            targets: vec![
                target(4, 2, 10, (0, 4, 0)),
                target(32, 1, 20, (0, 32, 0)),
                target(35, 1, 30, (0, 35, 0)),
                target(34, 1, 30, (0, 34, 0)),
            ],
        };
        let schedule = schedule(vec![obligation]);
        assert_eq!(schedule.routes[0].targets, vec![34, 35, 32, 4]);
    }

    #[test]
    fn promoted_sink_leads_only_its_own_route_tree() {
        let route_a = RouteObligation {
            source: source(1),
            must_precede: BTreeSet::new(),
            boundary_escape: false,
            structural_slack_ticks: 0,
            fanout: 3,
            level_span: 10,
            targets: vec![
                target(10, 2, 5, (0, 10, 0)),
                target(11, 1, 5, (0, 11, 0)),
                target(12, 1, 9, (0, 12, 0)),
            ],
        };
        let route_b = RouteObligation {
            source: source(2),
            must_precede: BTreeSet::new(),
            boundary_escape: false,
            structural_slack_ticks: 3,
            fanout: 2,
            level_span: 10,
            targets: vec![target(20, 1, 4, (0, 20, 0)), target(21, 0, 4, (0, 21, 0))],
        };

        let baseline = schedule(vec![route_a.clone(), route_b.clone()]);
        assert_eq!(baseline.routes[0].targets, vec![12, 11, 10]);

        let mut promoted_route_a = route_a;
        promoted_route_a.targets[0] = promoted_target(10, 2, 5, (0, 10, 0));
        let schedule = schedule(vec![promoted_route_a, route_b]);

        assert_eq!(schedule.routes[0].targets, vec![10, 12, 11]);
        assert_eq!(
            schedule
                .routes
                .iter()
                .map(|route| route.source)
                .collect::<Vec<_>>(),
            baseline
                .routes
                .iter()
                .map(|route| route.source)
                .collect::<Vec<_>>(),
        );
        assert_eq!(schedule.routes[1], baseline.routes[1]);
    }

    #[test]
    fn all_unpromoted_targets_keep_the_existing_schedule() {
        let obligations = vec![
            RouteObligation {
                source: source(1),
                must_precede: BTreeSet::new(),
                boundary_escape: false,
                structural_slack_ticks: 0,
                fanout: 4,
                level_span: 30,
                targets: vec![
                    target(4, 2, 10, (0, 4, 0)),
                    target(32, 1, 20, (0, 32, 0)),
                    target(35, 1, 30, (0, 35, 0)),
                    target(34, 1, 30, (0, 34, 0)),
                ],
            },
            RouteObligation {
                source: source(2),
                must_precede: BTreeSet::new(),
                boundary_escape: false,
                structural_slack_ticks: 5,
                fanout: 2,
                level_span: 4,
                targets: vec![target(7, 3, 2, (0, 7, 0)), target(6, 3, 8, (0, 6, 0))],
            },
        ];

        let schedule = schedule(obligations);

        assert_eq!(
            schedule
                .routes
                .iter()
                .map(|route| route.source)
                .collect::<Vec<_>>(),
            vec![source(1), source(2)],
        );
        assert_eq!(schedule.routes[0].targets, vec![34, 35, 32, 4]);
        assert_eq!(schedule.routes[1].targets, vec![6, 7]);
    }

    #[test]
    fn reversing_insertion_order_stays_canonical_with_promotion() {
        let forward = vec![RouteObligation {
            source: source(1),
            must_precede: BTreeSet::new(),
            boundary_escape: false,
            structural_slack_ticks: 0,
            fanout: 4,
            level_span: 12,
            targets: vec![
                target(40, 1, 9, (0, 40, 0)),
                promoted_target(41, 6, 1, (0, 41, 0)),
                target(42, 1, 2, (0, 42, 0)),
                promoted_target(43, 2, 7, (0, 43, 0)),
            ],
        }];
        let mut reversed = forward.clone();
        for route in &mut reversed {
            route.targets.reverse();
        }

        let forward_schedule = schedule(forward);
        assert_eq!(forward_schedule.routes[0].targets, vec![43, 41, 40, 42]);
        assert_eq!(forward_schedule, schedule(reversed));
    }

    #[test]
    fn reversing_insertion_order_keeps_the_complete_schedule_identical() {
        let mut forward = vec![
            route(source(4), false, 2, 2, 8),
            route(source(2), false, 0, 3, 20),
            route(PhysicalEndpointId::PrimaryInput(PortId(0)), true, 4, 1, 3),
        ];
        forward[1].targets = vec![
            target(4, 2, 10, (0, 4, 0)),
            target(35, 1, 30, (0, 35, 0)),
            target(32, 1, 20, (0, 32, 0)),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();
        for route in &mut reversed {
            route.targets.reverse();
        }
        assert_eq!(schedule(forward), schedule(reversed));
    }

    #[test]
    fn unknown_precedence_endpoint_is_rejected_with_evidence() {
        let mut obligation = route(source(1), false, 0, 1, 1);
        obligation.must_precede.insert(source(99));
        assert_eq!(
            RouteSchedule::build(vec![obligation]),
            Err(RouteScheduleError::UnknownEndpoint {
                route_source: source(1),
                referenced: source(99),
            })
        );
    }

    #[test]
    fn self_precedence_is_rejected() {
        let mut obligation = route(source(1), false, 0, 1, 1);
        obligation.must_precede.insert(source(1));
        assert_eq!(
            RouteSchedule::build(vec![obligation]),
            Err(RouteScheduleError::SelfPrecedence {
                route_source: source(1)
            })
        );
    }

    #[test]
    fn precedence_cycle_is_rejected_in_canonical_order() {
        let mut first = route(source(1), false, 8, 1, 1);
        let mut second = route(source(2), false, 0, 1, 1);
        first.must_precede.insert(source(2));
        second.must_precede.insert(source(1));
        assert_eq!(
            RouteSchedule::build(vec![first, second]),
            Err(RouteScheduleError::PrecedenceCycle {
                remaining: vec![source(2), source(1)],
            })
        );
    }
}

use std::cmp::Reverse;

use crate::compile::fragment_synth::identity::PhysicalEndpointId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetObligation<T> {
    pub target: T,
    pub promoted: bool,
    pub structural_slack_ticks: u64,
    pub forward_distance: u64,
    /// Lateral distance between the sink's row and the source's row.  A sink
    /// off the source row needs the channel lane; it goes first so the trunk
    /// is on the lane before any same-row sink is served from it.
    pub lateral_distance: u64,
    /// Manhattan distance from the placed source anchor to the placed sink
    /// terminal; among equal levels the farthest sink lays the trunk first.
    pub physical_distance: u64,
    pub key: (u8, u32, u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteObligation<T> {
    pub source: PhysicalEndpointId,
    pub pinned_boundary_escape: bool,
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

impl<T> RouteSchedule<T> {
    pub fn build(mut obligations: Vec<RouteObligation<T>>) -> Self {
        obligations.sort_by_key(|route| {
            (
                Reverse(route.pinned_boundary_escape),
                route.structural_slack_ticks,
                Reverse(route.fanout),
                Reverse(route.level_span),
                route.source,
            )
        });
        let routes = obligations
            .into_iter()
            .map(|route| {
                let mut targets = route.targets;
                targets.sort_by_key(|target| {
                    (
                        Reverse(target.promoted),
                        // A sink off the source row establishes the channel
                        // lane; every same-row sink is served from that lane
                        // afterwards, whatever its slack.
                        Reverse(target.lateral_distance > 0),
                        target.structural_slack_ticks,
                        Reverse(target.forward_distance),
                        Reverse(target.physical_distance),
                        target.key,
                    )
                });
                ScheduledRoute {
                    source: route.source,
                    targets: targets.into_iter().map(|target| target.target).collect(),
                }
            })
            .collect();
        Self { routes }
    }
}

#[cfg(test)]
mod tests {
    use super::{RouteObligation, RouteSchedule, TargetObligation};
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
            pinned_boundary_escape: pinned,
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
            lateral_distance: 0,
            physical_distance: 0,
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

        let schedule = RouteSchedule::build(obligations);
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
    fn fanout_targets_use_slack_then_decreasing_forward_distance_then_key() {
        let obligation = RouteObligation {
            source: source(0),
            pinned_boundary_escape: false,
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
        let schedule = RouteSchedule::build(vec![obligation]);
        assert_eq!(schedule.routes[0].targets, vec![34, 35, 32, 4]);
    }

    #[test]
    fn promoted_sink_leads_only_its_own_route_tree() {
        let route_a = RouteObligation {
            source: source(1),
            pinned_boundary_escape: false,
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
            pinned_boundary_escape: false,
            structural_slack_ticks: 3,
            fanout: 2,
            level_span: 10,
            targets: vec![target(20, 1, 4, (0, 20, 0)), target(21, 0, 4, (0, 21, 0))],
        };

        let baseline = RouteSchedule::build(vec![route_a.clone(), route_b.clone()]);
        assert_eq!(baseline.routes[0].targets, vec![12, 11, 10]);

        let mut promoted_route_a = route_a;
        promoted_route_a.targets[0] = promoted_target(10, 2, 5, (0, 10, 0));
        let schedule = RouteSchedule::build(vec![promoted_route_a, route_b]);

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
                pinned_boundary_escape: false,
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
                pinned_boundary_escape: false,
                structural_slack_ticks: 5,
                fanout: 2,
                level_span: 4,
                targets: vec![target(7, 3, 2, (0, 7, 0)), target(6, 3, 8, (0, 6, 0))],
            },
        ];

        let schedule = RouteSchedule::build(obligations);

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
            pinned_boundary_escape: false,
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

        let schedule = RouteSchedule::build(forward);
        assert_eq!(schedule.routes[0].targets, vec![43, 41, 40, 42]);
        assert_eq!(schedule, RouteSchedule::build(reversed));
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
        assert_eq!(
            RouteSchedule::build(forward),
            RouteSchedule::build(reversed)
        );
    }
}

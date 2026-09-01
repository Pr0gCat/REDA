use std::cmp::Reverse;

use crate::compile::fragment_synth::identity::PhysicalEndpointId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetObligation<T> {
    pub target: T,
    pub structural_slack_ticks: u64,
    pub forward_distance: u64,
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
            structural_slack_ticks: slack,
            forward_distance: distance,
            key,
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

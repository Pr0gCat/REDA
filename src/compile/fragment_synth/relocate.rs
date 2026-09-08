//! Moving and renumbering an already-compiled candidate.
//!
//! A block is compiled once as an [`ExpandedPhysicalCandidate`] full of
//! absolute Minecraft coordinates and identities scoped to its own
//! [`InstanceGraph`](crate::compile::fragment_synth::instance_graph::InstanceGraph).
//! Stamping that block at a parent instance needs two independent
//! operations: sliding every coordinate by a fixed offset ([`translate`]),
//! and rewriting every identity into the parent's numbering space
//! ([`renumber`]). Nothing else in `fragment_synth` performs either.
//!
//! `translate` and [`anchors_of`] are deliberately built from one shared
//! walker, [`for_each_anchor`], so a field neither one visits cannot go
//! unnoticed by the other: `anchors_of` is also what a future bounding-box
//! computation over a block's footprint relies on being exhaustive.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::compile::fragment_synth::candidate::ExpandedPhysicalCandidate;
use crate::compile::fragment_synth::identity::{
    ConnectionId, InstanceId, ObservationId, PhysicalEndpointId, PrimitiveId, RouteId, RoutedSinkId,
};
use crate::compile::fragment_synth::instance_graph::{
    InstanceDriver, PhysicalDriver, PhysicalSink,
};
use crate::compile::fragment_synth::topology::{
    ConnectionSource, ConnectionTarget, ContributorSpec, OutputSpec,
};
use crate::compile::geometry::Anchor;
use crate::compile::planner::PortPlacements;
use crate::compile::routing::{DelayedOwner, RealisedRouteTree, RouteTarget};

/// A fixed displacement applied to every coordinate a candidate owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Offset {
    pub dx: i32,
    pub dy: i32,
    pub dz: i32,
}

impl Offset {
    fn shift(self, anchor: Anchor) -> Anchor {
        Anchor {
            x: anchor.x + self.dx,
            y: anchor.y + self.dy,
            z: anchor.z + self.dz,
        }
    }
}

/// How to renumber a candidate's identities into a parent's numbering space.
///
/// `instances` must cover every [`InstanceId`] the candidate uses -- an
/// instance missing from the map is a [`RelocateError::UnmappedInstance`].
/// `route_offset` is added to every [`RouteId`] rather than looked up in a
/// map, since routes have no meaning outside the candidate that owns them
/// and a parent only needs their numbers to stop colliding with its own.
/// `PortId`s are not renumbered here: they name a block's own declared
/// interface, which stamping resolves separately.
#[derive(Debug, Clone, Default)]
pub(crate) struct IdMap {
    pub instances: BTreeMap<InstanceId, InstanceId>,
    pub route_offset: u32,
}

impl IdMap {
    fn instance(&self, id: InstanceId) -> Result<InstanceId, RelocateError> {
        self.instances
            .get(&id)
            .copied()
            .ok_or(RelocateError::UnmappedInstance(id))
    }

    fn route(&self, id: RouteId) -> Result<RouteId, RelocateError> {
        id.0.checked_add(self.route_offset)
            .map(RouteId)
            .ok_or(RelocateError::RouteIdOverflow(id))
    }

    fn primitive(&self, id: PrimitiveId) -> Result<PrimitiveId, RelocateError> {
        Ok(PrimitiveId {
            instance: self.instance(id.instance)?,
            node: id.node,
        })
    }

    fn connection(&self, id: ConnectionId) -> Result<ConnectionId, RelocateError> {
        Ok(match id {
            ConnectionId::External {
                instance,
                input_index,
            } => ConnectionId::External {
                instance: self.instance(instance)?,
                input_index,
            },
            ConnectionId::Internal {
                instance,
                edge_index,
            } => ConnectionId::Internal {
                instance: self.instance(instance)?,
                edge_index,
            },
        })
    }

    fn endpoint(&self, id: PhysicalEndpointId) -> Result<PhysicalEndpointId, RelocateError> {
        Ok(match id {
            PhysicalEndpointId::PrimaryInput(port) => PhysicalEndpointId::PrimaryInput(port),
            PhysicalEndpointId::DeclaredOutput(port) => PhysicalEndpointId::DeclaredOutput(port),
            PhysicalEndpointId::PrimitiveOutput(primitive) => {
                PhysicalEndpointId::PrimitiveOutput(self.primitive(primitive)?)
            }
            PhysicalEndpointId::Landing(connection) => {
                PhysicalEndpointId::Landing(self.connection(connection)?)
            }
            PhysicalEndpointId::Junction(instance) => {
                PhysicalEndpointId::Junction(self.instance(instance)?)
            }
        })
    }

    fn observation(&self, id: ObservationId) -> Result<ObservationId, RelocateError> {
        Ok(match id {
            ObservationId::PrimaryInput(port) => ObservationId::PrimaryInput(port),
            ObservationId::PrimitiveOutput(primitive) => {
                ObservationId::PrimitiveOutput(self.primitive(primitive)?)
            }
            ObservationId::InstanceOutput(instance) => {
                ObservationId::InstanceOutput(self.instance(instance)?)
            }
            ObservationId::JunctionOutput(instance) => {
                ObservationId::JunctionOutput(self.instance(instance)?)
            }
            ObservationId::DeclaredOutput(port) => ObservationId::DeclaredOutput(port),
        })
    }

    fn routed_sink(&self, sink: RoutedSinkId) -> Result<RoutedSinkId, RelocateError> {
        Ok(RoutedSinkId {
            route: self.route(sink.route)?,
            ordinal: sink.ordinal,
        })
    }

    fn delayed_owner(&self, owner: DelayedOwner) -> Result<DelayedOwner, RelocateError> {
        Ok(match owner {
            DelayedOwner::Primitive(primitive) => {
                DelayedOwner::Primitive(self.primitive(primitive)?)
            }
            DelayedOwner::Route(route) => DelayedOwner::Route(self.route(route)?),
            DelayedOwner::InputBinding(port) => DelayedOwner::InputBinding(port),
        })
    }

    fn route_target(&self, target: RouteTarget) -> Result<RouteTarget, RelocateError> {
        Ok(match target {
            RouteTarget::Connection(connection) => {
                RouteTarget::Connection(self.connection(connection)?)
            }
            RouteTarget::DeclaredOutput(port) => RouteTarget::DeclaredOutput(port),
        })
    }

    fn contributor(&self, contributor: ContributorSpec) -> Result<ContributorSpec, RelocateError> {
        Ok(match contributor {
            ContributorSpec::Landing(connection) => {
                ContributorSpec::Landing(self.connection(connection)?)
            }
            ContributorSpec::Primitive(primitive) => {
                ContributorSpec::Primitive(self.primitive(primitive)?)
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum RelocateError {
    #[error("instance {0:?} is not present in the renumbering map")]
    UnmappedInstance(InstanceId),
    #[error("route {0:?} overflows u32 after adding the route offset")]
    RouteIdOverflow(RouteId),
}

/// Visits every `Anchor` an [`ExpandedPhysicalCandidate`] owns, in a fixed
/// order, letting `f` rewrite each one in place.
///
/// This is the one place that knows the complete list of anchor-carrying
/// fields; [`translate`] and [`anchors_of`] both go through it so they
/// cannot disagree about which fields exist. See the module list in
/// `ExpandedPhysicalCandidate` (`candidate.rs`) for why each field below is
/// here: everything the candidate's `fingerprint()` walks that carries a
/// coordinate is visited exactly once.
fn for_each_anchor(candidate: &mut ExpandedPhysicalCandidate, f: &mut dyn FnMut(&mut Anchor)) {
    for placement in candidate.placements.values_mut() {
        f(&mut placement.anchor);
        if let Some(delayed) = &mut placement.delayed {
            f(&mut delayed.at);
        }
        for block in &mut placement.blocks {
            f(&mut block.at);
        }
    }
    for boundary in candidate.boundaries.values_mut() {
        if let Some(delayed) = &mut boundary.delayed {
            f(&mut delayed.at);
        }
        for block in &mut boundary.blocks {
            f(&mut block.at);
        }
    }
    for route in candidate.routes.values_mut() {
        for_each_route_anchor(route, f);
    }
    for junction in candidate.junctions.values_mut() {
        f(&mut junction.at);
        for block in &mut junction.cells {
            f(&mut block.at);
        }
    }
    for observation in candidate.observations.values_mut() {
        f(&mut observation.site.at);
    }
    // `PortPlacements` keeps its map private, so a pin's `at` cannot be
    // borrowed mutably in place -- rebuild the map from shifted copies
    // instead, through its own `pin` constructor.
    let mut relocated_pins = PortPlacements::default();
    for (name, pin) in candidate.pins.iter() {
        let mut at = pin.at;
        f(&mut at);
        relocated_pins.pin(name.clone(), at, pin.toward);
    }
    candidate.pins = relocated_pins;
    for pin in candidate.pin_contracts.values_mut() {
        f(&mut pin.at);
    }
}

/// The per-route half of [`for_each_anchor`], shared with [`translate_route`]
/// so a standalone route (not yet folded into a candidate) moves exactly the
/// same way a route inside a candidate does.
fn for_each_route_anchor(tree: &mut RealisedRouteTree, f: &mut dyn FnMut(&mut Anchor)) {
    for block in &mut tree.cells {
        f(&mut block.at);
    }
    for block in &mut tree.floors {
        f(&mut block.at);
    }
    for branch in &mut tree.branches {
        f(&mut branch.root);
        for at in &mut branch.path {
            f(at);
        }
        f(&mut branch.terminal.at);
    }
}

/// Shifts every coordinate `candidate` owns by `offset`, and nothing else --
/// no identity changes shape or value.
pub(crate) fn translate(candidate: &mut ExpandedPhysicalCandidate, offset: Offset) {
    for_each_anchor(candidate, &mut |anchor| *anchor = offset.shift(*anchor));
}

/// Shifts every coordinate a single realised route tree owns by `offset`.
///
/// `translate` never needs this itself -- it walks routes through
/// [`for_each_anchor`] like everything else a candidate owns. This is for a
/// future caller that has a `RealisedRouteTree` on its own, outside a
/// candidate (e.g. stamping a route while assembling a junction).
#[allow(dead_code)]
pub(crate) fn translate_route(tree: &mut RealisedRouteTree, offset: Offset) {
    for_each_route_anchor(tree, &mut |anchor| *anchor = offset.shift(*anchor));
}

/// Every anchor `candidate` owns, in the same fixed order [`translate`]
/// visits them in. Used to compute a block's bounding box, and to prove
/// `translate` is exhaustive (see `relocate::tests`).
pub(crate) fn anchors_of(candidate: &ExpandedPhysicalCandidate) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    // Route through the same mutating walker as `translate` -- cloning the
    // candidate to observe it, rather than duplicating the field list in a
    // second, read-only walker that could quietly drift from the first.
    let mut probe = candidate.clone();
    for_each_anchor(&mut probe, &mut |anchor| anchors.push(*anchor));
    anchors
}

/// Rewrites every instance and route identity `candidate` carries according
/// to `map`, rebuilding every map keyed by one of them so keys and values
/// never disagree. `PortId`s are untouched. Fails on the first instance
/// `map` does not cover.
///
/// Precondition the caller must know about: the renumbered candidate's
/// `instances` field (an [`InstanceGraph`](crate::compile::fragment_synth::instance_graph::InstanceGraph))
/// carries a STALE `ValidatedTopology.fingerprint` on every
/// `expanded.topology`. That fingerprint is a cached hash over a payload
/// containing `PrimitiveId`/`ConnectionId` values
/// (`crate::compile::fragment_synth::topology`'s `finish_topology` and its
/// private `FingerprintPayload`), and `renumber` has no way to recompute it
/// from outside that module -- it can only rewrite the ids the hash was
/// already taken over. `verify.rs:274` compares this field structurally. A
/// caller that needs a verifiable graph after renumbering must rebuild it
/// with `InstanceGraph::one_to_one_with_implementations` rather than reusing
/// the one `renumber` produced.
pub(crate) fn renumber(
    candidate: &mut ExpandedPhysicalCandidate,
    map: &IdMap,
) -> Result<(), RelocateError> {
    renumber_instance_graph(candidate, map)?;
    renumber_placements(candidate, map)?;
    renumber_boundaries(candidate, map)?;
    renumber_connections(candidate, map)?;
    renumber_routes(candidate, map)?;
    renumber_junctions(candidate, map)?;
    renumber_observations(candidate, map)?;
    Ok(())
}

fn renumber_instance_graph(
    candidate: &mut ExpandedPhysicalCandidate,
    map: &IdMap,
) -> Result<(), RelocateError> {
    for instance in &mut candidate.instances.instances {
        instance.id = map.instance(instance.id)?;
        instance.expanded.instance = map.instance(instance.expanded.instance)?;
        for primitive in &mut instance.expanded.topology.primitives {
            primitive.id = map.primitive(primitive.id)?;
        }
        for connection in &mut instance.expanded.topology.connections {
            connection.id = map.connection(connection.id)?;
            connection.source = match connection.source {
                ConnectionSource::ExternalInput { input_index } => {
                    ConnectionSource::ExternalInput { input_index }
                }
                ConnectionSource::Primitive(primitive) => {
                    ConnectionSource::Primitive(map.primitive(primitive)?)
                }
            };
            connection.target = match connection.target {
                ConnectionTarget::Primitive(primitive) => {
                    ConnectionTarget::Primitive(map.primitive(primitive)?)
                }
                ConnectionTarget::Junction(instance) => {
                    ConnectionTarget::Junction(map.instance(instance)?)
                }
            };
        }
        instance.expanded.topology.output = match &instance.expanded.topology.output {
            OutputSpec::Primitive(primitive) => OutputSpec::Primitive(map.primitive(*primitive)?),
            OutputSpec::Junction {
                logical_owner,
                contributors,
            } => OutputSpec::Junction {
                logical_owner: map.instance(*logical_owner)?,
                contributors: contributors
                    .iter()
                    .map(|contributor| map.contributor(*contributor))
                    .collect::<Result<Vec<_>, _>>()?,
            },
        };
    }
    for assignment in &mut candidate.instances.assignments {
        assignment.sink = match assignment.sink {
            PhysicalSink::InstanceInput {
                instance,
                input_index,
            } => PhysicalSink::InstanceInput {
                instance: map.instance(instance)?,
                input_index,
            },
            PhysicalSink::DeclaredOutput(port) => PhysicalSink::DeclaredOutput(port),
        };
        assignment.driver = match &assignment.driver {
            PhysicalDriver::PrimaryInput(port) => PhysicalDriver::PrimaryInput(*port),
            PhysicalDriver::Instance(InstanceDriver::Primitive {
                logical_owner,
                terminals,
            }) => PhysicalDriver::Instance(InstanceDriver::Primitive {
                logical_owner: map.instance(*logical_owner)?,
                terminals: terminals
                    .iter()
                    .map(|terminal| map.primitive(*terminal))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            PhysicalDriver::Instance(InstanceDriver::Junction {
                logical_owner,
                contributors,
            }) => PhysicalDriver::Instance(InstanceDriver::Junction {
                logical_owner: map.instance(*logical_owner)?,
                contributors: contributors
                    .iter()
                    .map(|contributor| map.contributor(*contributor))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
        };
    }
    // `InstanceGraph::assignments` must stay sorted by `PhysicalSink`
    // (`InstanceGraph::validate`'s canonical-order check) and the sort key
    // for an `InstanceInput` sink embeds the instance id we just rewrote
    // above. `map` is not required to be monotonic, so re-sort rather than
    // assume the existing order still holds -- mirroring the
    // `assignments.sort_by_key(...)` at the end of `InstanceGraph::with_variants`.
    candidate
        .instances
        .assignments
        .sort_by_key(|assignment| assignment.sink);
    // `instances` does NOT need the same treatment. Its sort key is
    // `(logical_gate, role, id)`, but `validate` also requires every
    // `(logical_gate, role)` pair to be unique (`DuplicateInstanceRole`), and
    // neither `logical_gate` nor `role` is touched by renumbering -- only
    // `id`. So any two instances already differ in one of the first two
    // components, the comparison never reaches `id`, and the vector's order
    // is unaffected by however `map` permutes ids.
    Ok(())
}

fn renumber_placements(
    candidate: &mut ExpandedPhysicalCandidate,
    map: &IdMap,
) -> Result<(), RelocateError> {
    let mut rebuilt = BTreeMap::new();
    for (_, mut placement) in std::mem::take(&mut candidate.placements) {
        placement.id = map.primitive(placement.id)?;
        if let Some(delayed) = &mut placement.delayed {
            delayed.owner = map.delayed_owner(delayed.owner)?;
        }
        rebuilt.insert(placement.id, placement);
    }
    candidate.placements = rebuilt;
    Ok(())
}

/// Remaps `boundary.delayed.owner` the same way [`renumber_placements`]
/// remaps a placement's -- `DelayedOwner::Primitive`/`DelayedOwner::Route`
/// both carry an identity `renumber` rewrites. `boundaries`' key
/// (`PhysicalEndpointId`, always `PrimaryInput`/`DeclaredOutput` here) is a
/// `PortId` and is never renumbered, so the map itself is never rebuilt --
/// only mutated in place.
fn renumber_boundaries(
    candidate: &mut ExpandedPhysicalCandidate,
    map: &IdMap,
) -> Result<(), RelocateError> {
    for boundary in candidate.boundaries.values_mut() {
        if let Some(delayed) = &mut boundary.delayed {
            delayed.owner = map.delayed_owner(delayed.owner)?;
        }
    }
    Ok(())
}

fn renumber_connections(
    candidate: &mut ExpandedPhysicalCandidate,
    map: &IdMap,
) -> Result<(), RelocateError> {
    let mut rebuilt = BTreeMap::new();
    for (_, mut binding) in std::mem::take(&mut candidate.connections) {
        binding.id = map.connection(binding.id)?;
        binding.source = map.endpoint(binding.source)?;
        binding.landing = map.endpoint(binding.landing)?;
        binding.route = map.route(binding.route)?;
        binding.sink = map.routed_sink(binding.sink)?;
        rebuilt.insert(binding.id, binding);
    }
    candidate.connections = rebuilt;
    Ok(())
}

fn renumber_routes(
    candidate: &mut ExpandedPhysicalCandidate,
    map: &IdMap,
) -> Result<(), RelocateError> {
    let mut rebuilt = BTreeMap::new();
    for (_, mut tree) in std::mem::take(&mut candidate.routes) {
        tree.id = map.route(tree.id)?;
        tree.source = map.endpoint(tree.source)?;
        for branch in &mut tree.branches {
            branch.sink = map.routed_sink(branch.sink)?;
            branch.target = map.route_target(branch.target)?;
            branch.terminal.sink = map.routed_sink(branch.terminal.sink)?;
            branch.terminal.delayed_owner = branch
                .terminal
                .delayed_owner
                .map(|owner| map.delayed_owner(owner))
                .transpose()?;
        }
        rebuilt.insert(tree.id, tree);
    }
    candidate.routes = rebuilt;
    Ok(())
}

fn renumber_junctions(
    candidate: &mut ExpandedPhysicalCandidate,
    map: &IdMap,
) -> Result<(), RelocateError> {
    let mut rebuilt = BTreeMap::new();
    for (_, mut junction) in std::mem::take(&mut candidate.junctions) {
        junction.id = map.instance(junction.id)?;
        for contributor in &mut junction.contributors {
            *contributor = map.endpoint(*contributor)?;
        }
        rebuilt.insert(junction.id, junction);
    }
    candidate.junctions = rebuilt;
    Ok(())
}

fn renumber_observations(
    candidate: &mut ExpandedPhysicalCandidate,
    map: &IdMap,
) -> Result<(), RelocateError> {
    let mut rebuilt = BTreeMap::new();
    for (_, mut observation) in std::mem::take(&mut candidate.observations) {
        observation.site.id = map.observation(observation.site.id)?;
        observation.site.logical_owner = observation
            .site
            .logical_owner
            .map(|owner| map.instance(owner))
            .transpose()?;
        rebuilt.insert(observation.site.id, observation);
    }
    candidate.observations = rebuilt;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::candidate::ExpandedPhysicalCandidate;

    fn full_adder_candidate() -> ExpandedPhysicalCandidate {
        crate::compile::fragment_synth::seed::tests::certified_full_adder()
            .candidate()
            .clone()
    }

    #[test]
    fn translate_moves_every_anchor_by_the_offset_and_nothing_else() {
        let before = full_adder_candidate();
        let mut after = before.clone();
        translate(
            &mut after,
            Offset {
                dx: 7,
                dy: 0,
                dz: -3,
            },
        );
        let a = anchors_of(&before);
        let b = anchors_of(&after);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!((y.x - x.x, y.y - x.y, y.z - x.z), (7, 0, -3));
        }
        // The anchor walk covers every field the fingerprint sees: translating
        // back restores the fingerprint exactly.
        translate(
            &mut after,
            Offset {
                dx: -7,
                dy: 0,
                dz: 3,
            },
        );
        assert_eq!(after.fingerprint(), before.fingerprint());
        // And a translated candidate is still a well-formed candidate.
        let mut moved = before.clone();
        translate(
            &mut moved,
            Offset {
                dx: 40,
                dy: 0,
                dz: 40,
            },
        );
        moved
            .validate_shape()
            .expect("translated candidate keeps its shape");
    }

    #[test]
    fn anchors_of_counts_the_fields_the_fingerprint_serialises() {
        let candidate = full_adder_candidate();
        let json = serde_json::to_string(&candidate.fingerprint_payload_for_test()).unwrap();
        // Every `"x":` in the payload is one Anchor (Anchor is the only
        // struct with an `x` field in the payload).
        let anchors_in_payload = json.matches("\"x\":").count();
        assert_eq!(anchors_of(&candidate).len(), anchors_in_payload);
    }

    #[test]
    fn renumbering_shifts_instances_and_routes_consistently() {
        let before = full_adder_candidate();
        let mut after = before.clone();
        let map = IdMap {
            instances: before
                .instances
                .instances
                .iter()
                .map(|i| (i.id, InstanceId(i.id.0 + 100)))
                .collect(),
            route_offset: 50,
        };
        renumber(&mut after, &map).expect("renumbers");
        assert!(after.placements.keys().all(|p| p.instance.0 >= 100));
        assert!(after.routes.keys().all(|r| r.0 >= 50));
        assert!(after
            .routes
            .values()
            .all(|t| t.branches.iter().all(|b| b.sink.route == t.id)));
        assert!(after
            .connections
            .values()
            .all(|c| c.sink.route == c.route && c.route.0 >= 50));
        assert_eq!(
            after.instances.instances.len(),
            before.instances.instances.len()
        );
        after
            .validate_shape()
            .expect("renumbered candidate keeps its shape");
    }

    #[test]
    fn renumber_remaps_boundary_delayed_owner() {
        use crate::compile::routing::DelayedComponent;

        let before = full_adder_candidate();
        let map = IdMap {
            instances: before
                .instances
                .instances
                .iter()
                .map(|i| (i.id, InstanceId(i.id.0 + 100)))
                .collect(),
            route_offset: 50,
        };
        let boundary_key = *before
            .boundaries
            .keys()
            .next()
            .expect("full adder candidate has at least one boundary");
        let primitive_id = *before
            .placements
            .keys()
            .next()
            .expect("full adder candidate has at least one placement");
        let route_id = *before
            .routes
            .keys()
            .next()
            .expect("full adder candidate has at least one route");

        // A boundary delayed on a primitive: `.owner` must follow the same
        // instance remapping `renumber_placements` already applies to a
        // placement's own `delayed.owner`.
        let mut primitive_owned = before.clone();
        primitive_owned
            .boundaries
            .get_mut(&boundary_key)
            .unwrap()
            .delayed = Some(DelayedComponent {
            at: Anchor { x: 0, y: 0, z: 0 },
            owner: DelayedOwner::Primitive(primitive_id),
        });
        renumber(&mut primitive_owned, &map).expect("renumbers");
        let expected_primitive = PrimitiveId {
            instance: InstanceId(primitive_id.instance.0 + 100),
            node: primitive_id.node,
        };
        assert_eq!(
            primitive_owned.boundaries[&boundary_key]
                .delayed
                .as_ref()
                .unwrap()
                .owner,
            DelayedOwner::Primitive(expected_primitive),
        );

        // A boundary delayed on a route: `.owner` must follow the same
        // route_offset every other `RouteId` in the candidate gets.
        let mut route_owned = before.clone();
        route_owned
            .boundaries
            .get_mut(&boundary_key)
            .unwrap()
            .delayed = Some(DelayedComponent {
            at: Anchor { x: 0, y: 0, z: 0 },
            owner: DelayedOwner::Route(route_id),
        });
        renumber(&mut route_owned, &map).expect("renumbers");
        assert_eq!(
            route_owned.boundaries[&boundary_key]
                .delayed
                .as_ref()
                .unwrap()
                .owner,
            DelayedOwner::Route(RouteId(route_id.0 + 50)),
        );
    }

    #[test]
    fn renumbering_resorts_assignments_after_a_non_monotonic_remap() {
        let before = full_adder_candidate();
        let mut after = before.clone();
        // Deliberately non-monotonic: reverses the ids. `IdMap`'s contract
        // never requires monotonicity, but the diff's own test above only
        // ever used `id.0 + 100`, which happens to be monotonic and so can
        // never exercise this.
        let ids: Vec<InstanceId> = before.instances.instances.iter().map(|i| i.id).collect();
        let map = IdMap {
            instances: ids.iter().copied().zip(ids.iter().copied().rev()).collect(),
            route_offset: 50,
        };
        renumber(&mut after, &map).expect("renumbers");

        let (netlist, _) = crate::circuits::full_adder::build_full_adder_netlist();
        let lowered =
            crate::compile::lowering::lower_optimised(&netlist).expect("full adder netlist lowers");
        after
            .instances
            .validate(&lowered)
            .expect("renumbered instance graph keeps canonical sink order");
    }
}

//! Stable typed identities for fragment synthesis.
//!
//! Coordinates and display labels deliberately do not appear in any identity
//! type.  They are mutable payload; these IDs are the durable keys shared by
//! topology, placement, routing, timing, verification, and the viewer.

use serde::{Deserialize, Serialize};

use crate::compile::geometry::Anchor;
pub use crate::compile::topology::LibraryEntryId;

macro_rules! scalar_id {
    ($name:ident, $inner:ty) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub struct $name(pub $inner);
    };
}

scalar_id!(GateIndex, u32);
scalar_id!(PortId, u32);
scalar_id!(InstanceId, u32);
scalar_id!(TopologyNodeId, u16);
scalar_id!(RouteId, u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InputMask(u64);

impl InputMask {
    pub const fn new(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, input_index: usize) -> bool {
        input_index < u64::BITS as usize && self.0 & (1u64 << input_index) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ImplementationKey {
    Library(LibraryEntryId),
    Merge { isolation_mask: InputMask },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PrimitiveId {
    pub instance: InstanceId,
    pub node: TopologyNodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConnectionId {
    External {
        instance: InstanceId,
        input_index: u16,
    },
    Internal {
        instance: InstanceId,
        edge_index: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RoutedSinkId {
    pub route: RouteId,
    pub ordinal: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PhysicalEndpointId {
    PrimaryInput(PortId),
    DeclaredOutput(PortId),
    PrimitiveOutput(PrimitiveId),
    Landing(ConnectionId),
    Junction(InstanceId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ObservationId {
    PrimaryInput(PortId),
    PrimitiveOutput(PrimitiveId),
    InstanceOutput(InstanceId),
    JunctionOutput(InstanceId),
    DeclaredOutput(PortId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationSite {
    pub id: ObservationId,
    pub at: Anchor,
    pub logical_owner: Option<InstanceId>,
    pub display_label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{InstanceId, ObservationId, ObservationSite, PortId, PrimitiveId, TopologyNodeId};
    use crate::compile::geometry::Anchor;

    #[test]
    fn observation_identity_round_trips_without_using_position_or_label_as_a_key() {
        let primitive = ObservationId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(4),
            node: TopologyNodeId(2),
        });
        let site = ObservationSite {
            id: primitive,
            at: Anchor { x: 8, y: 1, z: 9 },
            logical_owner: Some(InstanceId(4)),
            display_label: Some("same label".to_string()),
        };
        let moved = ObservationSite {
            at: Anchor { x: -3, y: 7, z: 0 },
            display_label: Some("renamed".to_string()),
            ..site.clone()
        };

        assert_eq!(site.id, moved.id);
        assert_ne!(site, moved);
        assert_ne!(primitive, ObservationId::PrimaryInput(PortId(4)));
        let encoded = serde_json::to_vec(&site).unwrap();
        assert_eq!(
            serde_json::from_slice::<ObservationSite>(&encoded).unwrap(),
            site
        );
    }
}

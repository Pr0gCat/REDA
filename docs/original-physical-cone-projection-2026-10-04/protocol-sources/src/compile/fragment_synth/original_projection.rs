//! Isolated original whole physical-manifest projection. No rerouting.
use std::collections::BTreeMap;
use super::candidate::*;
use super::identity::*;
use super::instance_graph::{InstanceGraph,InstanceRole};
use super::rewrite::{CertifiedPhysicalRewrite,realise_physical_rewrite_with_size};
use crate::compile::{Netlist,CompiledCircuit,geometry::Anchor,planner::PortPlacements};
use crate::redstone::{world::{storage::World,block::BlockKind}};
struct Map {
        instances: BTreeMap<InstanceId,InstanceId>,
        inputs: BTreeMap<PortId,PortId>,
        outputs: BTreeMap<PortId,PortId>,
        routes: BTreeMap<RouteId, RouteId>,
        offset: Anchor,
    }
    impl Map {
        fn instance(&self, p: InstanceId) -> InstanceId { self.instances[&p] }
        fn primitive(&self, p: PrimitiveId) -> PrimitiveId { PrimitiveId { instance:self.instance(p.instance), ..p } }
        fn connection(&self, p: ConnectionId) -> ConnectionId {
            match p {
                ConnectionId::External { instance,input_index } => ConnectionId::External { instance:self.instance(instance),input_index },
                ConnectionId::Internal { instance,edge_index } => ConnectionId::Internal { instance:self.instance(instance),edge_index },
            }
        }
        fn endpoint(&self, p: PhysicalEndpointId) -> PhysicalEndpointId {
            match p {
                PhysicalEndpointId::PrimaryInput(p) => PhysicalEndpointId::PrimaryInput(self.inputs[&p]),
                PhysicalEndpointId::DeclaredOutput(p) => PhysicalEndpointId::DeclaredOutput(self.outputs[&p]),
                PhysicalEndpointId::PrimitiveOutput(p) => PhysicalEndpointId::PrimitiveOutput(self.primitive(p)),
                PhysicalEndpointId::Landing(p) => PhysicalEndpointId::Landing(self.connection(p)),
                PhysicalEndpointId::Junction(p) => PhysicalEndpointId::Junction(self.instance(p)),
            }
        }
        fn observation(&self, p: ObservationId) -> ObservationId {
            match p {
                ObservationId::PrimaryInput(p) => ObservationId::PrimaryInput(self.inputs[&p]),
                ObservationId::DeclaredOutput(p) => ObservationId::DeclaredOutput(self.outputs[&p]),
                ObservationId::PrimitiveOutput(p) => ObservationId::PrimitiveOutput(self.primitive(p)),
                ObservationId::InstanceOutput(p) => ObservationId::InstanceOutput(self.instance(p)),
                ObservationId::JunctionOutput(p) => ObservationId::JunctionOutput(self.instance(p)),
            }
        }
        fn sink(&self, p: RoutedSinkId) -> RoutedSinkId { RoutedSinkId { route:self.routes[&p.route], ..p } }
        fn owner(&self, p: DelayedOwner) -> DelayedOwner {
            match p {
                DelayedOwner::Primitive(p) => DelayedOwner::Primitive(self.primitive(p)),
                DelayedOwner::Route(p) => DelayedOwner::Route(self.routes[&p]),
                DelayedOwner::InputBinding(p) => DelayedOwner::InputBinding(self.inputs[&p]),
            }
        }
        fn at(&self, p: Anchor) -> Anchor { Anchor { x:p.x + self.offset.x,y:p.y+self.offset.y,z:p.z+self.offset.z } }
        fn delayed(&self, p: Option<DelayedComponent>) -> Option<DelayedComponent> { p.map(|p|DelayedComponent { at:self.at(p.at),owner:self.owner(p.owner) }) }
        fn blocks(&self, p: &mut [PlacedBlock]) { for p in p { p.at=self.at(p.at); } }
    }

fn translate_manifest(c:&ExpandedPhysicalCandidate,m:&Map,graph:InstanceGraph)->ExpandedPhysicalCandidate {
    let mut root=ExpandedPhysicalCandidate::empty(graph,PortPlacements::default());
        for p in c.placements.values() {
            let mut p=p.clone();p.id=m.primitive(p.id);p.anchor=m.at(p.anchor);p.delayed=m.delayed(p.delayed);m.blocks(&mut p.blocks);assert!(root.placements.insert(p.id,p).is_none());
        }
        for p in c.boundaries.values() {
            let mut p=p.clone();p.endpoint=m.endpoint(p.endpoint);p.delayed=m.delayed(p.delayed);m.blocks(&mut p.blocks);assert!(root.boundaries.insert(p.endpoint,p).is_none());
        }
        for (&alias,&canonical) in &c.output_boundary_aliases { assert!(root.output_boundary_aliases.insert(m.outputs[&alias],m.outputs[&canonical]).is_none()); }
        for p in c.connections.values() {
            let mut p=p.clone();p.id=m.connection(p.id);p.source=m.endpoint(p.source);p.landing=m.endpoint(p.landing);p.route=m.routes[&p.route];p.sink=m.sink(p.sink);assert!(root.connections.insert(p.id,p).is_none());
        }
        for p in c.routes.values() {
            let mut p=p.clone();p.id=m.routes[&p.id];p.source=m.endpoint(p.source);m.blocks(&mut p.cells);m.blocks(&mut p.floors);
            for b in &mut p.branches {
                b.sink=m.sink(b.sink);b.target=match b.target { RouteTarget::Connection(p)=>RouteTarget::Connection(m.connection(p)),RouteTarget::DeclaredOutput(p)=>RouteTarget::DeclaredOutput(m.outputs[&p]) };
                b.root=m.at(b.root);for p in &mut b.path {*p=m.at(*p);}
                b.terminal.sink=m.sink(b.terminal.sink);b.terminal.at=m.at(b.terminal.at);b.terminal.delayed_owner=b.terminal.delayed_owner.map(|p|m.owner(p));
            }
            assert!(root.routes.insert(p.id,p).is_none());
        }
        for (&sink,handovers) in &c.repeater_handovers {
            let p=handovers.iter().map(|p|RepeaterHandover { primitive:m.primitive(p.primitive),support:m.at(p.support),kind:p.kind }).collect();assert!(root.repeater_handovers.insert(m.sink(sink),p).is_none());
        }
        for p in c.junctions.values() {
            let mut p=p.clone();p.id=m.instance(p.id);p.at=m.at(p.at);p.contributors=p.contributors.iter().map(|&p|m.endpoint(p)).collect();m.blocks(&mut p.cells);assert!(root.junctions.insert(p.id,p).is_none());
        }
        for p in c.observations.values() {
            let mut p=p.clone();p.site.id=m.observation(p.site.id);p.site.at=m.at(p.site.at);p.site.logical_owner=p.site.logical_owner.map(|p|m.instance(p));assert!(root.observations.insert(p.site.id,p).is_none());
        }

    root
}

pub struct Projection {
    pub leaves:Vec<CertifiedPhysicalRewrite>,
    pub original_root:CertifiedPhysicalRewrite,
    pub report:serde_json::Value,
}

fn connection_instance(p:ConnectionId)->InstanceId {
    match p { ConnectionId::External { instance,.. }|ConnectionId::Internal { instance,.. }=>instance }
}
fn owned_blocks(c:&ExpandedPhysicalCandidate)->Vec<&PlacedBlock> {
    c.placements.values().flat_map(|p|p.blocks.iter())
        .chain(c.boundaries.values().flat_map(|p|p.blocks.iter()))
        .chain(c.routes.values().flat_map(|p|p.cells.iter().chain(&p.floors)))
        .chain(c.junctions.values().flat_map(|p|p.cells.iter())).collect()
}
fn count_keys(c:&ExpandedPhysicalCandidate,counts:&mut BTreeMap<String,usize>) {
    macro_rules! keys {($section:ident)=>{for k in c.$section.keys(){*counts.entry(format!("{}:{k:?}",stringify!($section))).or_default()+=1;}};}
    keys!(placements);keys!(boundaries);keys!(output_boundary_aliases);keys!(connections);keys!(routes);keys!(repeater_handovers);keys!(junctions);keys!(observations);
}

/// Preserve the actual original routing and supply geometry for each cone.
/// Shared gate ownership, cross-cone branches, caller pins and missing metadata
/// are refused rather than synthesized into a replacement authority.
pub fn project_original(source:&Netlist,original:&CompiledCircuit)->Result<Projection,String> {
    let c=original.physical_candidate().ok_or("missing original physical manifest")?;
    if !c.pins.is_empty() || !c.pin_contracts.is_empty() || !c.pin_name_bindings.is_empty(){return Err("caller pins are not a free projection".into());}
    if original.physical_rewrite().is_some(){return Err("this projection requires an original one-to-one raw implementation".into());}
    if c.instances.instances.len()!=source.gates.len() || c.instances.instances.iter().any(|i|i.role!=InstanceRole::Canonical){return Err("whole original manifest must be one-to-one".into());}
    let groups=super::support_composition::source_leaf_groups(source);
    let mut expected=BTreeMap::new();count_keys(c,&mut expected);
    let mut covered=BTreeMap::new();let mut covered_gates=BTreeMap::new();
    let mut leaves=vec![];let mut reports=vec![];
    let mut union=World::new(original.world.size().0,original.world.size().1,original.world.size().2);
    let library=crate::compile::topology::Library::default_library();
    for (ordinal,(leaf,outputs)) in groups.into_iter().enumerate(){
        let mut instances=BTreeMap::new();
        for (local,g) in leaf.gates.iter().enumerate(){
            let root_gate=source.gates.iter().position(|s|s.output==g.output).ok_or("unknown source gate")?;
            *covered_gates.entry(root_gate).or_insert(0)+=1;
            let matches:Vec<_>=c.instances.instances.iter().filter(|i|i.logical_gate==GateIndex(root_gate as u32)&&i.role==InstanceRole::Canonical).collect();
            if matches.len()!=1{return Err("source gate does not have exactly one canonical original instance".into());}
            instances.insert(matches[0].id,InstanceId(local as u32));
        }
        let inputs:BTreeMap<_,_>=leaf.inputs.iter().enumerate().map(|(local,n)|(PortId(source.inputs.iter().position(|s|s==n).unwrap() as u32),PortId(local as u32))).collect();
        let outputs:BTreeMap<_,_>=outputs.iter().enumerate().map(|(local,&root)|(PortId(root as u32),PortId(local as u32))).collect();
        let endpoint_belongs=|p:PhysicalEndpointId|match p {
            PhysicalEndpointId::PrimaryInput(p)=>inputs.contains_key(&p),
            PhysicalEndpointId::DeclaredOutput(p)=>outputs.contains_key(&p),
            PhysicalEndpointId::PrimitiveOutput(p)=>instances.contains_key(&p.instance),
            PhysicalEndpointId::Landing(p)=>instances.contains_key(&connection_instance(p)),
            PhysicalEndpointId::Junction(p)=>instances.contains_key(&p),
        };
        let target_belongs=|p:RouteTarget|match p {
            RouteTarget::Connection(p)=>instances.contains_key(&connection_instance(p)),
            RouteTarget::DeclaredOutput(p)=>outputs.contains_key(&p),
        };
        let mut subset=c.clone();
        subset.placements.retain(|p,_|instances.contains_key(&p.instance));
        subset.boundaries.retain(|&p,_|endpoint_belongs(p));
        subset.output_boundary_aliases.retain(|alias,canonical|outputs.contains_key(alias)&&outputs.contains_key(canonical));
        subset.connections.retain(|&p,_|instances.contains_key(&connection_instance(p)));
        subset.routes.retain(|_,route|endpoint_belongs(route.source));
        for r in subset.routes.values(){if r.branches.iter().any(|b|!target_belongs(b.target)){return Err("original route crosses cone boundary".into());}}
        let routes:BTreeMap<_,_>=subset.routes.keys().enumerate().map(|(i,&id)|(id,RouteId(i as u32))).collect();
        subset.repeater_handovers.retain(|p,_|routes.contains_key(&p.route));
        subset.junctions.retain(|p,_|instances.contains_key(p));
        subset.observations.retain(|&p,_|match p {
            ObservationId::PrimaryInput(p)=>inputs.contains_key(&p),
            ObservationId::DeclaredOutput(p)=>outputs.contains_key(&p),
            ObservationId::PrimitiveOutput(p)=>instances.contains_key(&p.instance),
            ObservationId::InstanceOutput(p)|ObservationId::JunctionOutput(p)=>instances.contains_key(&p),
        });
        for r in subset.connections.values(){if !endpoint_belongs(r.source)||!routes.contains_key(&r.route){return Err("connection escapes original cone".into());}}
        for alias in outputs.keys(){if let Some(canonical)=c.output_boundary_aliases.get(alias){if !outputs.contains_key(canonical){return Err("shared output physical owner escapes original cone".into());}}}
        count_keys(&subset,&mut covered);
        let blocks=owned_blocks(&subset);if blocks.is_empty(){return Err("empty original physical cone".into());}
        let min=Anchor{x:blocks.iter().map(|p|p.at.x).min().unwrap(),y:blocks.iter().map(|p|p.at.y).min().unwrap(),z:blocks.iter().map(|p|p.at.z).min().unwrap()};
        let max=Anchor{x:blocks.iter().map(|p|p.at.x).max().unwrap(),y:blocks.iter().map(|p|p.at.y).max().unwrap(),z:blocks.iter().map(|p|p.at.z).max().unwrap()};
        let m=Map{instances,inputs,outputs,routes,offset:Anchor{x:-min.x,y:-min.y,z:-min.z}};
        let implementations=c.instances.instances.iter().filter(|i|m.instances.contains_key(&i.id)).map(|i|(m.instance(i.id),i.implementation)).collect();
        let graph=InstanceGraph::one_to_one_with_implementations(&leaf,&library,&implementations).map_err(|e|format!("{e:?}"))?;
        let candidate=translate_manifest(&subset,&m,graph);
        let size=(max.x-min.x+1,max.y-min.y+1,max.z-min.z+1);
        let rewrite=realise_physical_rewrite_with_size(&leaf,&leaf,&candidate,&library,&leaf.inputs,&leaf.outputs,1_000_000,Some(size)).map_err(|e|format!("leaf{ordinal}: {e:?}"))?;
        let world=rewrite.physical().world();
        for y in 0..size.1 {for z in 0..size.2 {for x in 0..size.0 {
            let state=world.get(x,y,z);if state.kind==BlockKind::Air{continue;}
            let at=(x+min.x,y+min.y,z+min.z);
            if original.world.get(at.0,at.1,at.2)!=state{return Err("projection changed an original occupied cell".into());}
            let old=union.get(at.0,at.1,at.2);if old.kind!=BlockKind::Air && old!=state{return Err("projection union has conflicting ownership geometry".into());}
            union.set(at.0,at.1,at.2,state.clone());
        }}}
        reports.push(serde_json::json!({"ordinal":ordinal,"source_inputs":leaf.inputs,"source_outputs":leaf.outputs,"source_gate_outputs":leaf.gates.iter().map(|g|&g.output).collect::<Vec<_>>(),"origin":min,"size":size,"local_manifest_fingerprint":rewrite.implemented_candidate().fingerprint(),"local_world_fingerprint":super::benchmark::canonical_world_fingerprint(world),"mapping_instances":m.instances.iter().map(|(a,b)|(a,b)).collect::<Vec<_>>(),"mapping_inputs":m.inputs.iter().map(|(a,b)|(a,b)).collect::<Vec<_>>(),"mapping_outputs":m.outputs.iter().map(|(a,b)|(a,b)).collect::<Vec<_>>(),"mapping_routes":m.routes.iter().map(|(a,b)|(a,b)).collect::<Vec<_>>() }));
        leaves.push(rewrite);
    }
    if covered!=expected || covered_gates.len()!=source.gates.len() || covered_gates.values().any(|&n|n!=1){return Err("original manifest or source gate ownership is not partitioned exactly once".into());}
    let original_hash=super::benchmark::canonical_world_fingerprint(&original.world);
    let union_hash=super::benchmark::canonical_world_fingerprint(&union);
    if original_hash!=union_hash{return Err("projected union differs from original whole world".into());}
    let root=realise_physical_rewrite_with_size(source,source,c,&library,&source.inputs,&source.outputs,1_000_000,Some(original.world.size())).map_err(|e|format!("root: {e:?}"))?;
    let root_hash=super::benchmark::canonical_world_fingerprint(root.physical().world());
    if root_hash!=original_hash{return Err("root sealing changed the original world".into());}
    let sealed=CompiledCircuit::from_certified_rewrite(root.clone());
    let interfaces_exact=sealed.input_positions==original.input_positions
        && sealed.output_occurrences()==original.output_occurrences()
        && sealed.output_positions==original.output_positions
        && sealed.gate_output_positions==original.gate_output_positions
        && sealed.gate_facings==original.gate_facings;
    if !interfaces_exact{return Err("root sealing changed original interface or implementation observations".into());}
    let report=serde_json::json!({"interfaces_and_implemented_observations_exact":interfaces_exact,"quality_optimization":false,"purpose":"original physical-cone identity and exact ownership coverage only","original_world_fingerprint":original_hash,"union_world_fingerprint":union_hash,"sealed_root_world_fingerprint":root_hash,"original_manifest_fingerprint":c.fingerprint(),"sealed_root_manifest_fingerprint":root.implemented_candidate().fingerprint(),"covered_records":covered,"source_gates":covered_gates.len(),"declared_outputs":source.outputs.len(),"leaves":reports,"accepted":true});
    Ok(Projection{leaves,original_root:root,report})
}

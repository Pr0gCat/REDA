//! `compile_hierarchical`: the front door that compiles a whole module
//! hierarchy.
//!
//! The procedure is one rule applied at every level, bottom up:
//!
//! * a module with no instances is compiled by the ordinary unpinned seed
//!   ([`compile_block`]) -- these are the leaves, and they are the only part
//!   that can run in parallel, because nothing they need depends on anything
//!   else being compiled first;
//! * a module that *does* instantiate something is planned around its
//!   children's already-compiled blocks, unioned into one flat candidate and
//!   certified ([`compile_module_with_blocks`]) -- and, unless it is the top,
//!   that certified candidate becomes a [`CompiledBlock`] for its own parent.
//!
//! The second bullet is what makes a three-level design work at all.
//! [`union_candidate`] handles exactly ONE level of blocks: its documented
//! preconditions say a block must not itself contain blocks, because
//! `block_locals` counts every flat gate whose path *starts* with a block
//! instance's name and a nested block's grandchildren would land in that
//! count while the compiled block's own graph does not have them -- a
//! silent mis-mapping, not a refusal. Compiling bottom up is precisely the
//! discipline that keeps that from happening: by the time a module is handed
//! to `union_candidate` as a block it has already been flattened into a
//! block of its own, so it contains no blocks.
//!
//! The other half of that discipline is what such a block carries as its
//! [`CompiledBlock::lowered`]: the module's whole FLATTENING, which is what
//! its candidate was certified against, and never
//! `LoweredHierarchy::block_netlist` (the module's own gates only). The two
//! differ exactly when the module instantiates something, and the
//! difference is not cosmetic. A parent module typically passes most of its
//! own inputs straight down to its children -- `alu4`'s `a0..a3`, `b0..b3`
//! and `cin` are consumed only by its four `slice` instances -- so with the
//! narrower netlist the union one level up asks `block_reads_input` about
//! gates that are not in it, concludes nine of eleven ports feed nothing,
//! and retires the grandparent's delivery to routes that are really there.
//! Certification then refuses the result. Keeping `lowered` in step with
//! the candidate is what makes three levels compile.
//!
//! A single-module design does not go through any of this. It returns
//! straight down [`compile_fragment_synth`] on the flattened lowered
//! netlist, which for a module with no instances is exactly
//! `lower_optimised` of that module -- same candidate, same case
//! fingerprint, same metrics as the flat front door, byte for byte.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;

use serde::Serialize;

use crate::compile::fragment_synth::api::{
    compiled_from_certified, synthesis_case_fingerprint, SynthesisCaseFingerprint, SynthesisError,
    SynthesisInput, SynthesisResult,
};
use crate::compile::fragment_synth::blocks::{compile_block, BlockPort, CompiledBlock};
use crate::compile::fragment_synth::candidate::RealisedRouteTree;
use crate::compile::fragment_synth::certification::{
    CertifiedCandidate, CompleteCandidateCertifier,
};
use crate::compile::fragment_synth::compile_fragment_synth;
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::fragment::terminal_for_seed_error;
use crate::compile::fragment_synth::identity::{InstanceId, RouteId};
use crate::compile::fragment_synth::instance_graph::{
    BlockSpec, InstanceDriver, InstanceGraph, LogicalSignalId, PhysicalDriver, PhysicalSink,
};
use crate::compile::fragment_synth::placement::{analyse_instance_dag, EdgeFacts};
use crate::compile::fragment_synth::relocate::Offset;
use crate::compile::fragment_synth::route_opt::{
    prune_descriptors, prune_route, ParentRouteChoice,
};
use crate::compile::fragment_synth::search::{
    run_budgeted_proposals, CapWorkCounters, ProposalEvaluation, ProposalStream, ProposalTerminal,
    SearchCandidate, SynthesisBudget, SystemMonotonicClock,
};
use crate::compile::fragment_synth::seed::{
    certify_planned, plan_parent_with_services, BlockPlacementOffset, ParentBlocks, SeedError,
    SeedInput, SeedServices, SeedVariant,
};
use crate::compile::fragment_synth::services::{
    DurableSeedEmitter, DurableSeedVerifier, TopologyAwareSeedPlacer,
};
use crate::compile::fragment_synth::union::{
    first_internal_repeater, input_route, planning_netlist, union_candidate, BlockSpecOwned,
    InputSeamChoice, UnionInput,
};
use crate::compile::hierarchy::{
    instance_prefix, lower_hierarchy, GatePath, HierarchicalNetlist, LoweredHierarchy, PortBinding,
};
use crate::compile::metrics::canonical_fingerprint;
use crate::compile::planner::PortPlacements;
use crate::compile::routing::GuardedPhysicalRouter;
use crate::compile::topology::{GateKind, Library};
use crate::compile::Netlist;

/// Compile a hierarchical design, reusing each module's compile across every
/// instance of it.
///
/// `pins` applies to the top module's own ports, exactly as it does for
/// [`compile_fragment_synth`]; blocks are always compiled unpinned, because
/// a block's frame is chosen by the parent that stamps it.
pub fn compile_hierarchical(
    design: &HierarchicalNetlist,
    budget: SynthesisBudget,
    pins: Option<&PortPlacements>,
) -> Result<SynthesisResult, SynthesisError> {
    let threads = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    compile_hierarchical_with_threads(design, budget, pins, threads)
}

/// [`compile_hierarchical`] with the leaf-block worker count pinned, so a
/// test can prove the result does not depend on it.
pub(crate) fn compile_hierarchical_with_threads(
    design: &HierarchicalNetlist,
    budget: SynthesisBudget,
    pins: Option<&PortPlacements>,
    threads: usize,
) -> Result<SynthesisResult, SynthesisError> {
    let design = design
        .specialise_constants()
        .map_err(|error| SynthesisError::Hierarchy(error.to_string()))?;
    let lowered =
        lower_hierarchy(&design).map_err(|error| SynthesisError::Hierarchy(error.to_string()))?;

    if lowered.modules[&lowered.top].instances.is_empty() {
        // Exactly today's path: same case fingerprint, same candidate.
        return compile_fragment_synth(
            SynthesisInput {
                lowered: &lowered.flat,
                source_provenance: None,
                pins,
            },
            budget,
        );
    }

    let order = lowered
        .as_hierarchical()
        .module_order()
        .map_err(|error| SynthesisError::Hierarchy(error.to_string()))?;
    let library = Library::default_library();
    let search_config = SearchConfig::checked_defaults();
    let certification_config = CertificationConfig::from_search(&search_config);
    let services = seed_services(&library, &search_config);

    let blocks = compile_blocks(&lowered, &order, threads)?;
    let ordered = ordered_blocks(&lowered, &lowered.top, &order, &blocks);

    let certified = compile_module_with_blocks(
        &lowered,
        &lowered.top,
        &ordered,
        pins,
        services,
        &SeedVariant::default(),
        &BTreeMap::new(),
        &[],
        &[],
    )
    .map_err(|error| SynthesisError::Seed(format!("{}: {error}", lowered.top)))?;
    let (_, graph) = parent_planning_graph(
        &lowered,
        &lowered.top,
        &ordered,
        &library,
        &SeedVariant::default(),
    )
    .map_err(|error| SynthesisError::Seed(format!("{}: {error}", lowered.top)))?;
    let block_delays = graph
        .blocks
        .iter()
        .map(|block| {
            let compiled = ordered
                .get(block.block as usize)
                .ok_or(SeedError::UnknownBlock {
                    block: block.id,
                    index: block.block,
                })?;
            Ok((block.id, compiled.delay.0))
        })
        .collect::<Result<BTreeMap<_, _>, SeedError>>()
        .map_err(|error| SynthesisError::Seed(format!("{}: {error}", lowered.top)))?;
    let analysis = analyse_instance_dag(&graph, &block_delays)
        .map_err(|error| SynthesisError::Seed(format!("{}: {error}", lowered.top)))?;
    let edges = explicit_block_edges(&graph, &analysis.edges);
    let (source_outputs, sink_inputs) = compiled_port_lookup(&graph, &ordered)
        .map_err(|error| SynthesisError::Seed(format!("{}: {error}", lowered.top)))?;
    let seams = seam_descriptors(&graph, |block, input| {
        ordered
            .get(block as usize)
            .and_then(|compiled| input_route(&compiled.candidate, input))
    });

    let flat_input = SynthesisInput {
        lowered: &lowered.flat,
        source_provenance: None,
        pins,
    };
    let flat_case =
        synthesis_case_fingerprint(&flat_input, &search_config, &certification_config, &library);
    let case_fingerprint = SynthesisCaseFingerprint::from_fingerprint(canonical_fingerprint(
        &[
            flat_case.as_str().as_bytes(),
            &hierarchy_descriptor_bytes(&design),
        ]
        .concat(),
    ));

    let clock = SystemMonotonicClock::start();
    let compile = |block_placements: &BTreeMap<InstanceId, BlockPlacementOffset>,
                   seams: &[InputSeamChoice],
                   prunes: &[ParentRouteChoice]| {
        compile_module_with_blocks(
            &lowered,
            &lowered.top,
            &ordered,
            pins,
            services,
            &SeedVariant::default(),
            block_placements,
            seams,
            prunes,
        )
    };
    let mut proposals = HierarchicalProposalStream::new(
        edges,
        source_outputs,
        sink_inputs,
        seams,
        Box::new(compile),
    );
    let summary = run_budgeted_proposals(certified, budget, &clock, &mut proposals);

    let compiled = compiled_from_certified(&summary.best.certified, &lowered.flat)?;
    let metrics = summary.best.certified.metrics().clone();
    let candidate_fingerprint = metrics.candidate_fingerprint.clone();
    Ok(SynthesisResult {
        compiled,
        metrics,
        trace: summary.trace,
        evaluations_used: summary.evaluations_used,
        case_fingerprint,
        candidate_fingerprint,
        stop_reason: summary.stop_reason,
    })
}

/// The durable services every level of the hierarchy is compiled with.
///
/// Built from a `&Library` and a `&SearchConfig` rather than held in one
/// shared value because `SeedServices` carries `&dyn` trait objects and is
/// therefore not `Send`: a worker thread has to make its own.
fn seed_services<'a>(library: &'a Library, search_config: &'a SearchConfig) -> SeedServices<'a> {
    SeedServices {
        library,
        placer: &TopologyAwareSeedPlacer,
        router: &GuardedPhysicalRouter,
        emitter: &DurableSeedEmitter,
        verifier: &DurableSeedVerifier,
        certifier: &CompleteCandidateCertifier,
        search_config,
    }
}

/// Plan `module` around its blocks with `variant` applied to the module's
/// own gates, dissolve the blocks into one flat candidate and certify it.
fn compile_module_with_blocks(
    lowered: &LoweredHierarchy,
    module: &str,
    ordered: &[CompiledBlock],
    pins: Option<&PortPlacements>,
    services: SeedServices<'_>,
    variant: &SeedVariant,
    block_placements: &BTreeMap<InstanceId, BlockPlacementOffset>,
    seams: &[InputSeamChoice],
    prunes: &[ParentRouteChoice],
) -> Result<HierarchicalCandidate, SeedError> {
    let parent_gates = u32::try_from(lowered.modules[module].gates.len())
        .map_err(|_| SeedError::IdentityOverflow)?;

    // The proposal stream chooses its instances against the FLAT netlist, in
    // which this module's own gates come first, in order, before any
    // instance's subtree (`HierarchicalNetlist::flatten`). So a flat id below
    // `parent_gates` is one of the parent's own gates and names the same
    // planning instance; anything at or above it names a gate inside a
    // block, which this parent does not place and cannot re-implement.
    if variant
        .implementations
        .keys()
        .chain(variant.placements.keys())
        .any(|id| id.0 >= parent_gates)
    {
        return Err(SeedError::Incomplete("proposal targets a block gate"));
    }
    // A duplicate would give the parent two instances for one logical gate,
    // and `union::UnionInput` documents that the parent's graph must be
    // one-to-one -- the union pairs the k-th empty-path flat gate with
    // planning instance k, and a duplicate breaks that count. Refuse it here
    // rather than plan and route a parent the union is guaranteed to reject.
    if !variant.duplicates.is_empty() {
        return Err(SeedError::Incomplete(
            "a duplicate proposal cannot be represented in a parent that stamps blocks",
        ));
    }

    let (planning, graph) =
        parent_planning_graph(lowered, module, ordered, services.library, variant)?;

    let planned = plan_parent_with_services(
        SeedInput {
            lowered: &planning,
            source_provenance: None,
            pins,
        },
        services,
        graph,
        ParentBlocks { compiled: ordered },
        &variant.placements,
        block_placements,
    )?;
    let realised_block_offsets = planned.block_offsets.clone();
    let (flat, paths) = module_flattening(lowered, module)?;
    // Decided on the pre-union trees, which are the ones the union prunes.
    let prunable = prunable_parent_routes(&planned.candidate.routes);
    // Certification keeps the union's route ids, so `parent_routes` names
    // the trees in the certified candidate's timing graph.
    let (union, mut parent_routes) = union_candidate(UnionInput {
        parent: &planned,
        blocks: ordered,
        flat: &flat,
        paths: &paths,
        library: services.library,
        seams,
        prunes,
    })
    .map_err(|error| SeedError::Union(error.to_string()))?;
    parent_routes.retain(|route, _| prunable.contains(route));
    let certified = certify_planned(union, &flat, services)?;
    Ok(HierarchicalCandidate {
        certified,
        block_placements: block_placements.clone(),
        realised_block_offsets,
        seams: seams.to_vec(),
        parent_routes,
        prunes: prunes.to_vec(),
    })
}

/// The parent routes `prune_route` would actually change, proven on a
/// throwaway clone of each tree. Offering any other route costs a full
/// proposal compile that the union then refuses.
fn prunable_parent_routes(routes: &BTreeMap<RouteId, RealisedRouteTree>) -> BTreeSet<RouteId> {
    routes
        .iter()
        .filter_map(|(&route, tree)| prune_route(&mut tree.clone()).then_some(route))
        .collect()
}

fn parent_planning_graph(
    lowered: &LoweredHierarchy,
    module: &str,
    ordered: &[CompiledBlock],
    library: &Library,
    variant: &SeedVariant,
) -> Result<(Netlist, InstanceGraph), SeedError> {
    let (planning, owned) = planning_netlist(lowered, module, ordered);
    let specs: Vec<BlockSpec<'_>> = owned.iter().map(BlockSpecOwned::as_spec).collect();
    let graph = InstanceGraph::with_blocks_and_implementations(
        &planning,
        library,
        &specs,
        &variant.implementations,
    )?;
    Ok((planning, graph))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct BlockEdge {
    source_block: InstanceId,
    source_port: u16,
    sink_block: InstanceId,
    sink_input: u16,
    slack: u64,
}

#[derive(Serialize)]
struct BlockEdgeFingerprint {
    schema: &'static str,
    edge: BlockEdge,
}

#[derive(Serialize)]
struct HierarchicalChoiceFingerprint<'a> {
    schema: &'static str,
    edge: BlockEdge,
    incumbent_fingerprint: &'a str,
    block_placements: Vec<(InstanceId, i32, i32)>,
}

/// The seam stage has no block edge: its descriptor is the choice itself.
#[derive(Serialize)]
struct SeamFingerprint {
    schema: &'static str,
    seam: InputSeamChoice,
}

#[derive(Serialize)]
struct SeamChoiceFingerprint<'a> {
    schema: &'static str,
    seam: InputSeamChoice,
    incumbent_fingerprint: &'a str,
}

fn serialized_fingerprint(descriptor: &impl Serialize) -> crate::compile::metrics::Fingerprint {
    canonical_fingerprint(
        &serde_json::to_vec(descriptor).expect("hierarchical proposal descriptor serializes"),
    )
}

fn block_edge_fingerprint(
    schema: &'static str,
    edge: BlockEdge,
) -> crate::compile::metrics::Fingerprint {
    serialized_fingerprint(&BlockEdgeFingerprint { schema, edge })
}

fn hierarchical_choice_fingerprint(
    schema: &'static str,
    edge: BlockEdge,
    incumbent: &HierarchicalCandidate,
    block_placements: &BTreeMap<InstanceId, BlockPlacementOffset>,
) -> crate::compile::metrics::Fingerprint {
    serialized_fingerprint(&HierarchicalChoiceFingerprint {
        schema,
        edge,
        incumbent_fingerprint: incumbent.candidate_fingerprint().as_str(),
        block_placements: block_placements
            .iter()
            .map(|(&block, offset)| (block, offset.dx, offset.dz))
            .collect(),
    })
}

fn seam_choice_fingerprint(
    schema: &'static str,
    seam: InputSeamChoice,
    incumbent: &HierarchicalCandidate,
) -> crate::compile::metrics::Fingerprint {
    serialized_fingerprint(&SeamChoiceFingerprint {
        schema,
        seam,
        incumbent_fingerprint: incumbent.candidate_fingerprint().as_str(),
    })
}

/// The prune stage's descriptor is one parent route.
#[derive(Serialize)]
struct PruneFingerprint {
    schema: &'static str,
    prune: ParentRouteChoice,
}

#[derive(Serialize)]
struct PruneChoiceFingerprint<'a> {
    schema: &'static str,
    prune: ParentRouteChoice,
    incumbent_fingerprint: &'a str,
}

/// Every Input Seam Absorption descriptor the stream will offer, in stream
/// order: one per stamped block input whose compiled route carries a
/// non-terminal route-owned repeater, whatever drives that input (a sibling
/// block, a parent primary input, or parent glue). `graph.blocks` lists the
/// stamped instances and their input counts; `routes` answers "the block at
/// this index, its route out of this input". Ordered by sink instance then
/// input, so the order is fixed by the hierarchy alone.
fn seam_descriptors<'a>(
    graph: &InstanceGraph,
    routes: impl Fn(u32, u16) -> Option<&'a RealisedRouteTree>,
) -> Vec<InputSeamChoice> {
    let mut seams = Vec::new();
    for block in &graph.blocks {
        for index in 0..block.inputs.len() {
            let Ok(input) = u16::try_from(index) else {
                break;
            };
            if let Some(at) = routes(block.block, input).and_then(first_internal_repeater) {
                seams.push(InputSeamChoice {
                    sink_block: block.id,
                    input,
                    at,
                });
            }
        }
    }
    seams.sort();
    seams
}

fn explicit_block_edges(graph: &InstanceGraph, facts: &[EdgeFacts]) -> Vec<BlockEdge> {
    let slack = facts
        .iter()
        .map(|edge| ((edge.source, edge.sink), edge.structural_slack_ticks))
        .collect::<BTreeMap<_, _>>();
    let mut edges = Vec::new();
    for assignment in &graph.assignments {
        let PhysicalSink::InstanceInput {
            instance: sink_block,
            input_index: sink_input,
        } = assignment.sink
        else {
            continue;
        };
        let Some(sink) = graph.block(sink_block) else {
            continue;
        };
        if usize::from(sink_input) >= sink.inputs.len() {
            continue;
        }
        let PhysicalDriver::Instance(InstanceDriver::Primitive {
            logical_owner,
            terminals,
        }) = &assignment.driver
        else {
            continue;
        };
        let [terminal] = terminals.as_slice() else {
            continue;
        };
        if *logical_owner != terminal.instance {
            continue;
        }
        let Some(source) = graph.block(*logical_owner) else {
            continue;
        };
        let source_port = usize::from(terminal.node.0);
        let Some(&output_gate) = source.output_gates.get(source_port) else {
            continue;
        };
        if assignment.signal != LogicalSignalId::GateOutput(output_gate) {
            continue;
        }
        let Some(&slack) = slack.get(&(*logical_owner, sink_block)) else {
            continue;
        };
        edges.push(BlockEdge {
            source_block: *logical_owner,
            source_port: terminal.node.0,
            sink_block,
            sink_input,
            slack,
        });
    }
    edges.sort_by_key(|edge| {
        (
            edge.slack,
            edge.source_block,
            edge.sink_block,
            edge.sink_input,
        )
    });
    edges
}

fn compiled_port_lookup(
    graph: &InstanceGraph,
    ordered: &[CompiledBlock],
) -> Result<
    (
        BTreeMap<(InstanceId, u16), BlockPort>,
        BTreeMap<(InstanceId, u16), BlockPort>,
    ),
    SeedError,
> {
    let mut source_outputs = BTreeMap::new();
    let mut sink_inputs = BTreeMap::new();
    for block in &graph.blocks {
        let compiled = ordered
            .get(block.block as usize)
            .ok_or(SeedError::UnknownBlock {
                block: block.id,
                index: block.block,
            })?;
        for (index, name) in compiled.lowered.outputs.iter().enumerate() {
            let port = compiled
                .outputs
                .get(name)
                .ok_or(SeedError::Incomplete("compiled block output port"))?;
            source_outputs.insert(
                (
                    block.id,
                    u16::try_from(index).map_err(|_| SeedError::IdentityOverflow)?,
                ),
                *port,
            );
        }
        for (index, name) in compiled.lowered.inputs.iter().enumerate() {
            let port = compiled
                .inputs
                .get(name)
                .ok_or(SeedError::Incomplete("compiled block input port"))?;
            sink_inputs.insert(
                (
                    block.id,
                    u16::try_from(index).map_err(|_| SeedError::IdentityOverflow)?,
                ),
                *port,
            );
        }
    }
    Ok((source_outputs, sink_inputs))
}

fn block_alignment_proposal(
    edge: &BlockEdge,
    source_outputs: &BTreeMap<(InstanceId, u16), BlockPort>,
    sink_inputs: &BTreeMap<(InstanceId, u16), BlockPort>,
    realised_offsets: &BTreeMap<InstanceId, Offset>,
    incumbent: &BTreeMap<InstanceId, BlockPlacementOffset>,
) -> BTreeMap<InstanceId, BlockPlacementOffset> {
    // Edge and ports share a validated graph; offsets come from its planned candidate.
    let source = source_outputs[&(edge.source_block, edge.source_port)];
    let sink = sink_inputs[&(edge.sink_block, edge.sink_input)];
    let source_z = source
        .cell
        .z
        .saturating_add(realised_offsets[&edge.source_block].dz);
    let sink_z = sink
        .cell
        .z
        .saturating_add(realised_offsets[&edge.sink_block].dz);
    let mut proposal = incumbent.clone();
    let placement = proposal
        .entry(edge.sink_block)
        .or_insert(BlockPlacementOffset { dx: 0, dz: 0 });
    placement.dz = placement.dz.saturating_add(source_z.saturating_sub(sink_z));
    proposal
}

/// Move the edge's sink block one X cell toward the realised source port,
/// preserving every other accepted offset. `None` when the ports already
/// share an X coordinate.
fn block_pull_x_proposal(
    edge: &BlockEdge,
    source_outputs: &BTreeMap<(InstanceId, u16), BlockPort>,
    sink_inputs: &BTreeMap<(InstanceId, u16), BlockPort>,
    realised_offsets: &BTreeMap<InstanceId, Offset>,
    incumbent: &BTreeMap<InstanceId, BlockPlacementOffset>,
) -> Option<BTreeMap<InstanceId, BlockPlacementOffset>> {
    let source = source_outputs[&(edge.source_block, edge.source_port)];
    let sink = sink_inputs[&(edge.sink_block, edge.sink_input)];
    let source_x = source
        .cell
        .x
        .saturating_add(realised_offsets[&edge.source_block].dx);
    let sink_x = sink
        .cell
        .x
        .saturating_add(realised_offsets[&edge.sink_block].dx);
    let step = source_x.saturating_sub(sink_x).signum();
    if step == 0 {
        return None;
    }
    let mut proposal = incumbent.clone();
    let placement = proposal
        .entry(edge.sink_block)
        .or_insert(BlockPlacementOffset { dx: 0, dz: 0 });
    placement.dx = placement.dx.saturating_add(step);
    Some(proposal)
}

struct HierarchicalCandidate {
    certified: CertifiedCandidate,
    block_placements: BTreeMap<InstanceId, BlockPlacementOffset>,
    realised_block_offsets: BTreeMap<InstanceId, Offset>,
    /// Accepted seam choices, cumulative like `block_placements`.
    seams: Vec<InputSeamChoice>,
    /// Each prunable pre-union parent route id to the id of the certified
    /// tree that carries it (a route out of a block lamp is absorbed by the
    /// block's output tree), for the prune stage's slack lookup. Routes
    /// `prune_route` cannot change are left out so they are never offered.
    parent_routes: BTreeMap<RouteId, RouteId>,
    /// Accepted prune choices, cumulative like `seams`.
    prunes: Vec<ParentRouteChoice>,
}

impl SearchCandidate for HierarchicalCandidate {
    fn candidate_fingerprint(&self) -> &crate::compile::metrics::Fingerprint {
        &self.certified.metrics().candidate_fingerprint
    }

    fn quality(&self) -> crate::compile::fragment_synth::certification::QualityKey {
        self.certified.metrics().quality
    }
}

type HierarchicalCompiler<'a> = dyn Fn(
        &BTreeMap<InstanceId, BlockPlacementOffset>,
        &[InputSeamChoice],
        &[ParentRouteChoice],
    ) -> Result<HierarchicalCandidate, SeedError>
    + 'a;

struct HierarchicalProposalStream<'a> {
    edges: Vec<BlockEdge>,
    /// Pull-X descriptors, frozen from the incumbent when the alignment
    /// stage is exhausted. Edges whose ports already share an X are omitted.
    pull_x_edges: Option<Vec<BlockEdge>>,
    /// Seam descriptors, offered once each after Pull-X is exhausted. They
    /// read only the compiled blocks, which never change, so they are fixed
    /// at construction rather than frozen from an incumbent.
    seams: Vec<InputSeamChoice>,
    /// Prune descriptors, frozen from the incumbent's timing graph when the
    /// seam stage is exhausted: parent routes by minimum slack then id.
    prunes: Option<Vec<ParentRouteChoice>>,
    source_outputs: BTreeMap<(InstanceId, u16), BlockPort>,
    sink_inputs: BTreeMap<(InstanceId, u16), BlockPort>,
    compile: Box<HierarchicalCompiler<'a>>,
}

impl<'a> HierarchicalProposalStream<'a> {
    fn new(
        edges: Vec<BlockEdge>,
        source_outputs: BTreeMap<(InstanceId, u16), BlockPort>,
        sink_inputs: BTreeMap<(InstanceId, u16), BlockPort>,
        seams: Vec<InputSeamChoice>,
        compile: Box<HierarchicalCompiler<'a>>,
    ) -> Self {
        Self {
            edges,
            pull_x_edges: None,
            seams,
            prunes: None,
            source_outputs,
            sink_inputs,
            compile,
        }
    }

    /// The seam descriptor at stream position `index`, which lies past every
    /// alignment edge and every frozen Pull-X edge. Only meaningful once
    /// Pull-X has been frozen; before that the Pull-X stage owns the index.
    fn seam(&self, index: usize) -> Option<InputSeamChoice> {
        let pull_x = self.pull_x_edges.as_ref().map_or(0, Vec::len);
        self.seams
            .get(index.checked_sub(self.edges.len() + pull_x)?)
            .copied()
    }

    fn pull_x_edge(
        &mut self,
        index: usize,
        incumbent: &HierarchicalCandidate,
    ) -> Option<BlockEdge> {
        let frozen = self.pull_x_edges.get_or_insert_with(|| {
            self.edges
                .iter()
                .filter(|edge| {
                    block_pull_x_proposal(
                        edge,
                        &self.source_outputs,
                        &self.sink_inputs,
                        &incumbent.realised_block_offsets,
                        &incumbent.block_placements,
                    )
                    .is_some()
                })
                .copied()
                .collect()
        });
        frozen.get(index).copied()
    }

    /// The prune descriptor at stream position `index`, past every
    /// alignment, Pull-X and seam descriptor. `freeze` reads the incumbent's
    /// timing graph, so it runs once, the first time the stage is reached.
    fn prune(
        &mut self,
        index: usize,
        freeze: impl FnOnce() -> Vec<ParentRouteChoice>,
    ) -> Option<ParentRouteChoice> {
        let pull_x = self.pull_x_edges.as_ref().map_or(0, Vec::len);
        let index = index.checked_sub(self.edges.len() + pull_x + self.seams.len())?;
        self.prunes.get_or_insert_with(freeze).get(index).copied()
    }
}

impl ProposalStream<HierarchicalCandidate> for HierarchicalProposalStream<'_> {
    fn next(
        &mut self,
        proposal_index: u64,
        incumbent: &HierarchicalCandidate,
    ) -> Option<ProposalEvaluation<HierarchicalCandidate>> {
        let index = usize::try_from(proposal_index).ok()?;
        // Each stage yields its two fingerprints and, unless the descriptor
        // is stale, the block placements and cumulative seams and prunes to
        // compile.
        let (fragment_fingerprint, choice_fingerprint, proposal) =
            if let Some(&edge) = self.edges.get(index) {
                let placements = block_alignment_proposal(
                    &edge,
                    &self.source_outputs,
                    &self.sink_inputs,
                    &incumbent.realised_block_offsets,
                    &incumbent.block_placements,
                );
                (
                    block_edge_fingerprint("hierarchical-block-fragment-v1", edge),
                    hierarchical_choice_fingerprint(
                        "hierarchical-block-choice-v1",
                        edge,
                        incumbent,
                        &placements,
                    ),
                    Some((placements, incumbent.seams.clone(), incumbent.prunes.clone())),
                )
            } else if let Some(edge) = self.pull_x_edge(index - self.edges.len(), incumbent) {
                // A descriptor whose ports now share an X is stale: refuse it
                // rather than retarget.
                let placements = block_pull_x_proposal(
                    &edge,
                    &self.source_outputs,
                    &self.sink_inputs,
                    &incumbent.realised_block_offsets,
                    &incumbent.block_placements,
                );
                (
                    block_edge_fingerprint("hierarchical-block-pull-x-fragment-v1", edge),
                    hierarchical_choice_fingerprint(
                        "hierarchical-block-pull-x-choice-v1",
                        edge,
                        incumbent,
                        placements.as_ref().unwrap_or(&incumbent.block_placements),
                    ),
                    placements.map(|placements| {
                        (placements, incumbent.seams.clone(), incumbent.prunes.clone())
                    }),
                )
            } else if let Some(seam) = self.seam(index) {
                // Block placements never change here; the union refuses a
                // seam whose anchor is no longer a route-owned repeater.
                let mut seams = incumbent.seams.clone();
                seams.push(seam);
                (
                    serialized_fingerprint(&SeamFingerprint {
                        schema: "hierarchical-input-seam-fragment-v1",
                        seam,
                    }),
                    seam_choice_fingerprint("hierarchical-input-seam-choice-v1", seam, incumbent),
                    Some((incumbent.block_placements.clone(), seams, incumbent.prunes.clone())),
                )
            } else {
                // The union refuses a prune whose route is gone or already
                // has nothing redundant left.
                let prune = self.prune(index, || {
                    prune_descriptors(incumbent.certified.timing_graph(), &incumbent.parent_routes)
                })?;
                let mut prunes = incumbent.prunes.clone();
                prunes.push(prune);
                (
                    serialized_fingerprint(&PruneFingerprint {
                        schema: "hierarchical-parent-prune-fragment-v1",
                        prune,
                    }),
                    serialized_fingerprint(&PruneChoiceFingerprint {
                        schema: "hierarchical-parent-prune-choice-v1",
                        prune,
                        incumbent_fingerprint: incumbent.candidate_fingerprint().as_str(),
                    }),
                    Some((incumbent.block_placements.clone(), incumbent.seams.clone(), prunes)),
                )
            };
        let mut cap_work = CapWorkCounters::default();
        let Some((block_placements, seams, prunes)) = proposal else {
            return Some(ProposalEvaluation {
                fragment_fingerprint,
                choice_fingerprint,
                terminal: ProposalTerminal::Refused,
                cap_work,
                certified: None,
            });
        };
        match (self.compile)(&block_placements, &seams, &prunes) {
            Ok(candidate) => Some(ProposalEvaluation {
                fragment_fingerprint,
                choice_fingerprint,
                terminal: ProposalTerminal::NoImprovement,
                cap_work,
                certified: Some(candidate),
            }),
            Err(error) => {
                let terminal = terminal_for_seed_error(&error, &mut cap_work);
                Some(ProposalEvaluation {
                    fragment_fingerprint,
                    choice_fingerprint,
                    terminal,
                    cap_work,
                    certified: None,
                })
            }
        }
    }
}

/// Every module the reachable design actually instantiates.
///
/// Reachability matters: `specialise_constants` leaves the unspecialised
/// original of every clone in `modules`, and nothing instantiates it any
/// more. Compiling those would be pure waste, and one of them may not even
/// be compilable on its own.
fn instantiated_modules(lowered: &LoweredHierarchy) -> BTreeSet<String> {
    let mut instantiated = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut pending = vec![lowered.top.clone()];
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        for instance in &lowered.modules[&name].instances {
            instantiated.insert(instance.module.clone());
            pending.push(instance.module.clone());
        }
    }
    instantiated
}

/// Compile every module the design instantiates, children before parents.
///
/// Leaves go out to `threads` scoped workers; a module that instantiates
/// something is compiled here, in `module_order`, once all of its own
/// children are present. Results are keyed by module name and the worker
/// pool only ever inserts into that map, so nothing in the outcome depends
/// on which thread finished first.
fn compile_blocks(
    lowered: &LoweredHierarchy,
    order: &[String],
    threads: usize,
) -> Result<BTreeMap<String, CompiledBlock>, SynthesisError> {
    let instantiated = instantiated_modules(lowered);
    let leaves: VecDeque<String> = order
        .iter()
        .filter(|name| instantiated.contains(*name) && lowered.modules[*name].instances.is_empty())
        .cloned()
        .collect();

    let queue = Mutex::new(leaves);
    let compiled = Mutex::new(BTreeMap::<String, CompiledBlock>::new());
    // Module name plus rendered error. The first failure by NAME wins, not
    // the first by wall clock, and the queue is deliberately not drained on
    // a failure: which module a racing worker would have got to next is the
    // one thing about a thread pool that is not reproducible, so every
    // queued leaf is attempted and the reported failure is a function of the
    // design alone.
    let failure = Mutex::new(None::<(String, String)>);

    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| {
                let library = Library::default_library();
                let search_config = SearchConfig::checked_defaults();
                let services = seed_services(&library, &search_config);
                loop {
                    let next = queue.lock().expect("block queue").pop_front();
                    let Some(name) = next else { break };
                    let netlist = lowered.block_netlist(&name);
                    match compile_block(&name, &netlist, services) {
                        Ok(block) => {
                            compiled
                                .lock()
                                .expect("compiled blocks")
                                .insert(name, block);
                        }
                        Err(error) => {
                            let mut slot = failure.lock().expect("block failure");
                            let first = match slot.as_ref() {
                                None => true,
                                Some((earlier, _)) => name < *earlier,
                            };
                            if first {
                                *slot = Some((name, error.to_string()));
                            }
                        }
                    }
                }
            });
        }
    });

    if let Some((module, reason)) = failure.into_inner().expect("block failure") {
        let first_path = first_instance_path(lowered, &module);
        return Err(SynthesisError::Block {
            module,
            first_path,
            reason,
        });
    }

    let mut compiled = compiled.into_inner().expect("compiled blocks");
    let library = Library::default_library();
    let search_config = SearchConfig::checked_defaults();
    let services = seed_services(&library, &search_config);
    for name in order {
        if !instantiated.contains(name) || lowered.modules[name].instances.is_empty() {
            continue;
        }
        let ordered = ordered_blocks(lowered, name, order, &compiled);
        let certified = compile_module_with_blocks(
            lowered,
            name,
            &ordered,
            None,
            services,
            &SeedVariant::default(),
            &BTreeMap::new(),
            &[],
            &[],
        )
        .map_err(|error| SynthesisError::Seed(format!("{name}: {error}")))?;
        // The netlist this block carries must be the one its candidate was
        // certified against -- this module's whole FLATTENING, grandchildren
        // included -- not `block_netlist`, which is the module's own gates
        // only. See `CompiledBlock::lowered`: a block whose netlist is
        // narrower than its candidate is internally inconsistent, and the
        // union one level up reads that netlist to decide which of the
        // block's ports anything behind them actually consumes.
        let (flat, _) = module_flattening(lowered, name)
            .map_err(|error| SynthesisError::Seed(format!("{name}: {error}")))?;
        let block =
            CompiledBlock::from_certified(name, &flat, &certified.certified).map_err(|error| {
                SynthesisError::Block {
                    module: name.clone(),
                    first_path: first_instance_path(lowered, name),
                    reason: error.to_string(),
                }
            })?;
        compiled.insert(name.clone(), block);
    }
    Ok(compiled)
}

/// The distinct modules `module` instantiates, in `module_order`, cloned out
/// of the already-compiled set.
///
/// The order is what indexes them: `planning_netlist` records each
/// instance's position in this slice as its `BlockSpec::block`, and both the
/// parent's placer and the union read the compiled block back by that index.
fn ordered_blocks(
    lowered: &LoweredHierarchy,
    module: &str,
    order: &[String],
    compiled: &BTreeMap<String, CompiledBlock>,
) -> Vec<CompiledBlock> {
    let mut wanted: BTreeSet<&str> = lowered.modules[module]
        .instances
        .iter()
        .map(|instance| instance.module.as_str())
        .collect();
    order
        .iter()
        .filter(|name| wanted.remove(name.as_str()))
        .map(|name| {
            compiled
                .get(name)
                .expect("a child module is compiled before its parent")
                .clone()
        })
        .collect()
}

/// The flattening of `module` as if it were the design's top: what
/// certification of that module must see.
fn module_flattening(
    lowered: &LoweredHierarchy,
    module: &str,
) -> Result<(Netlist, Vec<GatePath>), SeedError> {
    if module == lowered.top {
        return Ok((lowered.flat.clone(), lowered.paths.clone()));
    }
    let mut design = lowered.as_hierarchical();
    design.top = module.to_string();
    design
        .flatten()
        .map_err(|_| SeedError::Incomplete("a module of a validated hierarchy must flatten"))
}

/// Where `module` is first instantiated, for an error message that names a
/// place in the design rather than only a module.
fn first_instance_path(lowered: &LoweredHierarchy, module: &str) -> String {
    lowered
        .paths
        .iter()
        .find(|path| path.module == module && !path.path.is_empty())
        .map(|path| instance_prefix(&path.path))
        .unwrap_or_else(|| module.to_string())
}

// ---------------------------------------------------------------------
// Case fingerprint
// ---------------------------------------------------------------------

/// A hierarchical case is the flat case of its flattened netlist plus the
/// hierarchy that produced it: two designs that flatten to the same netlist
/// still compile differently, because which modules exist decides which
/// block is compiled once and stamped many times.
///
/// Written by hand rather than by serialising the design, because the
/// hierarchy types deliberately do not derive `Serialize` -- `Gate` derives
/// only `Debug, Clone, PartialEq, Eq`, and says so. This mirrors
/// `api::CaseDescriptor`: every field that can change what is compiled is
/// listed here explicitly, so adding one to the hierarchy without adding it
/// here is a visible omission rather than a silent hole.
#[derive(Serialize)]
struct HierarchyDescriptor<'a> {
    schema_version: u64,
    top: &'a str,
    modules: Vec<HierarchyModuleDescriptor<'a>>,
}

#[derive(Serialize)]
struct HierarchyModuleDescriptor<'a> {
    name: &'a str,
    inputs: &'a [String],
    outputs: &'a [String],
    gates: Vec<HierarchyGateDescriptor<'a>>,
    instances: Vec<HierarchyInstanceDescriptor<'a>>,
}

#[derive(Serialize)]
struct HierarchyGateDescriptor<'a> {
    name: &'a str,
    inputs: &'a [String],
    output: &'a str,
    kind: GateKind,
}

#[derive(Serialize)]
struct HierarchyInstanceDescriptor<'a> {
    name: &'a str,
    module: &'a str,
    ports: Vec<(&'a str, HierarchyBindingDescriptor<'a>)>,
}

#[derive(Serialize)]
enum HierarchyBindingDescriptor<'a> {
    Signal(&'a str),
    Zero,
    One,
}

/// The canonical bytes of `design`'s hierarchy.
///
/// `design` here is always the *specialised* design -- the one that is
/// actually compiled -- so every constant tie shows up as the specialised
/// module it folded into, and the `Zero`/`One` arms exist only so that a
/// design that somehow still carries one is not silently indistinguishable
/// from the same design with that port bound to a signal.
fn hierarchy_descriptor_bytes(design: &HierarchicalNetlist) -> Vec<u8> {
    let modules = design
        .modules
        .iter()
        .map(|(name, module)| HierarchyModuleDescriptor {
            name,
            inputs: &module.inputs,
            outputs: &module.outputs,
            gates: module
                .gates
                .iter()
                .map(|gate| HierarchyGateDescriptor {
                    name: &gate.name,
                    inputs: &gate.inputs,
                    output: &gate.output,
                    kind: gate.kind,
                })
                .collect(),
            instances: module
                .instances
                .iter()
                .map(|instance| HierarchyInstanceDescriptor {
                    name: &instance.name,
                    module: &instance.module,
                    ports: instance
                        .ports
                        .iter()
                        .map(|(port, binding)| {
                            (
                                port.as_str(),
                                match binding {
                                    PortBinding::Signal(signal) => {
                                        HierarchyBindingDescriptor::Signal(signal)
                                    }
                                    PortBinding::Zero => HierarchyBindingDescriptor::Zero,
                                    PortBinding::One => HierarchyBindingDescriptor::One,
                                },
                            )
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    serde_json::to_vec(&HierarchyDescriptor {
        schema_version: 1,
        top: &design.top,
        modules,
    })
    .expect("hierarchy descriptor must serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::identity::{
        GateIndex, ImplementationKey, InputMask, PortId, PrimitiveId, TopologyNodeId,
    };
    use crate::compile::fragment_synth::instance_graph::{
        BlockInstance, DuplicateRequest, SinkAssignment,
    };
    use crate::compile::fragment_synth::search::StopReason;
    use crate::compile::fragment_synth::seed::InstancePlacementOverride;
    use crate::compile::geometry::Anchor;
    use crate::compile::hierarchy::{Module, ModuleInstance};
    use crate::compile::Gate;
    use crate::redstone::world::block::Facing;

    struct BlockProposalFixture {
        graph: InstanceGraph,
        structural_edges: Vec<EdgeFacts>,
        source: InstanceId,
        sink: InstanceId,
        source_outputs: BTreeMap<(InstanceId, u16), BlockPort>,
        sink_inputs: BTreeMap<(InstanceId, u16), BlockPort>,
        realised_offsets: BTreeMap<InstanceId, Offset>,
        block_placements: BTreeMap<InstanceId, BlockPlacementOffset>,
    }

    /// One valid two-block connection plus every malformed assignment the
    /// parent proposal stream must ignore. The port cells are deliberately
    /// distinct from their realised offsets: alignment has to use both.
    fn block_proposal_fixture() -> BlockProposalFixture {
        let source = InstanceId(10);
        let later_source = InstanceId(11);
        let sink = InstanceId(20);
        let later_sink = InstanceId(21);
        let non_block = InstanceId(99);
        let primary = LogicalSignalId::PrimaryInput(PortId(0));
        let primitive = |logical_owner, terminals| {
            PhysicalDriver::Instance(InstanceDriver::Primitive {
                logical_owner,
                terminals,
            })
        };
        let terminal = |instance, node| PrimitiveId {
            instance,
            node: TopologyNodeId(node),
        };
        let assignment = |sink, signal, driver| SinkAssignment {
            sink,
            signal,
            driver,
        };
        let sink_input = |instance, input_index| PhysicalSink::InstanceInput {
            instance,
            input_index,
        };
        let block = |id, index, inputs, output_gates| BlockInstance {
            id,
            block: index,
            path: vec![format!("block{index}")],
            inputs,
            output_gates,
        };

        BlockProposalFixture {
            graph: InstanceGraph {
                instances: vec![],
                assignments: vec![
                    assignment(
                        sink_input(sink, 0),
                        LogicalSignalId::GateOutput(GateIndex(0)),
                        primitive(source, vec![terminal(source, 0)]),
                    ),
                    assignment(
                        sink_input(sink, 1),
                        LogicalSignalId::GateOutput(GateIndex(1)),
                        primitive(source, vec![terminal(source, 1)]),
                    ),
                    assignment(
                        sink_input(sink, 2),
                        LogicalSignalId::GateOutput(GateIndex(2)),
                        primitive(later_source, vec![terminal(later_source, 0)]),
                    ),
                    assignment(
                        sink_input(later_sink, 0),
                        LogicalSignalId::GateOutput(GateIndex(0)),
                        primitive(source, vec![terminal(source, 0)]),
                    ),
                    assignment(
                        sink_input(sink, 3),
                        primary,
                        PhysicalDriver::PrimaryInput(PortId(0)),
                    ),
                    assignment(
                        sink_input(sink, 4),
                        LogicalSignalId::GateOutput(GateIndex(0)),
                        PhysicalDriver::Instance(InstanceDriver::Junction {
                            logical_owner: source,
                            contributors: vec![],
                        }),
                    ),
                    assignment(
                        sink_input(sink, 5),
                        LogicalSignalId::GateOutput(GateIndex(0)),
                        primitive(source, vec![terminal(source, 0), terminal(source, 1)]),
                    ),
                    assignment(
                        sink_input(sink, 6),
                        LogicalSignalId::GateOutput(GateIndex(0)),
                        primitive(source, vec![terminal(later_source, 0)]),
                    ),
                    assignment(
                        sink_input(sink, 7),
                        LogicalSignalId::GateOutput(GateIndex(0)),
                        primitive(non_block, vec![terminal(non_block, 0)]),
                    ),
                    assignment(
                        sink_input(non_block, 0),
                        LogicalSignalId::GateOutput(GateIndex(0)),
                        primitive(source, vec![terminal(source, 0)]),
                    ),
                    assignment(
                        sink_input(sink, 8),
                        LogicalSignalId::GateOutput(GateIndex(0)),
                        primitive(source, vec![terminal(source, 2)]),
                    ),
                    assignment(
                        sink_input(sink, 9),
                        LogicalSignalId::GateOutput(GateIndex(0)),
                        primitive(source, vec![terminal(source, 0)]),
                    ),
                    assignment(
                        sink_input(sink, 1),
                        LogicalSignalId::GateOutput(GateIndex(1)),
                        primitive(source, vec![terminal(source, 0)]),
                    ),
                ],
                primary_inputs: vec![],
                declared_outputs: vec![],
                blocks: vec![
                    block(source, 0, vec![primary], vec![GateIndex(0), GateIndex(1)]),
                    block(later_source, 1, vec![primary], vec![GateIndex(2)]),
                    block(sink, 2, vec![primary; 9], vec![]),
                    block(later_sink, 3, vec![primary], vec![]),
                ],
            },
            structural_edges: vec![
                EdgeFacts {
                    source,
                    sink,
                    structural_slack_ticks: 5,
                },
                EdgeFacts {
                    source: later_source,
                    sink,
                    structural_slack_ticks: 0,
                },
                EdgeFacts {
                    source,
                    sink: later_sink,
                    structural_slack_ticks: 5,
                },
            ],
            source,
            sink,
            source_outputs: BTreeMap::from([(
                (source, 1),
                BlockPort {
                    cell: Anchor { x: 2, y: 0, z: 6 },
                    toward: Facing::East,
                },
            )]),
            sink_inputs: BTreeMap::from([(
                (sink, 1),
                BlockPort {
                    cell: Anchor { x: 8, y: 0, z: 2 },
                    toward: Facing::East,
                },
            )]),
            realised_offsets: BTreeMap::from([
                (
                    source,
                    Offset {
                        dx: 100,
                        dy: 0,
                        dz: 30,
                    },
                ),
                (
                    sink,
                    Offset {
                        dx: 200,
                        dy: 0,
                        dz: 10,
                    },
                ),
            ]),
            block_placements: BTreeMap::from([
                (source, BlockPlacementOffset { dx: 3, dz: 4 }),
                (sink, BlockPlacementOffset { dx: 7, dz: -5 }),
            ]),
        }
    }

    #[test]
    fn explicit_block_edges_accept_only_valid_block_terminals_and_sort_stably() {
        let fixture = block_proposal_fixture();

        assert_eq!(
            explicit_block_edges(&fixture.graph, &fixture.structural_edges),
            vec![
                BlockEdge {
                    source_block: InstanceId(11),
                    source_port: 0,
                    sink_block: fixture.sink,
                    sink_input: 2,
                    slack: 0,
                },
                BlockEdge {
                    source_block: fixture.source,
                    source_port: 0,
                    sink_block: fixture.sink,
                    sink_input: 0,
                    slack: 5,
                },
                BlockEdge {
                    source_block: fixture.source,
                    source_port: 1,
                    sink_block: fixture.sink,
                    sink_input: 1,
                    slack: 5,
                },
                BlockEdge {
                    source_block: fixture.source,
                    source_port: 0,
                    sink_block: InstanceId(21),
                    sink_input: 0,
                    slack: 5,
                },
            ],
            "primary inputs, malformed block terminals, invalid sink inputs, and non-block endpoints are not block edges",
        );
    }

    #[test]
    fn block_alignment_proposal_is_cumulative_and_moves_only_its_sink() {
        let fixture = block_proposal_fixture();
        let edge = BlockEdge {
            source_block: fixture.source,
            source_port: 1,
            sink_block: fixture.sink,
            sink_input: 1,
            slack: 5,
        };

        let proposal: BTreeMap<InstanceId, BlockPlacementOffset> = block_alignment_proposal(
            &edge,
            &fixture.source_outputs,
            &fixture.sink_inputs,
            &fixture.realised_offsets,
            &fixture.block_placements,
        );

        assert_eq!(
            proposal
                .iter()
                .map(|(&block, offset)| (block, (offset.dx, offset.dz)))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([(fixture.source, (3, 4)), (fixture.sink, (7, 19)),]),
            "the cumulative proposal retains every incumbent placement and updates only its sink",
        );
        assert_eq!(
            (proposal[&fixture.source].dx, proposal[&fixture.source].dz),
            (
                fixture.block_placements[&fixture.source].dx,
                fixture.block_placements[&fixture.source].dz,
            ),
        );

        let source_z = fixture.source_outputs[&(fixture.source, 1)].cell.z
            + fixture.realised_offsets[&fixture.source].dz;
        let incumbent = fixture.block_placements[&fixture.sink];
        let proposed = proposal[&fixture.sink];
        let moved_sink_z = fixture.sink_inputs[&(fixture.sink, 1)].cell.z
            + fixture.realised_offsets[&fixture.sink].dz
            + proposed.dz
            - incumbent.dz;
        assert_eq!(
            moved_sink_z, source_z,
            "the compiled port cells align in absolute Z"
        );
    }

    #[test]
    fn block_pull_x_moves_only_the_sink_one_cell_toward_the_source() {
        let mut fixture = block_proposal_fixture();
        let edge = BlockEdge {
            source_block: fixture.source,
            source_port: 1,
            sink_block: fixture.sink,
            sink_input: 1,
            slack: 5,
        };

        let proposal = block_pull_x_proposal(
            &edge,
            &fixture.source_outputs,
            &fixture.sink_inputs,
            &fixture.realised_offsets,
            &fixture.block_placements,
        )
        .expect("the sink is east of the source");

        assert_eq!(
            proposal
                .iter()
                .map(|(&block, offset)| (block, (offset.dx, offset.dz)))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([(fixture.source, (3, 4)), (fixture.sink, (6, -5))]),
            "only the sink moves one X cell and every accepted offset is preserved",
        );

        fixture.realised_offsets.get_mut(&fixture.sink).unwrap().dx = 94;
        assert!(
            block_pull_x_proposal(
                &edge,
                &fixture.source_outputs,
                &fixture.sink_inputs,
                &fixture.realised_offsets,
                &fixture.block_placements,
            )
            .is_none(),
            "equal realised port X produces no proposal",
        );
    }

    /// A three-level design (`top` -> `mid` -> two distinct leaves) small
    /// enough to compile inside a unit test.
    ///
    /// The shape is `alu4`'s, deliberately: **`mid` passes every one of its
    /// own inputs straight down to its children**, so not one of `mid`'s own
    /// gates names `x`, `y` or `z`, and `mid` declares no output carrying
    /// those names either. That is the only property that makes this
    /// fixture a test of anything -- a block whose `lowered` is its own
    /// gates instead of its flattening reads all three ports as consumed by
    /// nothing, and the union one level up retires the grandparent's
    /// delivery to routes that are really there.
    ///
    /// Two DISTINCT leaf modules, so the leaf worker pool has more than one
    /// name to hand out, and a middle module, so the sequential
    /// parent-compile loop runs too.
    fn three_level_design() -> HierarchicalNetlist {
        fn instance(name: &str, module: &str, ports: &[(&str, &str)]) -> ModuleInstance {
            ModuleInstance {
                name: name.to_string(),
                module: module.to_string(),
                ports: ports
                    .iter()
                    .map(|(port, signal)| {
                        (
                            (*port).to_string(),
                            PortBinding::Signal((*signal).to_string()),
                        )
                    })
                    .collect(),
            }
        }
        let mut modules = BTreeMap::new();
        // Leaf one: `w = u OR v`, as NOT(NOR(u, v)).
        modules.insert(
            "any_of".to_string(),
            Module {
                inputs: vec!["u".into(), "v".into()],
                outputs: vec!["w".into()],
                gates: vec![Gate::nor("t", &["u", "v"]), Gate::nor("w", &["t"])],
                instances: vec![],
            },
        );
        // Leaf two, a different module: `w = NOR(u, v)`.
        modules.insert(
            "neither".to_string(),
            Module {
                inputs: vec!["u".into(), "v".into()],
                outputs: vec!["w".into()],
                gates: vec![Gate::nor("w", &["u", "v"])],
                instances: vec![],
            },
        );
        modules.insert(
            "mid".to_string(),
            Module {
                inputs: vec!["x".into(), "y".into(), "z".into()],
                outputs: vec!["m".into()],
                // Reads only its children's outputs -- never x, y or z.
                gates: vec![Gate::nor("m", &["p", "q"])],
                instances: vec![
                    instance("lo", "any_of", &[("u", "x"), ("v", "y"), ("w", "p")]),
                    instance("hi", "neither", &[("u", "y"), ("v", "z"), ("w", "q")]),
                ],
            },
        );
        modules.insert(
            "top".to_string(),
            Module {
                inputs: vec!["a".into(), "b".into(), "c".into()],
                outputs: vec!["o".into()],
                gates: vec![Gate::nor("g", &["a", "b"]), Gate::nor("o", &["n", "g"])],
                instances: vec![instance(
                    "m0",
                    "mid",
                    &[("x", "a"), ("y", "b"), ("z", "c"), ("m", "n")],
                )],
            },
        );
        HierarchicalNetlist {
            top: "top".to_string(),
            modules,
        }
    }

    /// [`three_level_design`] specialised and lowered, with its module
    /// order -- the state `compile_hierarchical_with_threads` works from.
    fn lowered_three_level() -> (LoweredHierarchy, Vec<String>) {
        let design = three_level_design()
            .specialise_constants()
            .expect("no constants to specialise");
        let lowered = lower_hierarchy(&design).expect("lowers");
        let order = lowered.as_hierarchical().module_order().expect("acyclic");
        (lowered, order)
    }

    fn single_module(netlist: &Netlist, name: &str) -> HierarchicalNetlist {
        let mut modules = BTreeMap::new();
        modules.insert(
            name.to_string(),
            Module {
                inputs: netlist.inputs.clone(),
                outputs: netlist.outputs.clone(),
                gates: netlist.gates.clone(),
                instances: vec![],
            },
        );
        HierarchicalNetlist {
            top: name.to_string(),
            modules,
        }
    }

    #[test]
    fn an_unknown_module_is_returned_as_a_hierarchy_error_instead_of_panicking() {
        let design = HierarchicalNetlist {
            top: "top".to_string(),
            modules: BTreeMap::from([(
                "top".to_string(),
                Module {
                    inputs: vec![],
                    outputs: vec![],
                    gates: vec![],
                    instances: vec![ModuleInstance {
                        name: "u0".to_string(),
                        module: "missing".to_string(),
                        ports: BTreeMap::from([("a".to_string(), PortBinding::Zero)]),
                    }],
                },
            )]),
        };

        match design.specialise_constants() {
            Err(crate::compile::HierarchyError::UnknownModule { instance, module }) => {
                assert_eq!(instance, "u0");
                assert_eq!(module, "missing");
            }
            Err(other) => panic!("expected typed UnknownModule, got {other}"),
            Ok(_) => panic!("an unknown module must be rejected during specialisation"),
        }

        let outcome = std::panic::catch_unwind(|| {
            compile_hierarchical(&design, SynthesisBudget::Evaluations(0), None)
        });
        let error = match outcome {
            Ok(Err(error)) => error,
            Ok(Ok(_)) => panic!("an unknown module must be rejected"),
            Err(_) => panic!("an unknown module must return an error, not panic"),
        };
        match error {
            SynthesisError::Hierarchy(message) => {
                assert_eq!(message, "instance `u0` names unknown module `missing`")
            }
            other => panic!("expected the hierarchy error category, got {other}"),
        }
    }

    #[test]
    fn a_single_module_design_is_the_flat_compile_byte_for_byte() {
        for (name, netlist) in [
            ("and4", crate::circuits::and4::build_and4_netlist().0),
            (
                "full_adder",
                crate::circuits::full_adder::build_full_adder_netlist().0,
            ),
        ] {
            let lowered = crate::compile::lowering::lower_optimised(&netlist).unwrap();
            let flat = compile_fragment_synth(
                SynthesisInput {
                    lowered: &lowered,
                    source_provenance: None,
                    pins: None,
                },
                SynthesisBudget::Evaluations(0),
            )
            .unwrap();
            let hier = compile_hierarchical(
                &single_module(&netlist, name),
                SynthesisBudget::Evaluations(0),
                None,
            )
            .unwrap();
            assert_eq!(
                hier.candidate_fingerprint, flat.candidate_fingerprint,
                "{name}"
            );
            assert_eq!(hier.case_fingerprint, flat.case_fingerprint, "{name}");
            assert_eq!(hier.metrics, flat.metrics, "{name}");
        }
    }

    /// The precondition the pinned seven-segment test in
    /// `tests/build_circuit_pins.rs` rests on: that fixture only exposes an
    /// ALREADY lowered netlist, so wrapping it as a single-module
    /// `HierarchicalNetlist` makes `lower_hierarchy` lower it a second time.
    /// If that second pass were not the identity, the hierarchical front
    /// door would compile a different netlist and no fingerprint could
    /// match -- so state it here, where it is cheap, instead of finding out
    /// from a multi-minute integration test.
    #[test]
    fn lowering_an_already_lowered_netlist_is_the_identity() {
        let evaluator =
            crate::compile::fragment_synth::benchmark::legacy_benchmark_evaluator().unwrap();
        for case in ["pinned:verilog:seven_segment"] {
            let fixture = evaluator.fixture(case).unwrap();
            let netlist = fixture.lowered_netlist();
            assert_eq!(
                &crate::compile::lowering::lower_optimised(netlist).unwrap(),
                netlist,
                "{case} must be a fixed point of lowering"
            );
            let lowered = lower_hierarchy(&single_module(netlist, "seven_segment")).unwrap();
            assert_eq!(&lowered.flat, netlist, "{case} must flatten back to itself");
        }
    }

    #[test]
    fn a_two_level_design_certifies_and_reports_block_reuse() {
        let design = crate::circuits::hierarchical_builder::circuits::ripple_adder(2);
        let result = compile_hierarchical(&design, SynthesisBudget::Evaluations(0), None)
            .expect("certifies");
        assert!(result.metrics.quality.observed_settle > 0);
        assert!(result.compiled.output_positions.contains_key("s1"));
    }

    /// Three levels, end to end. `mid` is compiled as a parent, becomes a
    /// block, and is then stamped by `top` -- the recursion the whole
    /// module exists for, and the case that is broken by construction if a
    /// parent-turned-block carries its own gates instead of its flattening
    /// as its `lowered` netlist.
    #[test]
    fn a_three_level_design_certifies_end_to_end() {
        let design = three_level_design();

        // The fixture is only a test of anything while this holds.
        let mid = &design.modules["mid"];
        for port in &mid.inputs {
            assert!(
                !mid.gates
                    .iter()
                    .any(|gate| gate.inputs.contains(port) || &gate.output == port)
                    && !mid.outputs.contains(port),
                "`mid` must pass `{port}` straight to a child, as `alu4` does"
            );
        }
        assert_eq!(design.modules["top"].instances.len(), 1);
        assert_eq!(mid.instances.len(), 2);

        let result = compile_hierarchical(&design, SynthesisBudget::Evaluations(0), None)
            .expect("a three-level design must certify");
        assert!(result.metrics.quality.observed_settle > 0);
        assert!(result.compiled.output_positions.contains_key("o"));
    }

    /// The three-level acceptance circuit itself: `top` -> two `alu4` ->
    /// four `slice` each. Same shape as
    /// [`a_three_level_design_certifies_end_to_end`], two orders of
    /// magnitude bigger -- 19 primary inputs and several hundred flat
    /// gates, which is minutes of routing and certification, not seconds.
    ///
    /// Ignored so the fast suite stays fast; run it with
    /// `cargo test --lib compile::fragment_synth::hierarchy_api -- --ignored`.
    #[test]
    #[ignore = "minutes, not seconds: 2 x alu4 x 4 slices is the full acceptance circuit"]
    fn alu8_the_three_level_acceptance_circuit_certifies() {
        let design = crate::circuits::hierarchical_builder::circuits::alu8();
        let result = compile_hierarchical(&design, SynthesisBudget::Evaluations(0), None)
            .expect("alu8 must certify");
        assert!(result.metrics.quality.observed_settle > 0);
        for bit in 0..8 {
            assert!(result
                .compiled
                .output_positions
                .contains_key(&format!("r{bit}")));
        }
        assert!(result.compiled.output_positions.contains_key("cout"));
    }

    /// Both halves of `compile_blocks` have to run for this to mean
    /// anything: the leaf worker pool needs more than one name to hand out,
    /// and the sequential parent loop needs a module that instantiates
    /// something. [`three_level_design`] has two distinct leaves and one
    /// middle module, and the assertions below state that rather than
    /// trusting it -- a fixture that quietly loses its second leaf would
    /// make this test vacuous again.
    #[test]
    fn parallel_and_sequential_block_compiles_agree() {
        let design = three_level_design();
        let (lowered, order) = lowered_three_level();
        let instantiated = instantiated_modules(&lowered);
        let leaves: Vec<&String> = order
            .iter()
            .filter(|name| {
                instantiated.contains(*name) && lowered.modules[*name].instances.is_empty()
            })
            .collect();
        let parents: Vec<&String> = order
            .iter()
            .filter(|name| {
                instantiated.contains(*name) && !lowered.modules[*name].instances.is_empty()
            })
            .collect();
        assert_eq!(
            leaves.len(),
            2,
            "the worker pool must have real work: {leaves:?}"
        );
        assert_eq!(parents.len(), 1, "the parent loop must run: {parents:?}");

        let many =
            compile_hierarchical_with_threads(&design, SynthesisBudget::Evaluations(0), None, 4)
                .unwrap();
        let one =
            compile_hierarchical_with_threads(&design, SynthesisBudget::Evaluations(0), None, 1)
                .unwrap();
        assert_eq!(many.candidate_fingerprint, one.candidate_fingerprint);
        assert_eq!(many.case_fingerprint, one.case_fingerprint);
    }

    /// The budgeted search really runs over a hierarchical top: the
    /// block-edge stream is pulled, every proposal is compiled by
    /// `compile_module_with_blocks` (not the flat seed), and the incumbent
    /// survives whatever comes back.
    ///
    /// Every trace entry is proof the parent compiler ran: the stream emits
    /// no evaluation before it calls `compile_module_with_blocks` for its
    /// selected block edge.
    #[test]
    fn a_non_zero_budget_evaluates_real_proposals() {
        let design = crate::circuits::hierarchical_builder::circuits::ripple_adder(2);
        let baseline = compile_hierarchical(&design, SynthesisBudget::Evaluations(0), None)
            .expect("certifies");
        assert_eq!(baseline.evaluations_used, 0);
        assert!(baseline.trace.is_empty());

        let searched = compile_hierarchical(&design, SynthesisBudget::Evaluations(1), None)
            .expect("a budgeted hierarchical compile must still certify");

        assert_eq!(
            searched.evaluations_used, 1,
            "the carry edge must be evaluated"
        );
        assert_eq!(searched.trace.len(), 1);
        let trace = &searched.trace;
        let proposal = trace.first().expect("one trace entry asserted above");
        assert!(
            proposal.certified_quality.is_some(),
            "the carry alignment proposal must plan, union, and certify; terminal={:?}, trace={:?}",
            proposal.terminal,
            trace,
        );
        assert!(searched.compiled.output_positions.contains_key("s1"));
        assert!(
            searched.metrics.quality <= baseline.metrics.quality,
            "the search must never return worse than the seed it started from"
        );
    }

    /// A top with no gates of its own chaining five one-gate NOT blocks,
    /// alternating between two distinct leaf modules (`not_a`, `not_b`):
    /// `a -> g0 -> g1 -> g2 -> g3 -> g4 -> z`. Every block has exactly one
    /// input and one output, so the four `g(i) -> g(i+1)` connections are
    /// exactly the design's four block-to-block edges -- `a -> g0` and
    /// `g4 -> z` are primary-input/output bindings, not block edges.
    fn chain_of_five_not_blocks() -> HierarchicalNetlist {
        fn instance(name: &str, module: &str, ports: &[(&str, &str)]) -> ModuleInstance {
            ModuleInstance {
                name: name.to_string(),
                module: module.to_string(),
                ports: ports
                    .iter()
                    .map(|(port, signal)| {
                        (
                            (*port).to_string(),
                            PortBinding::Signal((*signal).to_string()),
                        )
                    })
                    .collect(),
            }
        }
        fn not_module() -> Module {
            Module {
                inputs: vec!["u".into()],
                outputs: vec!["w".into()],
                gates: vec![Gate::nor("w", &["u"])],
                instances: vec![],
            }
        }
        let mut modules = BTreeMap::new();
        modules.insert("not_a".to_string(), not_module());
        modules.insert("not_b".to_string(), not_module());
        modules.insert(
            "top".to_string(),
            Module {
                inputs: vec!["a".into()],
                outputs: vec!["z".into()],
                gates: vec![],
                instances: vec![
                    instance("g0", "not_a", &[("u", "a"), ("w", "n1")]),
                    instance("g1", "not_b", &[("u", "n1"), ("w", "n2")]),
                    instance("g2", "not_a", &[("u", "n2"), ("w", "n3")]),
                    instance("g3", "not_b", &[("u", "n3"), ("w", "n4")]),
                    instance("g4", "not_a", &[("u", "n4"), ("w", "z")]),
                ],
            },
        );
        HierarchicalNetlist {
            top: "top".to_string(),
            modules,
        }
    }

    /// Evaluation budgets 0/1/2/4 against the four-edge chain fixture: each
    /// smaller trace is an exact prefix of the larger one, quality never
    /// worsens as the budget grows, every budget stops for the same reason,
    /// and worker count cannot change the result. `Time(d)` shares the same
    /// ordered stream and is already covered by
    /// `a_time_budget_stops_only_after_the_crossing_proposal_finishes` in
    /// `search.rs`, so it is not re-tested here.
    #[test]
    fn evaluation_budgets_0_1_2_4_are_deterministic_quality_staircases() {
        let design = chain_of_five_not_blocks();
        let budgets = [0u64, 1, 2, 4];

        let results: Vec<_> = budgets
            .iter()
            .map(|&budget| {
                compile_hierarchical_with_threads(
                    &design,
                    SynthesisBudget::Evaluations(budget),
                    None,
                    1,
                )
                .unwrap_or_else(|error| panic!("budget {budget} must certify: {error}"))
            })
            .collect();

        for (result, &budget) in results.iter().zip(&budgets) {
            assert_eq!(result.evaluations_used, budget, "budget {budget}");
            assert_eq!(result.trace.len(), budget as usize, "budget {budget}");
            assert_eq!(
                result.stop_reason,
                StopReason::EvaluationBudget,
                "budget {budget} stops exactly at its limit, since the fixture offers four proposals"
            );
        }

        let budget_4_trace = &results[3].trace;
        assert_eq!(
            budget_4_trace.len(),
            4,
            "budget 4 evaluates exactly four proposals before EvaluationBudget stops it; \
             the fixture's four block-to-block edges are asserted separately, above, via the \
             comment on chain_of_five_not_blocks"
        );
        for result in &results[..3] {
            assert_eq!(
                result.trace,
                budget_4_trace[..result.trace.len()],
                "a smaller budget's trace must be an exact prefix of the larger one"
            );
        }

        for pair in results.windows(2) {
            assert!(
                pair[1].metrics.quality <= pair[0].metrics.quality,
                "quality must be a non-increasing staircase as the budget grows"
            );
        }
        let budget_zero_quality = results[0].metrics.quality;
        for result in &results[1..] {
            assert!(
                result.metrics.quality <= budget_zero_quality,
                "no larger budget may select worse than budget 0"
            );
        }

        let many =
            compile_hierarchical_with_threads(&design, SynthesisBudget::Evaluations(4), None, 4)
                .expect("budget 4 must certify with four workers");
        assert_eq!(many.candidate_fingerprint, results[3].candidate_fingerprint);
        assert_eq!(many.metrics.quality, results[3].metrics.quality);
        assert_eq!(many.trace, results[3].trace);
    }

    /// Exhausting the stream runs past the four alignment edges into the
    /// Pull-X stage, deterministically, with fragment fingerprints that never
    /// collide with the alignment ones (same edges, distinct schema).
    #[test]
    fn block_pull_x_stage_follows_alignment_deterministically() {
        let design = chain_of_five_not_blocks();
        let run = || {
            compile_hierarchical_with_threads(&design, SynthesisBudget::Evaluations(64), None, 1)
                .expect("exhausting the stream must certify")
        };
        let first = run();
        assert_eq!(first.stop_reason, StopReason::ProposalStreamExhausted);
        assert!(
            first.trace.len() > 4,
            "the stream must continue into Pull-X after the four alignment edges; trace={:?}",
            first.trace
        );
        let alignment: BTreeSet<_> = first.trace[..4]
            .iter()
            .map(|entry| entry.fragment_fingerprint.clone())
            .collect();
        for entry in &first.trace[4..] {
            assert!(
                !alignment.contains(&entry.fragment_fingerprint),
                "Pull-X fragment fingerprints use a distinct schema: {entry:?}"
            );
        }
        assert_eq!(run().trace, first.trace, "the full trace is deterministic");
    }

    /// The multiplier shape without compiling it: a stamped block whose
    /// input 3 is fed by a parent lever, so no block-to-block edge names
    /// it. The seam stage must still offer that input, and must offer it
    /// at the first stream position after alignment and Pull-X.
    #[test]
    fn seam_stage_proposes_lever_fed_block_inputs() {
        let fixture = block_proposal_fixture();
        let edges = explicit_block_edges(&fixture.graph, &fixture.structural_edges);
        assert!(
            edges.iter().all(|edge| edge.sink_input != 3),
            "input 3 is driven by PrimaryInput(0), never by a block edge"
        );
        let tree = crate::compile::fragment_synth::union::tests::seam_tree(3, 4);
        let at = first_internal_repeater(&tree).unwrap();
        let sink_index = fixture.graph.block(fixture.sink).unwrap().block;
        let seams = seam_descriptors(&fixture.graph, |block, input| {
            (block == sink_index && input == 3).then_some(&tree)
        });
        let lever_fed = InputSeamChoice {
            sink_block: fixture.sink,
            input: 3,
            at,
        };
        assert_eq!(seams, vec![lever_fed]);

        let stream = HierarchicalProposalStream::new(
            edges.clone(),
            fixture.source_outputs.clone(),
            fixture.sink_inputs.clone(),
            seams,
            Box::new(
                |_: &BTreeMap<InstanceId, BlockPlacementOffset>,
                 _: &[InputSeamChoice],
                 _: &[ParentRouteChoice]| {
                    unreachable!("the descriptor lookup never compiles")
                },
            ),
        );
        assert_eq!(stream.seam(edges.len() - 1), None, "alignment owns that index");
        assert_eq!(stream.seam(edges.len()), Some(lever_fed));
        assert_eq!(stream.seam(edges.len() + 1), None, "each descriptor is offered once");
    }

    /// The prune stage starts one past the last seam, counting the frozen
    /// Pull-X descriptors in between; it is frozen once, from whatever the
    /// incumbent's timing graph yields, and offers each descriptor exactly
    /// once. Its fingerprints follow the seam stage's shape: the fragment
    /// names only the route, the choice binds the incumbent as well.
    #[test]
    fn prune_stage_follows_seams_and_freezes_once() {
        let fixture = block_proposal_fixture();
        let edges = explicit_block_edges(&fixture.graph, &fixture.structural_edges);
        let seam = InputSeamChoice {
            sink_block: fixture.sink,
            input: 3,
            at: Anchor { x: 3, y: 0, z: 0 },
        };
        let mut stream = HierarchicalProposalStream::new(
            edges.clone(),
            fixture.source_outputs.clone(),
            fixture.sink_inputs.clone(),
            vec![seam],
            Box::new(
                |_: &BTreeMap<InstanceId, BlockPlacementOffset>,
                 _: &[InputSeamChoice],
                 _: &[ParentRouteChoice]| {
                    unreachable!("the descriptor lookup never compiles")
                },
            ),
        );
        // Pull-X frozen to one edge, as the alignment stage leaves it.
        stream.pull_x_edges = Some(vec![edges[0]]);
        let first = ParentRouteChoice { route: RouteId(7) };
        let second = ParentRouteChoice { route: RouteId(5) };
        let start = edges.len() + 1 + 1;
        assert_eq!(stream.seam(start - 1), Some(seam), "the seam stage owns the index before");
        assert_eq!(
            stream.prune(start - 1, || unreachable!("no freeze before the stage is reached")),
            None
        );
        assert_eq!(stream.prune(start, || vec![first, second]), Some(first));
        assert_eq!(stream.prune(start + 1, || unreachable!("frozen once")), Some(second));
        assert_eq!(
            stream.prune(start + 2, || unreachable!("frozen once")),
            None,
            "each descriptor is offered once"
        );

        let fragment = |prune: ParentRouteChoice| {
            serialized_fingerprint(&PruneFingerprint {
                schema: "hierarchical-parent-prune-fragment-v1",
                prune,
            })
        };
        let choice = |prune: ParentRouteChoice, incumbent_fingerprint: &str| {
            serialized_fingerprint(&PruneChoiceFingerprint {
                schema: "hierarchical-parent-prune-choice-v1",
                prune,
                incumbent_fingerprint,
            })
        };
        assert_eq!(fragment(first), fragment(first), "the fragment names only the route");
        assert_ne!(fragment(first), fragment(second));
        assert_ne!(choice(first, "a"), choice(first, "b"), "the choice binds the incumbent");
        assert_ne!(choice(first, "a"), fragment(first), "the two schemas never collide");

        // Only routes `prune_route` can change reach the sidecar: route 5
        // carries a redundant x5 refresh behind its x3 trunk repeater,
        // route 7 has nothing behind x3 to remove.
        let seam_tree = crate::compile::fragment_synth::union::tests::seam_tree;
        let mut redundant = seam_tree(3, 4);
        let x5 = Anchor { x: 5, y: 0, z: 0 };
        redundant.cells.iter_mut().find(|cell| cell.at == x5).unwrap().state =
            crate::compile::repeater(Facing::East);
        let routes = BTreeMap::from([(RouteId(5), redundant), (RouteId(7), seam_tree(3, 4))]);
        assert_eq!(
            prunable_parent_routes(&routes),
            BTreeSet::from([RouteId(5)]),
            "the unprunable route is dropped, the prunable one is kept"
        );
    }

    /// Parent Route Repack chooses the lowest-slack prunable parent route in
    /// `multiplier4`, changes no child block, survives the unchanged full
    /// certifier, and strictly improves quality.
    #[test]
    #[ignore = "minutes: compiles multiplier4's blocks and certifies the top twice"]
    fn parent_route_pruning_improves_multiplier4() {
        let design = crate::circuits::hierarchical_builder::circuits::multiplier4()
            .specialise_constants()
            .unwrap();
        let lowered = lower_hierarchy(&design).unwrap();
        let order = lowered.as_hierarchical().module_order().unwrap();
        let library = Library::default_library();
        let search_config = SearchConfig::checked_defaults();
        let services = seed_services(&library, &search_config);
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let blocks = compile_blocks(&lowered, &order, threads).expect("blocks compile");
        let ordered = ordered_blocks(&lowered, &lowered.top, &order, &blocks);
        let variant = SeedVariant::default();
        let (planning, graph) = parent_planning_graph(
            &lowered,
            &lowered.top,
            &ordered,
            &library,
            &variant,
        )
        .unwrap();
        let planned = plan_parent_with_services(
            SeedInput {
                lowered: &planning,
                source_provenance: None,
                pins: None,
            },
            services,
            graph,
            ParentBlocks { compiled: &ordered },
            &variant.placements,
            &BTreeMap::new(),
        )
        .expect("parent plans");
        let route_repeaters = |tree: &RealisedRouteTree| {
            tree.cells
                .iter()
                .filter(|cell| {
                    cell.state.kind == crate::redstone::world::block::BlockKind::Repeater
                })
                .count()
        };
        let prunable = planned
            .candidate
            .routes
            .iter()
            .filter_map(|(&route, tree)| {
                let mut clone = tree.clone();
                crate::compile::fragment_synth::route_opt::prune_route(&mut clone)
                    .then_some(route)
            })
            .collect::<BTreeSet<_>>();
        assert!(!prunable.is_empty(), "multiplier4 has prunable parent routes");

        let compile = |prunes: &[ParentRouteChoice]| {
            let started = std::time::Instant::now();
            let result = compile_module_with_blocks(
                &lowered,
                &lowered.top,
                &ordered,
                None,
                services,
                &variant,
                &BTreeMap::new(),
                &[],
                prunes,
            );
            println!(
                "parent prune test: {} route(s) compiled in {:?}: {:?}",
                prunes.len(),
                started.elapsed(),
                result.as_ref().map(|candidate| candidate.certified.metrics().quality)
            );
            result
        };
        let baseline = compile(&[]).expect("baseline certifies");
        let choice = prune_descriptors(
            baseline.certified.timing_graph(),
            &baseline.parent_routes,
        )
        .into_iter()
        .next()
        .expect("a timed parent route is prunable");
        assert!(
            baseline.parent_routes.keys().all(|route| prunable.contains(route)),
            "the sidecar offers only prunable parent routes"
        );
        let mut expected = planned.candidate.routes[&choice.route].clone();
        let before = route_repeaters(&expected);
        assert!(crate::compile::fragment_synth::route_opt::prune_route(
            &mut expected
        ));
        let removed = before - route_repeaters(&expected);
        let pruned = compile(&[choice]).expect("the prune proposal certifies");
        assert!(
            pruned.certified.metrics().quality < baseline.certified.metrics().quality,
            "parent pruning must strictly improve quality: pruned={:?} baseline={:?}",
            pruned.certified.metrics().quality,
            baseline.certified.metrics().quality
        );
        let all_repeaters = |candidate: &HierarchicalCandidate| {
            candidate
                .certified
                .candidate()
                .routes
                .values()
                .flat_map(|route| &route.cells)
                .filter(|cell| {
                    cell.state.kind == crate::redstone::world::block::BlockKind::Repeater
                })
                .count()
        };
        assert_eq!(
            all_repeaters(&pruned) + removed,
            all_repeaters(&baseline),
            "only the proven parent-route refreshes disappear"
        );
        assert_eq!(pruned.prunes, vec![choice]);
    }

    /// Block Pull-X on the real hierarchical `ripple_adder(8)`: the first
    /// block edge (structural slack first) with a feasible pull moves its
    /// sink one X cell toward the source, changes no compiled block,
    /// survives the unchanged full certifier, and strictly improves quality.
    #[test]
    #[ignore = "minutes: compiles ripple_adder(8)'s block and certifies the top twice"]
    fn block_pull_x_improves_ripple_adder8() {
        let design = crate::circuits::hierarchical_builder::circuits::ripple_adder(8)
            .specialise_constants()
            .unwrap();
        let lowered = lower_hierarchy(&design).unwrap();
        let order = lowered.as_hierarchical().module_order().unwrap();
        let library = Library::default_library();
        let search_config = SearchConfig::checked_defaults();
        let services = seed_services(&library, &search_config);
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let blocks = compile_blocks(&lowered, &order, threads).expect("blocks compile");
        let ordered = ordered_blocks(&lowered, &lowered.top, &order, &blocks);
        let blocks_before = ordered
            .iter()
            .map(|block| block.candidate.routes.clone())
            .collect::<Vec<_>>();
        let variant = SeedVariant::default();
        let (_, graph) = parent_planning_graph(
            &lowered,
            &lowered.top,
            &ordered,
            &library,
            &variant,
        )
        .unwrap();
        let block_delays = graph
            .blocks
            .iter()
            .map(|block| (block.id, ordered[block.block as usize].delay.0))
            .collect::<BTreeMap<_, _>>();
        let analysis = analyse_instance_dag(&graph, &block_delays).expect("dag analyses");
        let edges = explicit_block_edges(&graph, &analysis.edges);
        assert!(!edges.is_empty(), "ripple_adder(8) has block-to-block edges");
        let (source_outputs, sink_inputs) =
            compiled_port_lookup(&graph, &ordered).expect("compiled ports resolve");

        let compile = |placements: &BTreeMap<InstanceId, BlockPlacementOffset>| {
            let started = std::time::Instant::now();
            let result = compile_module_with_blocks(
                &lowered,
                &lowered.top,
                &ordered,
                None,
                services,
                &variant,
                placements,
                &[],
                &[],
            );
            println!(
                "pull-x test: {} placement(s) compiled in {:?}: {:?}",
                placements.len(),
                started.elapsed(),
                result.as_ref().map(|candidate| candidate.certified.metrics().quality)
            );
            result
        };
        let baseline = compile(&BTreeMap::new()).expect("baseline certifies");
        let (edge, proposal) = edges
            .iter()
            .find_map(|edge| {
                block_pull_x_proposal(
                    edge,
                    &source_outputs,
                    &sink_inputs,
                    &baseline.realised_block_offsets,
                    &baseline.block_placements,
                )
                .map(|proposal| (*edge, proposal))
            })
            .expect("ripple_adder(8) must have a block edge whose ports differ in X");
        let source_x = source_outputs[&(edge.source_block, edge.source_port)].cell.x
            + baseline.realised_block_offsets[&edge.source_block].dx;
        let sink_x = sink_inputs[&(edge.sink_block, edge.sink_input)].cell.x
            + baseline.realised_block_offsets[&edge.sink_block].dx;
        let step = (source_x - sink_x).signum();
        println!("pull-x test: edge {edge:?}, step {step}, proposal {proposal:?}");

        let pulled = compile(&proposal).expect("the pull-x proposal certifies");
        println!(
            "pull-x test: baseline {:?} pulled {:?}",
            baseline.certified.metrics().quality,
            pulled.certified.metrics().quality
        );
        assert!(
            pulled.certified.metrics().quality < baseline.certified.metrics().quality,
            "pull-x must strictly improve quality: pulled={:?} baseline={:?}",
            pulled.certified.metrics().quality,
            baseline.certified.metrics().quality
        );
        assert_eq!(
            ordered
                .iter()
                .map(|block| block.candidate.routes.clone())
                .collect::<Vec<_>>(),
            blocks_before,
            "the compiled blocks are never mutated"
        );
        // `BlockPlacementOffset` has no `PartialEq`; compare as plain tuples.
        let flat = |placements: &BTreeMap<InstanceId, BlockPlacementOffset>| {
            placements
                .iter()
                .map(|(&block, offset)| (block, (offset.dx, offset.dz)))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(
            flat(&proposal),
            BTreeMap::from([(edge.sink_block, (step, 0))]),
            "the proposal moves only the sink, one X toward the source"
        );
        for (block, before) in &baseline.realised_block_offsets {
            let after = pulled.realised_block_offsets[block];
            let expected_dx = if *block == edge.sink_block {
                before.dx + step
            } else {
                before.dx
            };
            assert_eq!(
                (after.dx, after.dy, after.dz),
                (expected_dx, before.dy, before.dz),
                "only the chosen sink {:?} moves, by one X toward the source",
                edge.sink_block
            );
        }
        assert_eq!(
            flat(&pulled.block_placements),
            flat(&proposal),
            "the candidate carries the proposed placement map"
        );
    }

    /// Input Seam Absorption applied to ONE stamped `adder_row` of
    /// `multiplier4` (the parent stamps three): the parent's boundary
    /// repeater on the lever stays, the selected child repeater becomes
    /// dust in that instance only, the route repeater count falls by
    /// exactly one, the compiled block is untouched, the unchanged full
    /// certifier accepts the result, and quality strictly improves.
    ///
    /// `adder_row/x1` is the input the local strength walk accepts;
    /// `full_adder` and `slice` inputs are all refused by it.
    #[test]
    #[ignore = "minutes: compiles multiplier4's blocks and certifies the top twice"]
    fn input_seam_absorption_removes_the_child_refresh() {
        let design = crate::circuits::hierarchical_builder::circuits::multiplier4();
        let design = design.specialise_constants().unwrap();
        let lowered = lower_hierarchy(&design).unwrap();
        let order = lowered.as_hierarchical().module_order().unwrap();
        let library = Library::default_library();
        let search_config = SearchConfig::checked_defaults();
        let services = seed_services(&library, &search_config);
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let blocks = compile_blocks(&lowered, &order, threads).expect("blocks compile");
        let ordered = ordered_blocks(&lowered, &lowered.top, &order, &blocks);
        let (_, graph) = parent_planning_graph(
            &lowered,
            &lowered.top,
            &ordered,
            &library,
            &SeedVariant::default(),
        )
        .unwrap();
        let row = |name: &str| {
            graph
                .blocks
                .iter()
                .find(|block| block.path == [name.to_string()])
                .unwrap_or_else(|| panic!("top stamps `{name}`"))
        };
        let (sink_block, other_block) = (row("row2"), row("row1"));
        let compiled = &ordered[sink_block.block as usize];
        assert_eq!(compiled.module, "adder_row");
        assert_eq!(other_block.block, sink_block.block, "both rows stamp one block");
        let compiled_before = compiled.candidate.routes.clone();
        let input = u16::try_from(
            compiled
                .lowered
                .inputs
                .iter()
                .position(|port| port == "x1")
                .expect("adder_row declares x1"),
        )
        .unwrap();
        let at = input_route(&compiled.candidate, input)
            .and_then(first_internal_repeater)
            .expect("adder_row's x1 route carries a non-terminal repeater");
        let (other, sink) = (other_block.id, sink_block.id);
        let choice = InputSeamChoice {
            sink_block: sink,
            input,
            at,
        };
        let compile = |seams: &[InputSeamChoice]| {
            let started = std::time::Instant::now();
            let result = compile_module_with_blocks(
                &lowered,
                &lowered.top,
                &ordered,
                None,
                services,
                &SeedVariant::default(),
                &BTreeMap::new(),
                seams,
                &[],
            );
            println!(
                "seam test: {} seam(s) compiled in {:?}: {:?}",
                seams.len(),
                started.elapsed(),
                result.as_ref().map(|c| c.certified.metrics().quality)
            );
            result
        };
        let baseline = compile(&[]).expect("baseline certifies");
        let seamed = compile(&[choice]).expect("the seam proposal must certify unchanged");
        assert_eq!(
            compiled.candidate.routes, compiled_before,
            "the compiled block is never mutated"
        );
        assert!(
            seamed.certified.metrics().quality < baseline.certified.metrics().quality,
            "seam must strictly improve quality: seamed={:?} baseline={:?}",
            seamed.certified.metrics().quality,
            baseline.certified.metrics().quality
        );

        let kind_at = |candidate: &HierarchicalCandidate, at: Anchor| {
            candidate
                .certified
                .candidate()
                .routes
                .values()
                .flat_map(|route| &route.cells)
                .find(|cell| cell.at == at)
                .map(|cell| cell.state.kind)
        };
        let repeaters = |candidate: &HierarchicalCandidate| {
            candidate
                .certified
                .candidate()
                .routes
                .values()
                .flat_map(|route| &route.cells)
                .filter(|cell| cell.state.kind == crate::redstone::world::block::BlockKind::Repeater)
                .count()
        };
        let shift = |block: InstanceId, at: Anchor| {
            let offset = seamed.realised_block_offsets[&block];
            Anchor {
                x: at.x + offset.dx,
                y: at.y + offset.dy,
                z: at.z + offset.dz,
            }
        };
        let lever = shift(sink, compiled.inputs["x1"].cell);
        let kind = crate::redstone::world::block::BlockKind::Repeater;
        assert_eq!(kind_at(&baseline, shift(sink, at)), Some(kind));
        assert_eq!(
            kind_at(&seamed, shift(sink, at)),
            Some(crate::redstone::world::block::BlockKind::RedstoneWire),
            "the selected child repeater becomes dust"
        );
        assert_eq!(
            kind_at(&seamed, lever),
            Some(kind),
            "the parent boundary repeater is retained"
        );
        assert_eq!(
            kind_at(&seamed, shift(other, at)),
            Some(kind),
            "the sibling instance is untouched"
        );
        assert_eq!(repeaters(&seamed) + 1, repeaters(&baseline));
        assert_eq!(seamed.seams, vec![choice]);
    }

    /// The two proposals a block-stamping parent cannot represent, and the
    /// one it can.
    ///
    /// `compile_module_with_blocks` is the whole point of the pluggable
    /// compiler seam, so its guards are stated directly rather than left to
    /// whichever variant the stream happens to choose.
    #[test]
    fn a_parent_refuses_only_the_proposals_it_cannot_represent() {
        let (lowered, order) = lowered_three_level();
        let library = Library::default_library();
        let search_config = SearchConfig::checked_defaults();
        let services = seed_services(&library, &search_config);
        let blocks = compile_blocks(&lowered, &order, 1).expect("blocks compile");
        let ordered = ordered_blocks(&lowered, &lowered.top, &order, &blocks);
        let parent_gates =
            u32::try_from(lowered.modules[&lowered.top].gates.len()).expect("narrow");
        assert!(parent_gates > 0, "the top must own gates of its own");
        let block_gate = InstanceId(parent_gates);
        let compile = |variant: &SeedVariant| {
            compile_module_with_blocks(
                &lowered,
                &lowered.top,
                &ordered,
                None,
                services,
                variant,
                &BTreeMap::new(),
                &[],
                &[],
            )
        };

        let certified = compile(&SeedVariant::default()).expect("the default variant plans");

        // A proposal naming one of the parent's OWN gates is a proposal the
        // parent can act on, and must not be refused. Re-stating the
        // placement the seed already chose keeps this about the guard
        // rather than about whether some other placement routes.
        let facing = certified
            .certified
            .candidate()
            .placements
            .iter()
            .find(|(id, _)| id.instance == InstanceId(0))
            .map(|(_, placement)| placement.facing)
            .expect("the parent's first gate is placed");
        let mut representable = SeedVariant::default();
        representable.placements.insert(
            InstanceId(0),
            InstancePlacementOverride {
                facing,
                dx: 0,
                dz: 0,
            },
        );
        compile(&representable).expect("a variant over the parent's own gates is representable");

        let mut implements_a_block_gate = SeedVariant::default();
        implements_a_block_gate.implementations.insert(
            block_gate,
            ImplementationKey::Merge {
                isolation_mask: InputMask::new(0),
            },
        );
        let mut places_a_block_gate = SeedVariant::default();
        places_a_block_gate.placements.insert(
            block_gate,
            InstancePlacementOverride {
                facing,
                dx: 0,
                dz: 0,
            },
        );
        // Rendered rather than matched: a `CertifiedCandidate`'s own `Debug`
        // is the entire compiled world, which is not what anyone wants out
        // of a failing assertion.
        let refusal = |variant: &SeedVariant| match compile(variant) {
            Err(SeedError::Incomplete(reason)) => reason.to_string(),
            Err(other) => format!("some other error: {other}"),
            Ok(_) => "certified".to_string(),
        };

        for variant in [implements_a_block_gate, places_a_block_gate] {
            assert_eq!(refusal(&variant), "proposal targets a block gate");
        }

        let mut duplicates = SeedVariant::default();
        duplicates.duplicates.push(DuplicateRequest {
            canonical: InstanceId(0),
            ordinal: 1,
            sinks: BTreeSet::new(),
        });
        assert_eq!(
            refusal(&duplicates),
            "a duplicate proposal cannot be represented in a parent that stamps blocks"
        );
    }

    /// The hierarchy is part of the case, not just its flattening. Two
    /// designs that flatten to the same netlist compile differently -- one
    /// stamps a block twice, the other places every gate itself -- so they
    /// must not share a case fingerprint.
    #[test]
    fn the_hierarchy_is_part_of_the_case_fingerprint() {
        let design = crate::circuits::hierarchical_builder::circuits::ripple_adder(2);
        let (flat, _) = design.flatten().expect("flattens");
        let flattened_design = single_module(&flat, &design.top);
        assert_ne!(
            hierarchy_descriptor_bytes(&design),
            hierarchy_descriptor_bytes(&flattened_design),
            "a design and its own flattening must not describe the same case"
        );

        // And any single edit anywhere in the hierarchy changes it too.
        let mut renamed_instance = design.clone();
        renamed_instance.modules.get_mut("top").unwrap().instances[0].name = "renamed".into();
        assert_ne!(
            hierarchy_descriptor_bytes(&design),
            hierarchy_descriptor_bytes(&renamed_instance)
        );

        let mut rebound_port = design.clone();
        rebound_port.modules.get_mut("top").unwrap().instances[0]
            .ports
            .insert("cin".to_string(), PortBinding::Zero);
        assert_ne!(
            hierarchy_descriptor_bytes(&design),
            hierarchy_descriptor_bytes(&rebound_port)
        );
    }
}

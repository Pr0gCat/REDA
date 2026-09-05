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
use crate::compile::fragment_synth::blocks::{compile_block, CompiledBlock};
use crate::compile::fragment_synth::certification::{
    CertifiedCandidate, CompleteCandidateCertifier,
};
use crate::compile::fragment_synth::compile_fragment_synth;
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::fragment::FragmentProposalStream;
use crate::compile::fragment_synth::instance_graph::{BlockSpec, InstanceGraph};
use crate::compile::fragment_synth::search::{
    run_budgeted_proposals, SynthesisBudget, SystemMonotonicClock,
};
use crate::compile::fragment_synth::seed::{
    certify_planned, plan_parent_with_services, ParentBlocks, SeedError, SeedInput, SeedServices,
    SeedVariant,
};
use crate::compile::fragment_synth::services::{
    DurableSeedEmitter, DurableSeedVerifier, TopologyAwareSeedPlacer,
};
use crate::compile::fragment_synth::union::{
    planning_netlist, union_candidate, BlockSpecOwned, UnionInput,
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

    let compile_variant = |variant: &SeedVariant| -> Result<CertifiedCandidate, SeedError> {
        compile_module_with_blocks(&lowered, &lowered.top, &ordered, pins, services, variant)
    };
    let certified = compile_variant(&SeedVariant::default())
        .map_err(|error| SynthesisError::Seed(format!("{}: {error}", lowered.top)))?;

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
    let seed_input = SeedInput::from(&flat_input);
    let mut proposals =
        FragmentProposalStream::with_compiler(seed_input, services, Box::new(compile_variant));
    let summary = run_budgeted_proposals(certified, budget, &clock, &mut proposals);

    let compiled = compiled_from_certified(&summary.best, &lowered.flat)?;
    let metrics = summary.best.metrics().clone();
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
) -> Result<CertifiedCandidate, SeedError> {
    let (planning, owned) = planning_netlist(lowered, module, ordered);
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

    let specs: Vec<BlockSpec<'_>> = owned.iter().map(BlockSpecOwned::as_spec).collect();
    let graph = InstanceGraph::with_blocks_and_implementations(
        &planning,
        services.library,
        &specs,
        &variant.implementations,
    )?;
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
    )?;
    let (flat, paths) = module_flattening(lowered, module)?;
    let union = union_candidate(UnionInput {
        parent: &planned,
        blocks: ordered,
        flat: &flat,
        paths: &paths,
        library: services.library,
    })
    .map_err(|error| SeedError::Union(error.to_string()))?;
    certify_planned(union, &flat, services)
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
        )
        .map_err(|error| SynthesisError::Seed(format!("{name}: {error}")))?;
        let block = CompiledBlock::from_certified(name, &lowered.block_netlist(name), &certified)
            .map_err(|error| SynthesisError::Block {
            module: name.clone(),
            first_path: first_instance_path(lowered, name),
            reason: error.to_string(),
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
    use crate::compile::hierarchy::Module;

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

    #[test]
    fn parallel_and_sequential_block_compiles_agree() {
        let design = crate::circuits::hierarchical_builder::circuits::ripple_adder(2);
        let many =
            compile_hierarchical_with_threads(&design, SynthesisBudget::Evaluations(0), None, 4)
                .unwrap();
        let one =
            compile_hierarchical_with_threads(&design, SynthesisBudget::Evaluations(0), None, 1)
                .unwrap();
        assert_eq!(many.candidate_fingerprint, one.candidate_fingerprint);
        assert_eq!(many.case_fingerprint, one.case_fingerprint);
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

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
        // The netlist this block carries must be the one its candidate was
        // certified against -- this module's whole FLATTENING, grandchildren
        // included -- not `block_netlist`, which is the module's own gates
        // only. See `CompiledBlock::lowered`: a block whose netlist is
        // narrower than its candidate is internally inconsistent, and the
        // union one level up reads that netlist to decide which of the
        // block's ports anything behind them actually consumes.
        let (flat, _) = module_flattening(lowered, name)
            .map_err(|error| SynthesisError::Seed(format!("{name}: {error}")))?;
        let block = CompiledBlock::from_certified(name, &flat, &certified).map_err(|error| {
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
    use crate::compile::fragment_synth::identity::{ImplementationKey, InputMask, InstanceId};
    use crate::compile::fragment_synth::instance_graph::DuplicateRequest;
    use crate::compile::fragment_synth::seed::InstancePlacementOverride;
    use crate::compile::hierarchy::{Module, ModuleInstance};
    use crate::compile::Gate;

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
            SynthesisError::Hierarchy(message) => assert_eq!(
                message,
                "instance `u0` names unknown module `missing`"
            ),
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
    /// proposal stream is pulled, every proposal is compiled by
    /// `compile_module_with_blocks` (not the flat seed), and the incumbent
    /// survives whatever comes back.
    ///
    /// Every trace entry is proof the pluggable compiler ran: the stream
    /// records a terminal only after `(self.compile)(&variant)` returned,
    /// and the one terminal it can produce without calling the compiler
    /// (`BacktrackCapExhausted` from the shell-ordinal cap) is asserted
    /// against below.
    #[test]
    fn a_non_zero_budget_evaluates_real_proposals() {
        let design = three_level_design();
        let baseline = compile_hierarchical(&design, SynthesisBudget::Evaluations(0), None)
            .expect("certifies");
        assert_eq!(baseline.evaluations_used, 0);
        assert!(baseline.trace.is_empty());

        let searched = compile_hierarchical(&design, SynthesisBudget::Evaluations(2), None)
            .expect("a budgeted hierarchical compile must still certify");

        assert_eq!(searched.evaluations_used, 2, "the stream must be pulled");
        assert_eq!(searched.trace.len(), 2);
        for entry in &searched.trace {
            assert_ne!(
                entry.terminal,
                crate::compile::fragment_synth::search::ProposalTerminal::BacktrackCapExhausted,
                "a proposal that never reached the parent compiler proves nothing"
            );
        }
        // Stronger than "it was called": at least one proposal came back
        // with a certified candidate, so a variant really was re-planned
        // around the blocks, unioned and certified. Both do today; one is
        // asserted so a single proposal drifting to a refusal does not
        // silently empty the test.
        assert!(
            searched
                .trace
                .iter()
                .any(|entry| entry.certified_quality.is_some()),
            "no proposal was actually planned and certified: {:?}",
            searched
                .trace
                .iter()
                .map(|entry| entry.terminal)
                .collect::<Vec<_>>()
        );
        assert!(searched.compiled.output_positions.contains_key("o"));
        assert!(
            searched.metrics.quality <= baseline.metrics.quality,
            "the search must never return worse than the seed it started from"
        );
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
            compile_module_with_blocks(&lowered, &lowered.top, &ordered, None, services, variant)
        };

        let certified = compile(&SeedVariant::default()).expect("the default variant plans");

        // A proposal naming one of the parent's OWN gates is a proposal the
        // parent can act on, and must not be refused. Re-stating the
        // placement the seed already chose keeps this about the guard
        // rather than about whether some other placement routes.
        let facing = certified
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

//! What the module tree looks like after optimisation: which instance each
//! lowered gate belongs to, and how many structurally different kinds each
//! module's instances fall into -- the layouts hierarchical generation would
//! have to build.
//!
//! ```text
//! cargo run --release --example hierarchy_probe -- <file.sv> <top>
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap};

use reda::compile::lowering::lower_optimised_with_provenance;
use reda::compile::Netlist;
use reda::frontend::debug::InstancePathId;
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (path, top) = (&args[0], &args[1]);
    let text = std::fs::read_to_string(path).expect("readable source");
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: top,
            text: &text,
        }],
        &CompileOptions::new(top),
    )
    .expect("frontend");
    let debug = &artifact.debug;
    let lowered = lower_optimised_with_provenance(&artifact.netlist).expect("lowering");
    let netlist = &lowered.netlist;

    // The instance of every lowered gate, through the source gate it came from.
    let owner: Vec<InstancePathId> = lowered
        .provenance
        .iter()
        .map(|&source| {
            debug
                .instance_of_gate(&debug.gates[source])
                .unwrap_or(InstancePathId(0))
        })
        .collect();
    let instances = &debug.instances;
    let within = |gate: usize, node: usize| {
        let mut at = Some(owner[gate]);
        while let Some(instance) = at {
            if instance.0 as usize == node {
                return true;
            }
            at = instances[instance.0 as usize].parent;
        }
        false
    };

    println!(
        "{top}: {} lowered gates, {} instances",
        netlist.gates.len(),
        instances.len()
    );
    let mut kinds: BTreeMap<&str, BTreeMap<u64, Vec<usize>>> = BTreeMap::new();
    for (node, info) in instances.iter().enumerate() {
        let own = owner
            .iter()
            .filter(|instance| instance.0 as usize == node)
            .count();
        let subtree: Vec<usize> = (0..netlist.gates.len())
            .filter(|&gate| within(gate, node))
            .collect();
        kinds
            .entry(info.module.as_str())
            .or_default()
            .entry(signature(netlist, &subtree))
            .or_default()
            .push(node);
        let depth =
            std::iter::successors(info.parent, |parent| instances[parent.0 as usize].parent)
                .count();
        println!(
            "  {}{} ({}): {} gates of its own, {} in its subtree",
            "  ".repeat(depth),
            if info.name.is_empty() {
                "<top>"
            } else {
                &info.name
            },
            info.module,
            own,
            subtree.len()
        );
    }
    if std::env::var_os("PROBE_ROOT").is_some() {
        for (gate, instance) in owner.iter().enumerate() {
            if std::env::var("PROBE_GATE").map_or(instance.0 == 0, |name| {
                artifact.netlist.gates[lowered.provenance[gate]].output == name
            }) {
                let source = lowered.provenance[gate];
                let origins = debug.origins_for_gate(&debug.gates[source]);
                eprintln!(
                    "  root gate {} {:?} <- source {} {:?} origins {}",
                    netlist.gates[gate].output,
                    netlist.gates[gate].kind,
                    artifact.netlist.gates[source].output,
                    artifact.netlist.gates[source].kind,
                    origins.len()
                );
                if std::env::var_os("PROBE_ORIGINS").is_some() {
                    for origin in &origins {
                        let elab = &debug.elab_nodes[origin.elab.0 as usize];
                        eprintln!(
                            "      {:?} instance {:?} extra {} synthetic {:?}",
                            elab.kind, elab.instance, origin.extra, origin.synthetic
                        );
                    }
                }
            }
        }
    }
    println!("kinds after optimisation (instances per structurally distinct layout):");
    for (module, by_kind) in &kinds {
        let counts: Vec<String> = by_kind
            .values()
            .map(|nodes| {
                let names: Vec<&str> = nodes
                    .iter()
                    .map(|&node| instances[node].name.as_str())
                    .collect();
                format!("{} [{}]", nodes.len(), names.join(" "))
            })
            .collect();
        println!(
            "  {module}: {} kind(s): {}",
            by_kind.len(),
            counts.join(", ")
        );
    }
}

fn hash(value: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// A structural hash of the gates in `subtree`, blind to signal names:
/// Weisfeiler-Lehman refinement over the gates, with every signal read from
/// outside the subtree an anonymous input and every gate whose output leaves
/// it marked as a port. Equal hashes are taken as one layout.
fn signature(netlist: &Netlist, subtree: &[usize]) -> u64 {
    let inside: BTreeSet<usize> = subtree.iter().copied().collect();
    let driver: HashMap<&str, usize> = subtree
        .iter()
        .map(|&gate| (netlist.gates[gate].output.as_str(), gate))
        .collect();
    let exported: BTreeSet<usize> = subtree
        .iter()
        .copied()
        .filter(|&gate| {
            let output = &netlist.gates[gate].output;
            netlist.outputs.contains(output)
                || netlist.gates.iter().enumerate().any(|(other, reader)| {
                    !inside.contains(&other) && reader.inputs.contains(output)
                })
        })
        .collect();
    let mut label: HashMap<usize, u64> = subtree
        .iter()
        .map(|&gate| {
            let kind = format!("{:?}", netlist.gates[gate].kind);
            (
                gate,
                hash(&(
                    kind,
                    netlist.gates[gate].inputs.len(),
                    exported.contains(&gate),
                )),
            )
        })
        .collect();
    for _ in 0..subtree.len().min(32) {
        let next: HashMap<usize, u64> = subtree
            .iter()
            .map(|&gate| {
                let mut reads: Vec<u64> = netlist.gates[gate]
                    .inputs
                    .iter()
                    .map(|input| driver.get(input.as_str()).map_or(0, |source| label[source]))
                    .collect();
                reads.sort_unstable();
                (gate, hash(&(label[&gate], reads)))
            })
            .collect();
        label = next;
    }
    let mut all: Vec<u64> = label.into_values().collect();
    all.sort_unstable();
    hash(&all)
}

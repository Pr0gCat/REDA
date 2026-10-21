//! Generation that follows the module tree.
//!
//! Logic is optimised flat; the module tree only says how to lay it out.
//! Each kept instance becomes a block: its own gates and its children's
//! blocks, generated once per structurally distinct kind and placed whole by
//! its parent. Instances whose subtree holds fewer than `grain` gates are
//! dissolved into their parent.
//!
//! Planning runs top-down: each parent places its children as soft blocks
//! of an estimated size, choosing each one's shape and where it goes, and
//! tells it when its inputs arrive. Generation then runs bottom-up, kinds
//! of the same height in parallel, deepest first. A block's ports stand on
//! top of it, wherever suits its own gates, and its parent's routes run
//! over it to reach them.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rayon::prelude::*;

use super::{generate, plan, BlockUse, Construction, ConstructError, Frame, Method, PortPlace, Tile};
use crate::compile::{Gate, Netlist};

/// One kept node of the layout tree: its own gates and its kept children.
struct Node {
    gates: Vec<usize>,
    children: Vec<usize>,
    /// Its ports: the signals it reads from outside and those it drives
    /// that are read outside or are primary outputs, in canonical order.
    inputs: Vec<String>,
    outputs: Vec<String>,
}

/// Generate `netlist` along a module tree: `owner[g]` is the instance that
/// built gate `g`, and `parents[i]` instance `i`'s parent (instance 0, the
/// top, has none). The top's ports go where `places` says, as for
/// [`super::construct`].
pub fn construct_hierarchical(
    netlist: &Netlist,
    owner: &[usize],
    parents: &[Option<usize>],
    grain: usize,
    places: &BTreeMap<String, PortPlace>,
) -> Result<Construction, ConstructError> {
    let count = parents.len();
    // Subtree sizes, then which instances are kept and where the rest go.
    let mut own = vec![0usize; count];
    for &instance in owner {
        own[instance] += 1;
    }
    let mut size = own.clone();
    for instance in (1..count).rev() {
        if let Some(parent) = parents[instance] {
            size[parent] += size[instance];
        }
    }
    let kept: Vec<bool> = (0..count).map(|instance| instance == 0 || (size[instance] >= grain && size[instance] > 0)).collect();
    let keeper = |mut instance: usize| {
        while !kept[instance] {
            instance = parents[instance].expect("the top is kept");
        }
        instance
    };
    let mut nodes: HashMap<usize, Node> = (0..count)
        .filter(|&instance| kept[instance])
        .map(|instance| {
            (
                instance,
                Node {
                    gates: Vec::new(),
                    children: Vec::new(),
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                },
            )
        })
        .collect();
    for (gate, &instance) in owner.iter().enumerate() {
        nodes.get_mut(&keeper(instance)).expect("a kept node").gates.push(gate);
    }
    for instance in 1..count {
        if kept[instance] {
            let parent = keeper(parents[instance].expect("only the top has no parent"));
            nodes.get_mut(&parent).expect("a kept node").children.push(instance);
        }
    }

    // Ports, children first: a node reads what its items read and it does
    // not drive; it exports what it drives that anything outside reads.
    let mut order = Vec::new();
    post_order(&nodes, 0, &mut order);
    let produced_by: HashMap<&str, usize> =
        netlist.gates.iter().enumerate().map(|(index, gate)| (gate.output.as_str(), index)).collect();
    let mut readers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, gate) in netlist.gates.iter().enumerate() {
        for input in &gate.inputs {
            readers.entry(input.as_str()).or_default().push(index);
        }
    }
    let mut subtree: HashMap<usize, Vec<bool>> = HashMap::new();
    for &node in &order {
        let mut inside = vec![false; netlist.gates.len()];
        for &gate in &nodes[&node].gates {
            inside[gate] = true;
        }
        for child in nodes[&node].children.clone() {
            for (gate, &is) in subtree[&child].iter().enumerate() {
                inside[gate] |= is;
            }
        }
        let (inputs, outputs) = ports(netlist, &nodes, node, &inside, &produced_by, &readers);
        let entry = nodes.get_mut(&node).expect("a kept node");
        entry.inputs = inputs;
        entry.outputs = outputs;
        subtree.insert(node, inside);
    }

    // Estimated areas, in slots, children first: a node's lattice holds its
    // gates, its ports and its children's blocks with the slack placement
    // gives them.
    let mut estimate: HashMap<usize, usize> = HashMap::new();
    for &node in &order {
        let entry = &nodes[&node];
        let blocks: usize = entry.children.iter().map(|child| estimate[child]).sum();
        let area = entry.gates.len() + entry.inputs.len() + entry.outputs.len() + blocks;
        let slack = if entry.children.is_empty() { 1.15 } else { 1.6 };
        estimate.insert(node, ((area as f64 * slack).ceil() as usize).max(1));
    }

    // Estimated delays: the longest gate path inside each subtree, each
    // gate a gate's ticks and a slot of wire.
    let combinational = netlist.combinational_order().expect("generation checks for loops");
    let mut longest: HashMap<usize, f64> = HashMap::new();
    for &node in &order {
        let inside = &subtree[&node];
        let mut level: HashMap<&str, usize> = HashMap::new();
        for &gate in &combinational {
            if inside[gate] {
                let gate = &netlist.gates[gate];
                let deepest = gate.inputs.iter().filter_map(|input| level.get(input.as_str())).max().copied().unwrap_or(0);
                level.insert(gate.output.as_str(), deepest + 1);
            }
        }
        let depth = level.values().copied().max().unwrap_or(0);
        longest.insert(node, depth as f64 * (super::placement::GATE_TICKS + super::placement::SLOT_TICKS));
    }

    // Frames, parents first, a depth at a time in parallel.
    let mut frames: HashMap<usize, Frame> = HashMap::new();
    frames.insert(0, Frame::top(netlist, places));
    let mut depth = vec![0usize];
    let planning = std::time::Instant::now();
    while !depth.is_empty() {
        let planned: Vec<Vec<(usize, Frame)>> = depth
            .par_iter()
            .filter(|node| !nodes[node].children.is_empty())
            .map(|&node| {
                let entry = &nodes[&node];
                let netlist = if node == 0 { top_netlist(netlist, entry) } else { own_netlist(netlist, entry) };
                let soft = Arc::new(Tile {
                    cells: Vec::new(),
                    exits: Vec::new(),
                    source: Vec::new(),
                    stubs: Vec::new(),
                    probe: None,
                    ports: Vec::new(),
                });
                let blocks: Vec<BlockUse> = entry
                    .children
                    .iter()
                    .map(|child| BlockUse {
                        tile: soft.clone(),
                        inputs: nodes[child].inputs.clone(),
                        outputs: nodes[child].outputs.clone(),
                        delay: longest[child],
                    })
                    .collect();
                let areas: Vec<usize> = entry.children.iter().map(|child| estimate[child]).collect();
                entry.children.iter().copied().zip(plan(&netlist, &blocks, &areas, &frames[&node])).collect()
            })
            .collect();
        frames.extend(planned.into_iter().flatten());
        depth = depth.iter().flat_map(|node| nodes[node].children.iter().copied()).collect();
    }

    if std::env::var_os("HIER_TRACE").is_some() {
        eprintln!("planned in {:?}", planning.elapsed());
    }

    // Kinds: nodes whose canonical form, frame included, is equal share one
    // layout.
    let mut kind_of: HashMap<usize, usize> = HashMap::new();
    let mut kinds: Vec<(usize, usize)> = Vec::new(); // (representative, height)
    let mut keys: HashMap<Vec<String>, usize> = HashMap::new();
    let mut height: HashMap<usize, usize> = HashMap::new();
    for &node in order.iter().filter(|&&node| node != 0) {
        let key = canonical(netlist, &nodes, node, &kind_of, &frames[&node]);
        let h = nodes[&node].children.iter().map(|child| height[child] + 1).max().unwrap_or(0);
        height.insert(node, h);
        let next = kinds.len();
        let kind = *keys.entry(key).or_insert(next);
        if kind == next {
            kinds.push((node, h));
        }
        kind_of.insert(node, kind);
    }

    // Generate kinds deepest first, a height at a time in parallel.
    let started = std::time::Instant::now();
    let mut built: Vec<Option<(Arc<Tile>, f64)>> = vec![None; kinds.len()];
    let tallest = kinds.iter().map(|&(_, h)| h).max();
    for level in 0..=tallest.unwrap_or(0) {
        if tallest.is_none() {
            break;
        }
        let here: Vec<usize> = (0..kinds.len()).filter(|&kind| kinds[kind].1 == level).collect();
        let results: Vec<Result<(Tile, f64), ConstructError>> = here
            .par_iter()
            .map(|&kind| {
                let node = kinds[kind].0;
                let (netlist, blocks, frame) = renamed(netlist, &nodes, node, &kind_of, &built, &frames[&node]);
                let generated = generate(&netlist, &blocks, &frame, true, Method::Constructive)?;
                Ok(generated.block.expect("a block was asked for"))
            })
            .collect();
        if std::env::var_os("HIER_TRACE").is_some() {
            eprintln!("height {level}: {} kinds in {:?}", here.len(), started.elapsed());
        }
        for (&kind, result) in here.iter().zip(results) {
            let (tile, delay) = result?;
            if std::env::var_os("HIER_TRACE").is_some() {
                let (lx, hx, lz, hz) = super::tile::extent(&tile);
                eprintln!(
                    "kind {kind} (node {}, height {level}): {} cells, {}x{}, {} inputs {} outputs, delay {delay:.1}",
                    kinds[kind].0,
                    tile.cells.len(),
                    hx - lx + 1,
                    hz - lz + 1,
                    tile.stubs.len(),
                    tile.exits.len()
                );
            }
            built[kind] = Some((Arc::new(tile), delay));
        }
    }

    // The top, under its own names.
    let top = &nodes[&0];
    let blocks: Vec<BlockUse> = top
        .children
        .iter()
        .map(|child| {
            let (tile, delay) = built[kind_of[child]].clone().expect("children are built first");
            BlockUse {
                tile,
                inputs: nodes[child].inputs.clone(),
                outputs: nodes[child].outputs.clone(),
                delay,
            }
        })
        .collect();
    generate(&top_netlist(netlist, top), &blocks, &frames[&0], false, Method::Constructive)
        .map(|generated| generated.construction)
}

/// The top's own gates, with the whole netlist's inputs and outputs.
fn top_netlist(netlist: &Netlist, top: &Node) -> Netlist {
    Netlist {
        inputs: netlist.inputs.clone(),
        outputs: netlist.outputs.clone(),
        gates: top.gates.iter().map(|&gate| netlist.gates[gate].clone()).collect(),
    }
}

/// A node's own gates between its ports, under their own names.
fn own_netlist(netlist: &Netlist, node: &Node) -> Netlist {
    Netlist {
        inputs: node.inputs.clone(),
        outputs: node.outputs.clone(),
        gates: node.gates.iter().map(|&gate| netlist.gates[gate].clone()).collect(),
    }
}

fn post_order(nodes: &HashMap<usize, Node>, node: usize, order: &mut Vec<usize>) {
    for &child in &nodes[&node].children {
        post_order(nodes, child, order);
    }
    order.push(node);
}

/// The signals each of a node's items reads and drives, in item order: its
/// gates by index, then its children's ports.
fn items<'a>(netlist: &'a Netlist, nodes: &'a HashMap<usize, Node>, node: usize) -> Vec<(Vec<&'a str>, Vec<&'a str>)> {
    let entry = &nodes[&node];
    let gates = entry.gates.iter().map(|&gate| {
        let gate = &netlist.gates[gate];
        (gate.inputs.iter().map(String::as_str).collect(), vec![gate.output.as_str()])
    });
    let children = entry.children.iter().map(|child| {
        let child = &nodes[child];
        (child.inputs.iter().map(String::as_str).collect(), child.outputs.iter().map(String::as_str).collect())
    });
    gates.chain(children).collect()
}

/// A node's ports, in order of first appearance across its items.
fn ports(
    netlist: &Netlist,
    nodes: &HashMap<usize, Node>,
    node: usize,
    inside: &[bool],
    produced_by: &HashMap<&str, usize>,
    readers: &HashMap<&str, Vec<usize>>,
) -> (Vec<String>, Vec<String>) {
    let internal = |signal: &str| produced_by.get(signal).is_some_and(|&gate| inside[gate]);
    let mut inputs: Vec<String> = Vec::new();
    let mut outputs: Vec<String> = Vec::new();
    for (reads, drives) in items(netlist, nodes, node) {
        for signal in reads {
            if !internal(signal) && !inputs.iter().any(|seen| seen == signal) {
                inputs.push(signal.to_string());
            }
        }
        for signal in drives {
            let read_outside = readers.get(signal).is_some_and(|readers| readers.iter().any(|&gate| !inside[gate]));
            let exported = read_outside || netlist.outputs.iter().any(|output| output == signal);
            if exported && !outputs.iter().any(|seen| seen == signal) {
                outputs.push(signal.to_string());
            }
        }
    }
    (inputs, outputs)
}

/// A node's structure with its signals renamed by first appearance, so two
/// instances of one layout give the same key.
fn canonical(
    netlist: &Netlist,
    nodes: &HashMap<usize, Node>,
    node: usize,
    kind_of: &HashMap<usize, usize>,
    frame: &Frame,
) -> Vec<String> {
    let mut names: HashMap<String, usize> = HashMap::new();
    let mut name = |signal: &str| {
        let next = names.len();
        *names.entry(signal.to_string()).or_insert(next)
    };
    let entry = &nodes[&node];
    let mut key = Vec::new();
    for signal in &entry.inputs {
        name(signal);
    }
    for &gate in &entry.gates {
        let gate = &netlist.gates[gate];
        let inputs: Vec<usize> = gate.inputs.iter().map(|input| name(input)).collect();
        key.push(format!("{:?} {inputs:?} {}", gate.kind, name(&gate.output)));
    }
    for child in &entry.children {
        let child_node = &nodes[child];
        let inputs: Vec<usize> = child_node.inputs.iter().map(|input| name(input)).collect();
        let outputs: Vec<usize> = child_node.outputs.iter().map(|output| name(output)).collect();
        key.push(format!("block {} {inputs:?} {outputs:?}", kind_of[child]));
    }
    let outputs: Vec<usize> = entry.outputs.iter().map(|output| name(output)).collect();
    key.push(format!("out {outputs:?}"));
    let edges: Vec<(usize, _)> =
        frame.inputs.iter().chain(&frame.outputs).map(|(port, side)| (name(port), side)).collect();
    let arrivals: Vec<(usize, u32)> = frame.arrivals.iter().map(|(port, ticks)| (name(port), *ticks)).collect();
    let toward: Vec<(usize, (i8, i8))> = frame.toward.iter().map(|(port, at)| (name(port), *at)).collect();
    key.push(format!("frame {edges:?} {:?} {arrivals:?} {toward:?}", frame.shape));
    key
}

/// A node as a standalone netlist, its children's blocks and its frame,
/// under the canonical names `s0, s1, ...`, so every instance of its
/// kind generates the same layout.
fn renamed(
    netlist: &Netlist,
    nodes: &HashMap<usize, Node>,
    node: usize,
    kind_of: &HashMap<usize, usize>,
    built: &[Option<(Arc<Tile>, f64)>],
    frame: &Frame,
) -> (Netlist, Vec<BlockUse>, Frame) {
    let mut names: HashMap<String, String> = HashMap::new();
    let mut name = |signal: &str| {
        let next = format!("s{}", names.len());
        names.entry(signal.to_string()).or_insert(next).clone()
    };
    let entry = &nodes[&node];
    let inputs: Vec<String> = entry.inputs.iter().map(|signal| name(signal)).collect();
    let gates: Vec<Gate> = entry
        .gates
        .iter()
        .enumerate()
        .map(|(rank, &gate)| {
            let gate = &netlist.gates[gate];
            Gate {
                name: format!("g{rank}"),
                inputs: gate.inputs.iter().map(|input| name(input)).collect(),
                output: name(&gate.output),
                kind: gate.kind,
            }
        })
        .collect();
    let blocks: Vec<BlockUse> = entry
        .children
        .iter()
        .map(|child| {
            let child_node = &nodes[child];
            let (tile, delay) = built[kind_of[child]].clone().expect("children are built first");
            BlockUse {
                tile,
                inputs: child_node.inputs.iter().map(|signal| name(signal)).collect(),
                outputs: child_node.outputs.iter().map(|signal| name(signal)).collect(),
                delay,
            }
        })
        .collect();
    let outputs: Vec<String> = entry.outputs.iter().map(|signal| name(signal)).collect();
    let frame = Frame {
        inputs: frame.inputs.iter().map(|(port, side)| (name(port), *side)).collect(),
        outputs: frame.outputs.iter().map(|(port, side)| (name(port), *side)).collect(),
        shape: frame.shape,
        arrivals: frame.arrivals.iter().map(|(port, ticks)| (name(port), *ticks)).collect(),
        toward: frame.toward.iter().map(|(port, at)| (name(port), *at)).collect(),
    };
    (
        Netlist {
            inputs,
            outputs,
            gates,
        },
        blocks,
        frame,
    )
}

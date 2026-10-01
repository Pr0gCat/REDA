use reda::compile::{
    fragment_synth::torch_row::compile_player_torch_row, primitive_graph, topology::Primitive,
    CompiledCircuit, Gate, Netlist,
};
use reda::redstone::simulator::{position::Position, Simulator};
use reda::timing::{measure_transition, summarize_worst_case, watch_all_nets};
use std::collections::BTreeSet;

fn source() -> Netlist {
    Netlist {
        inputs: vec!["left".into(), "right".into()],
        outputs: vec![
            "implemented:literal_a".into(),
            "implemented::literal_b".into(),
            "sum".into(),
        ],
        gates: vec![
            Gate::nor("removed_internal", &["left", "right"]),
            Gate::nor("implemented:literal_a", &["left"]),
            Gate::nor("implemented::literal_b", &["right"]),
            Gate::merge("sum", &["implemented:literal_a", "implemented::literal_b"]),
        ],
    }
}

#[test]
fn rewritten_compiled_views_measure_actual_nets_without_source_name_collisions() {
    let source = source();
    let rewrite = compile_player_torch_row(&source, 1_000_000).unwrap();
    let compiled = CompiledCircuit::from_certified_rewrite(rewrite);
    let implementation = compiled.implementation_netlist(&source);
    assert_eq!(compiled.gate_facings.len(), implementation.gates.len());
    assert!(!compiled
        .gate_output_positions
        .contains_key("removed_internal"));
    let watched = watch_all_nets(&compiled);
    let labels: BTreeSet<_> = watched.iter().map(|(_, label)| label).collect();
    assert_eq!(labels.len(), watched.len());
    assert!(watched
        .iter()
        .any(|(_, name)| name == "implemented:literal_a"));
    assert!(watched
        .iter()
        .any(|(_, name)| name == &compiled.implementation_signal_label("literal_a")));
    assert_ne!(
        compiled.implementation_signal_label("literal_a"),
        "implemented:literal_a"
    );
    let mut sim = Simulator::new(compiled.world.clone());
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(watched);
    let mut transitions = Vec::new();
    for from in 0..4 {
        for to in 0..4 {
            for address in [from, to] {
                transitions.push(
                    measure_transition(&mut sim, 2000, |sim| {
                        for (bit, name) in source.inputs.iter().enumerate() {
                            let (x, y, z) = compiled.input_positions[name];
                            let mut lever = sim.world().get(x, y, z).clone();
                            lever.lit = address & (1 << bit) != 0;
                            sim.world_mut().set(x, y, z, lever);
                        }
                    })
                    .unwrap(),
                );
                let expected = [address & 1 == 0, address & 2 == 0, address != 3];
                for (bit, &(x, y, z)) in compiled
                    .physical_rewrite()
                    .unwrap()
                    .source_output_positions()
                    .iter()
                    .enumerate()
                {
                    assert_eq!(sim.world().get(x, y, z).lit, expected[bit]);
                }
            }
        }
    }
    let summary = summarize_worst_case(&source, &compiled, &source.outputs, &transitions);
    assert_eq!(
        summary.logic_depth,
        reda::timing::logic_depth(implementation)
    );
    assert!(summary
        .critical_path
        .iter()
        .all(|signal| implementation.inputs.contains(signal)
            || implementation
                .gates
                .iter()
                .any(|gate| &gate.output == signal)));
    assert!(summary.critical_path_repeater_count.is_none());
}

#[test]
fn certified_flat_graph_keeps_selected_comparators_and_merge_contributors() {
    let source = source();
    let rewrite = compile_player_torch_row(&source, 1_000_000).unwrap();
    let graph = primitive_graph::from_certified_rewrite(&rewrite);
    let candidate = rewrite.implemented_candidate();
    let primitives: Vec<_> = candidate
        .instances
        .instances
        .iter()
        .flat_map(|instance| &instance.expanded.topology.primitives)
        .collect();
    assert_eq!(
        graph.nodes.len(),
        primitives.len() + source.inputs.len() + source.outputs.len()
    );
    assert_eq!(
        graph
            .nodes
            .iter()
            .filter(|node| node.primitive == Primitive::Comparator)
            .count(),
        2
    );
    let sum = rewrite
        .implemented()
        .gates
        .iter()
        .position(|gate| gate.output == "shared_sum")
        .unwrap();
    assert!(graph.gate_nodes[sum].is_empty());
    assert_eq!(graph.output_nodes[sum].len(), 2);
    assert!(graph.output_nodes[sum]
        .iter()
        .all(|&node| graph.nodes[node].primitive == Primitive::Repeater));
    assert!(graph
        .edges
        .iter()
        .all(|edge| edge.from < graph.nodes.len() && edge.to < graph.nodes.len()));
}

#[test]
fn primary_output_aliases_keep_distinct_terminals_and_one_honest_observation_label() {
    let source = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs: vec!["a".into(), "a".into(), "sum".into()],
        gates: vec![Gate::merge("sum", &["a", "b"])],
    };
    let rewrite = compile_player_torch_row(&source, 1_000_000).unwrap();
    assert_ne!(
        rewrite.source_output_positions()[0],
        rewrite.source_output_positions()[1]
    );
    let drivers = rewrite.source_output_driver_positions().to_vec();
    let compiled = CompiledCircuit::from_certified_rewrite(rewrite);
    let watched = watch_all_nets(&compiled);
    let a: Vec<_> = watched.iter().filter(|(_, name)| name == "a").collect();
    assert_eq!(a.len(), 1);
    assert_eq!(
        a[0].0,
        Position::new(drivers[0].0, drivers[0].1, drivers[0].2)
    );
    assert!(watched.iter().any(
        |(at, name)| name == &compiled.implementation_signal_label("a")
            && *at
                == Position::new(
                    compiled.input_positions["a"].0,
                    compiled.input_positions["a"].1,
                    compiled.input_positions["a"].2
                )
    ));
    assert_eq!(
        compiled
            .physical_rewrite()
            .unwrap()
            .source_output_positions()
            .len(),
        3
    );
}

#[test]
fn compiled_and_flat_views_follow_proved_permuted_port_bindings() {
    use reda::compile::fragment_synth::rewrite::realise_physical_rewrite;
    use reda::compile::primitive_graph::Provenance;
    use reda::compile::topology::{GateKind, Library, LibraryEntry, Template, TemplateNode};
    use std::collections::BTreeMap;
    let initial_source = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs: vec!["sum".into(), "a".into(), "b".into()],
        gates: vec![Gate::merge("sum", &["a", "b"])],
    };
    let initial = compile_player_torch_row(&initial_source, 1_000_000).unwrap();
    let source = Netlist {
        inputs: vec!["rhs".into(), "lhs".into()],
        outputs: vec!["right".into(), "sum".into(), "left".into()],
        gates: vec![
            Gate {
                name: "right".into(),
                output: "right".into(),
                kind: GateKind::Buf,
                inputs: vec!["rhs".into()],
            },
            Gate {
                name: "left".into(),
                output: "left".into(),
                kind: GateKind::Buf,
                inputs: vec!["lhs".into()],
            },
            Gate::merge("sum", &["right", "left"]),
        ],
    };
    let library = Library::new(BTreeMap::from([(
        GateKind::Buf,
        vec![LibraryEntry {
            name: "matrix-repeater-buffer",
            template: Template {
                nodes: vec![(TemplateNode::Torch, Primitive::Repeater)],
                internal_edges: Vec::new(),
                inputs: vec![TemplateNode::Torch],
                output: Some(TemplateNode::Torch),
                embedding_hints: Vec::new(),
            },
        }],
    )]));
    let rewrite = realise_physical_rewrite(
        &source,
        initial.implemented(),
        initial.implemented_candidate(),
        &library,
        &["b".into(), "a".into()],
        &[
            "positive_b".into(),
            "shared_sum".into(),
            "positive_a".into(),
        ],
        1_000_000,
    )
    .unwrap();
    assert_eq!(
        rewrite.source_input_positions()[0],
        initial.source_input_positions()[1]
    );
    let graph = primitive_graph::from_certified_rewrite(&rewrite);
    let inputs: Vec<_> = graph
        .nodes
        .iter()
        .filter_map(|node| match &node.provenance {
            Provenance::PrimaryInput { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(inputs, ["lhs", "rhs"]);
    let lamps: BTreeSet<_> = graph
        .nodes
        .iter()
        .filter_map(|node| match &node.provenance {
            Provenance::PrimaryOutput { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(lamps, BTreeSet::from(["left", "right", "sum"]));
    for (source_output, implementation_output) in [
        ("right", "positive_b"),
        ("sum", "shared_sum"),
        ("left", "positive_a"),
    ] {
        let lamp = graph
            .nodes
            .iter()
            .position(|node| {
                matches!(&node.provenance,
            Provenance::PrimaryOutput { name } if name == source_output)
            })
            .unwrap();
        let gate = initial
            .implemented()
            .gates
            .iter()
            .position(|gate| gate.output == implementation_output)
            .unwrap();
        let drivers: BTreeSet<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.to == lamp)
            .map(|edge| edge.from)
            .collect();
        assert_eq!(drivers, graph.output_nodes[gate].iter().copied().collect());
    }

    let compiled = CompiledCircuit::from_certified_rewrite(rewrite);
    let watched = watch_all_nets(&compiled);
    let actual_a = watched
        .iter()
        .find(|(_, label)| label == &compiled.implementation_signal_label("a"))
        .unwrap();
    let (x, y, z) = compiled.input_positions["lhs"];
    assert_eq!(actual_a.0, Position::new(x, y, z));
    let mut sim = Simulator::new(compiled.world.clone());
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(watched);
    let mut transitions = Vec::new();
    for address in [0, 1, 2, 3, 0] {
        transitions.push(
            measure_transition(&mut sim, 2000, |sim| {
                for (bit, name) in source.inputs.iter().enumerate() {
                    let (x, y, z) = compiled.input_positions[name];
                    let mut lever = sim.world().get(x, y, z).clone();
                    lever.lit = address & (1 << bit) != 0;
                    sim.world_mut().set(x, y, z, lever);
                }
            })
            .unwrap(),
        );
        for (name, expected) in [
            ("right", address & 1 != 0),
            ("left", address & 2 != 0),
            ("sum", address != 0),
        ] {
            let (x, y, z) = compiled.output_positions[name];
            assert_eq!(sim.world().get(x, y, z).lit, expected);
        }
    }
    let summary = summarize_worst_case(&source, &compiled, &source.outputs, &transitions);
    let ordinal = source
        .outputs
        .iter()
        .position(|name| name == &summary.critical_output)
        .unwrap();
    assert_eq!(
        summary.critical_path.last().unwrap(),
        &compiled
            .physical_rewrite()
            .unwrap()
            .rewrite_certificate()
            .rewritten_outputs_in_source_order()[ordinal]
    );
}

#[test]
fn flat_projection_covers_qualified_generation_techniques_and_constants() {
    for words in [
        [7, 4, 2, 0],
        [6, 4, 2, 1],
        [6, 5, 2, 0],
        [7, 5, 3, 0],
        [0, 3, 5, 7],
        [1, 0, 0, 0],
        [0, 0, 0, 1],
        [0, 7, 0, 0],
        [0, 0, 7, 0],
        [3, 2, 1, 0],
        [1, 2, 1, 2],
        [5, 6, 1, 2],
        [1, 6, 3, 5],
        [2, 7, 7, 1],
        [0, 0, 0, 0],
        [7, 7, 7, 7],
    ] {
        let mut source = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: Vec::new(),
            gates: vec![Gate::nor("not_a", &["a"]), Gate::nor("not_b", &["b"])],
        };
        for address in 0..4 {
            source.gates.push(Gate::nor(
                format!("row{address}"),
                &[
                    if address & 1 == 0 { "a" } else { "not_a" },
                    if address & 2 == 0 { "b" } else { "not_b" },
                ],
            ));
        }
        for bit in 0..3 {
            let rows: Vec<_> = (0..4)
                .filter(|&address| words[address] & (1 << bit) != 0)
                .map(|address| format!("row{address}"))
                .collect();
            let output = format!("y{bit}");
            match rows.len() {
                0 => {
                    source.gates.push(Gate::nor(format!("high{bit}"), &[]));
                    source
                        .gates
                        .push(Gate::nor(&output, &[&format!("high{bit}")]));
                }
                4 => source.gates.push(Gate::nor(&output, &[])),
                _ => source.gates.push(Gate::merge(
                    &output,
                    &rows.iter().map(String::as_str).collect::<Vec<_>>(),
                )),
            }
            source.outputs.push(output);
        }
        let rewrite = compile_player_torch_row(&source, 1_000_000).unwrap();
        let graph = primitive_graph::from_certified_rewrite(&rewrite);
        let manifest = rewrite.implemented_candidate();
        assert_eq!(graph.gate_nodes.len(), rewrite.implemented().gates.len());
        for primitive in [Primitive::Comparator, Primitive::Repeater, Primitive::Torch] {
            let count = manifest
                .instances
                .instances
                .iter()
                .flat_map(|instance| &instance.expanded.topology.primitives)
                .filter(|spec| spec.primitive == primitive)
                .count();
            assert_eq!(
                graph
                    .nodes
                    .iter()
                    .filter(|node| node.primitive == primitive)
                    .count(),
                count,
                "{words:?}"
            );
        }
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|node| node.primitive == Primitive::Lamp)
                .count(),
            3
        );
        assert!(graph.output_nodes.iter().all(|nodes| !nodes.is_empty()));
        assert!(graph
            .edges
            .iter()
            .all(|edge| edge.from < graph.nodes.len() && edge.to < graph.nodes.len()));
    }
}

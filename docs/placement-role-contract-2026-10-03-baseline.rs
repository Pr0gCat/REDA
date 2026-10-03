    /// Having physical orientations does not make a primitive compatible with
    /// the fixed NOR, branch-diode, or primary-input geometry used by placement.
    #[test]
    fn baseline_accepts_incompatible_body_roles() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into(), "spy".into()],
            gates: vec![
                nor("na", &["a"]), nor("nb", &["b"]),
                merge("m", &["na", "nb"]),
                nor("out", &["m"]), nor("spy", &["nb"]),
            ],
        };
        let start = vec![Anchor { x: 0, y: 1, z: 0 };
            netlist.gates.len() + netlist.inputs.len()];
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        build(&netlist, &graph, &start, &PortPlacements::default()).expect("valid roles build");
        assert_eq!(physical::variants(Primitive::Comparator).len(), 4);
        for (role, context) in [("Torch", "gate `na`"),
            ("Repeater", "gate `m`'s branch 1"), ("Lever", "declared input `a`")] {
            let mut graph = expand(&netlist, &Library::default_library()).expect("expands");
            let node = graph.nodes.iter_mut().find(|node| match &node.provenance {
                Provenance::Gate { gate: 0, .. } => role == "Torch",
                Provenance::Gate { gate: 2, role: TemplateNode::IsolatingRepeater(1) } =>
                    role == "Repeater",
                Provenance::PrimaryInput { name } => role == "Lever" && name == "a",
                _ => false,
            }).expect("the tested role exists");
            node.primitive = Primitive::Comparator;
            let built = build(&netlist, &graph, &start, &PortPlacements::default())
                .expect("baseline accepts the incompatible physical body");
            if role == "Repeater" {
                let branch = built.bodies.iter().find(|body| matches!(body.what,
                    BodyKind::Primitive { kind: Primitive::Comparator, .. }))
                    .expect("the incompatible branch was built");
                assert!(std::panic::catch_unwind(||
                    attach_offset(Attach::Port(PortKind::RepeaterRear), branch)).is_err(),
                    "baseline's later physical port lookup panics");
            }
            println!("BASELINE_ACCEPTED {context} as Comparator instead of {role}");
        }
    }

//! Module hierarchy kept from the front end to placement: a design is a
//! tree of module instances; each module owns gates and child instances.
//! Certification always sees the flattened netlist (`flatten`); only
//! placement and routing see blocks.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::topology::GateKind;
use crate::compile::{Gate, Netlist};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HierarchicalNetlist {
    pub top: String,
    pub modules: BTreeMap<String, Module>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub gates: Vec<Gate>,
    pub instances: Vec<ModuleInstance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleInstance {
    pub name: String,
    pub module: String,
    /// Child port name -> what the parent connects to it.
    pub ports: BTreeMap<String, PortBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortBinding {
    Signal(String),
    Zero,
    One,
}

/// Where a flattened gate came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatePath {
    pub path: Vec<String>,
    pub module: String,
    pub gate: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HierarchyError {
    #[error("module instantiation cycle through {modules:?}")]
    Cycle { modules: Vec<String> },
    #[error("instance `{instance}` names unknown module `{module}`")]
    UnknownModule { instance: String, module: String },
    #[error("instance `{instance}` binds `{port}`, which its module does not declare")]
    PortMismatch { instance: String, port: String },
    #[error("instance `{instance}` leaves port `{port}` unconnected")]
    UnconnectedPort { instance: String, port: String },
    #[error("top module `{0}` is not in the design")]
    UnknownTop(String),
    #[error("constant on `{instance}.{port}` cannot be folded into a {kind:?} gate")]
    UnfoldableConstant { instance: String, port: String, kind: GateKind },
    #[error(
        "instance `{instance}` still binds `{port}` to a constant; call specialise_constants \
         before flatten"
    )]
    UnspecialisedConstant { instance: String, port: String },
}

/// `a.b.c` with Yosys's derived names made identifier-safe. `$` (Yosys's
/// marker for a generated name, as in `$paramod\...`) becomes a double
/// underscore so it stays visually distinct from an ordinary separator;
/// every other non-identifier character becomes a single underscore.
pub fn instance_prefix(path: &[String]) -> String {
    path.iter()
        .map(|segment| {
            segment
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        c.to_string()
                    } else if c == '$' {
                        "__".to_string()
                    } else {
                        "_".to_string()
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(".")
}

impl HierarchicalNetlist {
    pub fn validate(&self) -> Result<(), HierarchyError> {
        if !self.modules.contains_key(&self.top) {
            return Err(HierarchyError::UnknownTop(self.top.clone()));
        }
        for module in self.modules.values() {
            for instance in &module.instances {
                let child = self.modules.get(&instance.module).ok_or_else(|| {
                    HierarchyError::UnknownModule {
                        instance: instance.name.clone(),
                        module: instance.module.clone(),
                    }
                })?;
                let declared: BTreeSet<&str> = child
                    .inputs
                    .iter()
                    .chain(child.outputs.iter())
                    .map(String::as_str)
                    .collect();
                for port in instance.ports.keys() {
                    if !declared.contains(port.as_str()) {
                        return Err(HierarchyError::PortMismatch {
                            instance: instance.name.clone(),
                            port: port.clone(),
                        });
                    }
                }
                for port in child.inputs.iter().chain(child.outputs.iter()) {
                    if !instance.ports.contains_key(port) {
                        return Err(HierarchyError::UnconnectedPort {
                            instance: instance.name.clone(),
                            port: port.clone(),
                        });
                    }
                }
            }
        }
        self.module_order().map(|_| ())
    }

    /// Children before parents; a cycle is reported with every module on it.
    pub fn module_order(&self) -> Result<Vec<String>, HierarchyError> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Open,
            Done,
        }
        let mut marks: BTreeMap<&str, Mark> = BTreeMap::new();
        let mut order = Vec::new();
        fn visit<'a>(
            design: &'a HierarchicalNetlist,
            name: &'a str,
            marks: &mut BTreeMap<&'a str, Mark>,
            order: &mut Vec<String>,
            stack: &mut Vec<String>,
        ) -> Result<(), HierarchyError> {
            match marks.get(name) {
                Some(Mark::Done) => return Ok(()),
                Some(Mark::Open) => {
                    let mut modules = stack.clone();
                    modules.push(name.to_string());
                    return Err(HierarchyError::Cycle { modules });
                }
                None => {}
            }
            marks.insert(name, Mark::Open);
            stack.push(name.to_string());
            let module = design.modules.get(name).ok_or_else(|| HierarchyError::UnknownModule {
                instance: String::new(),
                module: name.to_string(),
            })?;
            for instance in &module.instances {
                visit(design, &instance.module, marks, order, stack)?;
            }
            stack.pop();
            marks.insert(name, Mark::Done);
            order.push(name.to_string());
            Ok(())
        }
        for name in self.modules.keys() {
            visit(self, name, &mut marks, &mut order, &mut Vec::new())?;
        }
        Ok(order)
    }

    /// Every instance port tied to a constant becomes a specialised module
    /// `<name>@<port>=<0|1>[,<port>=<bit>]`, shared by equal instances.
    /// Only NOR/OR inputs tied to zero fold (the neutral element); anything
    /// else is refused, matching the Yosys reader's rule.
    pub fn specialise_constants(&self) -> Result<HierarchicalNetlist, HierarchyError> {
        let mut out = self.clone();
        let mut pending: Vec<String> = out.modules.keys().cloned().collect();
        while let Some(parent_name) = pending.pop() {
            let parent = out.modules[&parent_name].clone();
            let mut rewritten = parent.clone();
            for (index, instance) in parent.instances.iter().enumerate() {
                let constants: BTreeMap<&str, bool> = instance
                    .ports
                    .iter()
                    .filter_map(|(port, binding)| match binding {
                        PortBinding::Signal(_) => None,
                        PortBinding::Zero => Some((port.as_str(), false)),
                        PortBinding::One => Some((port.as_str(), true)),
                    })
                    .collect();
                if constants.is_empty() {
                    continue;
                }
                let suffix = constants
                    .iter()
                    .map(|(port, bit)| format!("{port}={}", u8::from(*bit)))
                    .collect::<Vec<_>>()
                    .join(",");
                let clone_name = format!("{}@{suffix}", instance.module);
                if !out.modules.contains_key(&clone_name) {
                    let child = out.modules[&instance.module].clone();
                    let clone = specialise_module(&child, &constants, &instance.name)?;
                    out.modules.insert(clone_name.clone(), clone);
                    pending.push(clone_name.clone());
                }
                let target = &mut rewritten.instances[index];
                target.module = clone_name;
                target.ports.retain(|_, binding| matches!(binding, PortBinding::Signal(_)));
            }
            out.modules.insert(parent_name, rewritten);
        }
        Ok(out)
    }

    /// The module's own gates as a netlist whose inputs also include every
    /// child instance's outputs and whose outputs also include every child
    /// instance's inputs, so lowering keeps those signals as boundaries.
    pub fn boundary_netlist(&self, module: &str) -> Netlist {
        let module = &self.modules[module];
        let mut inputs = module.inputs.clone();
        let mut outputs = module.outputs.clone();
        for instance in &module.instances {
            let child = &self.modules[&instance.module];
            for port in &child.outputs {
                if let Some(PortBinding::Signal(signal)) = instance.ports.get(port) {
                    if !inputs.contains(signal) {
                        inputs.push(signal.clone());
                    }
                }
            }
            for port in &child.inputs {
                if let Some(PortBinding::Signal(signal)) = instance.ports.get(port) {
                    if !outputs.contains(signal) {
                        outputs.push(signal.clone());
                    }
                }
            }
        }
        Netlist { inputs, outputs, gates: module.gates.clone() }
    }

    /// The flat netlist certification uses, plus the origin of every gate.
    ///
    /// Precondition: `specialise_constants` must already have been run on
    /// this design. A `PortBinding::Zero`/`One` reaching this point means
    /// some instance's constant tie was never folded into a specialised
    /// module, which would otherwise silently rename the port into a signal
    /// nothing drives; `flatten` refuses that instead of guessing.
    pub fn flatten(&self) -> Result<(Netlist, Vec<GatePath>), HierarchyError> {
        self.validate()?;
        let top = &self.modules[&self.top];
        let mut gates = Vec::new();
        let mut paths = Vec::new();
        self.flatten_into(&self.top, &[], &BTreeMap::new(), &mut gates, &mut paths)?;
        Ok((
            Netlist { inputs: top.inputs.clone(), outputs: top.outputs.clone(), gates },
            paths,
        ))
    }

    fn flatten_into(
        &self,
        module_name: &str,
        path: &[String],
        aliases: &BTreeMap<String, String>,
        gates: &mut Vec<Gate>,
        paths: &mut Vec<GatePath>,
    ) -> Result<(), HierarchyError> {
        let module = &self.modules[module_name];
        let prefix = instance_prefix(path);
        let rename = |signal: &str| -> String {
            if let Some(alias) = aliases.get(signal) {
                return alias.clone();
            }
            if prefix.is_empty() { signal.to_string() } else { format!("{prefix}.{signal}") }
        };
        // A gate's own name is always path-prefixed, never alias-rewritten:
        // `aliases` is keyed by this module's *port* names, and a gate that
        // happens to be named after one of them is not that port.
        let path_prefixed = |signal: &str| -> String {
            if prefix.is_empty() { signal.to_string() } else { format!("{prefix}.{signal}") }
        };
        for (index, gate) in module.gates.iter().enumerate() {
            gates.push(Gate {
                name: path_prefixed(&gate.name),
                inputs: gate.inputs.iter().map(|s| rename(s)).collect(),
                output: rename(&gate.output),
                kind: gate.kind,
            });
            paths.push(GatePath { path: path.to_vec(), module: module_name.to_string(), gate: index });
        }
        for instance in &module.instances {
            let mut child_aliases = BTreeMap::new();
            for (port, binding) in &instance.ports {
                match binding {
                    PortBinding::Signal(signal) => {
                        child_aliases.insert(port.clone(), rename(signal));
                    }
                    PortBinding::Zero | PortBinding::One => {
                        return Err(HierarchyError::UnspecialisedConstant {
                            instance: instance.name.clone(),
                            port: port.clone(),
                        });
                    }
                }
            }
            let mut child_path = path.to_vec();
            child_path.push(instance.name.clone());
            self.flatten_into(&instance.module, &child_path, &child_aliases, gates, paths)?;
        }
        Ok(())
    }
}

/// Fold a set of constant-tied ports into `child`, following the same rule
/// `Context::build_cell` uses when a Yosys pin is a literal constant: a
/// `Nor`/`Or` input tied to zero (the neutral element for both) drops out of
/// the gate and shrinks its arity; anything else -- a one-tied `Nor`/`Or`
/// input, or any constant into a fixed-arity kind -- has no fold and is
/// refused by name.
fn specialise_module(
    child: &Module,
    constants: &BTreeMap<&str, bool>,
    instance: &str,
) -> Result<Module, HierarchyError> {
    let mut clone = child.clone();
    clone.inputs.retain(|port| !constants.contains_key(port.as_str()));
    for gate in &mut clone.gates {
        let mut inputs = Vec::with_capacity(gate.inputs.len());
        for input in &gate.inputs {
            match constants.get(input.as_str()) {
                None => inputs.push(input.clone()),
                Some(false) if matches!(gate.kind, GateKind::Nor(_) | GateKind::Or(_)) => {}
                Some(_) => {
                    return Err(HierarchyError::UnfoldableConstant {
                        instance: instance.to_string(),
                        port: input.clone(),
                        kind: gate.kind,
                    })
                }
            }
        }
        if inputs.is_empty() && matches!(gate.kind, GateKind::Nor(_) | GateKind::Or(_)) {
            // Every input folded away: the gate would become a hard-wired
            // constant output, which nothing in redstone can realise (see
            // `Context::build_cell` in `yosys_json.rs`, which refuses both
            // "hard-wired-1" and "hard-wired-0" for the same reason).
            let culprits: Vec<&str> = gate.inputs.iter().map(String::as_str).collect();
            return Err(HierarchyError::UnfoldableConstant {
                instance: instance.to_string(),
                port: culprits.join(","),
                kind: gate.kind,
            });
        }
        gate.kind = match gate.kind {
            GateKind::Nor(_) => GateKind::Nor(inputs.len()),
            GateKind::Or(_) => GateKind::Or(inputs.len()),
            other => other,
        };
        gate.inputs = inputs;
    }
    // The clone's own child instances may bind one of the ports that was
    // just tied off (e.g. this module passes its input straight through to
    // a grandchild's port). That binding must follow the same constant,
    // otherwise it survives as a `Signal` reference to a port the clone no
    // longer declares -- a wire nothing drives that neither `validate` nor
    // `flatten` would catch. The outer `specialise_constants` loop re-queues
    // this clone (`pending.push`), so if this rewrite turns any of its own
    // instances into a constant binding, that gets folded in turn.
    for instance in &mut clone.instances {
        for binding in instance.ports.values_mut() {
            if let PortBinding::Signal(signal) = binding {
                if let Some(&bit) = constants.get(signal.as_str()) {
                    *binding = if bit { PortBinding::One } else { PortBinding::Zero };
                }
            }
        }
    }
    alias_single_input_ors(&mut clone);
    Ok(clone)
}

/// `GateKind::Or(1)` is a bare wire in this project, not a gate (see
/// `Context::build_cell` in `yosys_json.rs`, which returns the single input
/// directly rather than building a gate). Folding a constant can leave an
/// `Or` at arity 1, so remove that gate and rename every reference to its
/// output -- other gates' inputs, the module's own outputs, and any
/// instance port bound to it -- to its one remaining input instead.
fn alias_single_input_ors(module: &mut Module) {
    loop {
        let Some(index) = module.gates.iter().position(|gate| matches!(gate.kind, GateKind::Or(1)))
        else {
            break;
        };
        let alias = module.gates.remove(index);
        let source = alias.inputs[0].clone();
        let target = alias.output;
        for gate in &mut module.gates {
            for input in &mut gate.inputs {
                if *input == target {
                    *input = source.clone();
                }
            }
        }
        for output in &mut module.outputs {
            if *output == target {
                *output = source.clone();
            }
        }
        for instance in &mut module.instances {
            for binding in instance.ports.values_mut() {
                if let PortBinding::Signal(signal) = binding {
                    if *signal == target {
                        *signal = source.clone();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::topology::GateKind;

    fn gate(name: &str, inputs: &[&str], output: &str, kind: GateKind) -> Gate {
        Gate {
            name: name.to_string(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            output: output.to_string(),
            kind,
        }
    }

    /// inv: y = NOT a.  top: two instances chained, u0.y -> u1.a.
    fn two_level() -> HierarchicalNetlist {
        let mut modules = BTreeMap::new();
        modules.insert(
            "inv".to_string(),
            Module {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![gate("g0", &["a"], "y", GateKind::Nor(1))],
                instances: vec![],
            },
        );
        modules.insert(
            "top".to_string(),
            Module {
                inputs: vec!["x".into()],
                outputs: vec!["z".into()],
                gates: vec![],
                instances: vec![
                    ModuleInstance {
                        name: "u0".into(),
                        module: "inv".into(),
                        ports: BTreeMap::from([
                            ("a".to_string(), PortBinding::Signal("x".into())),
                            ("y".to_string(), PortBinding::Signal("mid".into())),
                        ]),
                    },
                    ModuleInstance {
                        name: "u1".into(),
                        module: "inv".into(),
                        ports: BTreeMap::from([
                            ("a".to_string(), PortBinding::Signal("mid".into())),
                            ("y".to_string(), PortBinding::Signal("z".into())),
                        ]),
                    },
                ],
            },
        );
        HierarchicalNetlist { top: "top".into(), modules }
    }

    #[test]
    fn flattening_prefixes_instance_signals_and_aliases_ports() {
        let (flat, paths) = two_level().flatten().expect("flattens");
        assert_eq!(flat.inputs, vec!["x".to_string()]);
        assert_eq!(flat.outputs, vec!["z".to_string()]);
        assert_eq!(flat.gates.len(), 2);
        assert_eq!(flat.gates[0].inputs, vec!["x".to_string()]);
        assert_eq!(flat.gates[0].output, "mid".to_string());
        assert_eq!(flat.gates[1].inputs, vec!["mid".to_string()]);
        assert_eq!(flat.gates[1].output, "z".to_string());
        assert_eq!(flat.gates[0].name, "u0.g0");
        assert_eq!(paths[0], GatePath { path: vec!["u0".into()], module: "inv".into(), gate: 0 });
        assert_eq!(paths[1], GatePath { path: vec!["u1".into()], module: "inv".into(), gate: 0 });
    }

    #[test]
    fn a_single_module_flattens_to_itself() {
        let mut design = two_level();
        let inv = design.modules.remove("inv").unwrap();
        design.modules.clear();
        design.modules.insert("inv".into(), inv.clone());
        design.top = "inv".into();
        let (flat, paths) = design.flatten().expect("flattens");
        assert_eq!(flat, Netlist { inputs: inv.inputs, outputs: inv.outputs, gates: inv.gates });
        assert_eq!(paths, vec![GatePath { path: vec![], module: "inv".into(), gate: 0 }]);
    }

    #[test]
    fn a_module_cycle_is_refused_by_name() {
        let mut design = two_level();
        design.modules.get_mut("inv").unwrap().instances.push(ModuleInstance {
            name: "loop".into(),
            module: "top".into(),
            ports: BTreeMap::from([
                ("x".to_string(), PortBinding::Signal("a".into())),
                ("z".to_string(), PortBinding::Signal("unused".into())),
            ]),
        });
        match design.validate() {
            Err(HierarchyError::Cycle { modules }) => {
                assert!(modules.contains(&"inv".to_string()) && modules.contains(&"top".to_string()))
            }
            other => panic!("expected a cycle, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_module_and_a_missing_port_are_refused_by_name() {
        let mut design = two_level();
        design.modules.get_mut("top").unwrap().instances[0].module = "nope".into();
        assert!(matches!(
            design.validate(),
            Err(HierarchyError::UnknownModule { ref instance, ref module }) if instance == "u0" && module == "nope"
        ));
        let mut design = two_level();
        design.modules.get_mut("top").unwrap().instances[1].ports.remove("a");
        assert!(matches!(
            design.validate(),
            Err(HierarchyError::UnconnectedPort { ref instance, ref port }) if instance == "u1" && port == "a"
        ));
    }

    #[test]
    fn a_constant_port_specialises_the_module_and_shares_the_clone() {
        // and2: y = NOT(NOT a NOR NOT b) written as NOR gates; tie b = 1 in both instances.
        let mut modules = BTreeMap::new();
        modules.insert(
            "or2".to_string(),
            Module {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into()],
                gates: vec![
                    gate("g0", &["a", "b"], "n", GateKind::Nor(2)),
                    gate("g1", &["n"], "y", GateKind::Nor(1)),
                ],
                instances: vec![],
            },
        );
        let instance = |name: &str, out: &str| ModuleInstance {
            name: name.into(),
            module: "or2".into(),
            ports: BTreeMap::from([
                ("a".to_string(), PortBinding::Signal("x".into())),
                ("b".to_string(), PortBinding::Zero),
                ("y".to_string(), PortBinding::Signal(out.into())),
            ]),
        };
        modules.insert(
            "top".to_string(),
            Module {
                inputs: vec!["x".into()],
                outputs: vec!["p".into(), "q".into()],
                gates: vec![],
                instances: vec![instance("u0", "p"), instance("u1", "q")],
            },
        );
        let design = HierarchicalNetlist { top: "top".into(), modules };
        let specialised = design.specialise_constants().expect("specialises");
        assert_eq!(specialised.modules.len(), 3, "or2, or2@b=0 and top");
        let clone = &specialised.modules["or2@b=0"];
        assert_eq!(clone.inputs, vec!["a".to_string()]);
        // NOR(a, 0) folds to NOR(a).
        assert_eq!(clone.gates[0].inputs, vec!["a".to_string()]);
        assert_eq!(clone.gates[0].kind, GateKind::Nor(1));
        let top = &specialised.modules["top"];
        assert!(top.instances.iter().all(|i| i.module == "or2@b=0"));
        assert!(top.instances.iter().all(|i| !i.ports.contains_key("b")));
    }

    #[test]
    fn an_or_left_with_one_input_becomes_an_alias() {
        // g0 is an Or(2) already folded down to one input by constant
        // specialisation (as `specialise_module` would leave it): in this
        // project `Or(1)` is a bare wire, not a gate, so it must disappear
        // and every reference to its output must become its input instead.
        let mut module = Module {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![
                gate("g0", &["a"], "n", GateKind::Or(1)),
                gate("g1", &["n"], "y", GateKind::Nor(1)),
            ],
            instances: vec![],
        };
        alias_single_input_ors(&mut module);
        assert_eq!(module.gates.len(), 1, "the Or(1) alias gate is removed entirely");
        assert_eq!(module.gates[0].inputs, vec!["a".to_string()], "downstream gate now reads the alias's source");
        assert_eq!(module.gates[0].output, "y".to_string());
    }

    #[test]
    fn the_boundary_netlist_exposes_child_ports_as_pseudo_ports() {
        let mut design = two_level();
        // Give top a gate of its own that consumes u1's output.
        let top = design.modules.get_mut("top").unwrap();
        top.gates.push(gate("g0", &["z"], "w", GateKind::Nor(1)));
        top.outputs = vec!["w".into()];
        let boundary = design.boundary_netlist("top");
        assert_eq!(boundary.inputs, vec!["x".to_string(), "mid".to_string(), "z".to_string()]);
        assert_eq!(boundary.outputs, vec!["w".to_string(), "x".to_string(), "mid".to_string()]);
        assert_eq!(boundary.gates.len(), 1);
    }

    #[test]
    fn module_order_lists_children_before_parents() {
        assert_eq!(two_level().module_order().unwrap(), vec!["inv".to_string(), "top".to_string()]);
    }

    #[test]
    fn instance_prefixes_sanitise_paramod_names() {
        assert_eq!(instance_prefix(&["a".into(), "$paramod\\fa\\W=4".into()]), "a.__paramod_fa_W_4");
    }

    /// Regression for review finding "Critical 1": specialising a module
    /// must rewrite bindings inside its OWN child instances too, not just
    /// its own gates. `top` ties `mid`'s port `b` to zero; `mid` passes that
    /// same signal straight into its own child instance `uleaf`'s port `a`.
    /// The clone `mid@b=0` must carry `a: Zero` on `uleaf`, which the outer
    /// `specialise_constants` loop then folds again into `leaf@a=0`.
    #[test]
    fn specialising_rewrites_the_clones_own_child_instance_bindings() {
        let mut modules = BTreeMap::new();
        modules.insert(
            "leaf".to_string(),
            Module {
                inputs: vec!["a".into(), "c".into()],
                outputs: vec!["y".into()],
                gates: vec![gate("g0", &["a", "c"], "y", GateKind::Nor(2))],
                instances: vec![],
            },
        );
        modules.insert(
            "mid".to_string(),
            Module {
                inputs: vec!["b".into(), "c".into()],
                outputs: vec!["y".into()],
                gates: vec![],
                instances: vec![ModuleInstance {
                    name: "uleaf".into(),
                    module: "leaf".into(),
                    ports: BTreeMap::from([
                        ("a".to_string(), PortBinding::Signal("b".into())),
                        ("c".to_string(), PortBinding::Signal("c".into())),
                        ("y".to_string(), PortBinding::Signal("y".into())),
                    ]),
                }],
            },
        );
        modules.insert(
            "top".to_string(),
            Module {
                inputs: vec!["w".into()],
                outputs: vec!["z".into()],
                gates: vec![],
                instances: vec![ModuleInstance {
                    name: "umid".into(),
                    module: "mid".into(),
                    ports: BTreeMap::from([
                        ("b".to_string(), PortBinding::Zero),
                        ("c".to_string(), PortBinding::Signal("w".into())),
                        ("y".to_string(), PortBinding::Signal("z".into())),
                    ]),
                }],
            },
        );
        let design = HierarchicalNetlist { top: "top".into(), modules };
        let specialised = design.specialise_constants().expect("specialises");

        // The clone chain: mid@b=0 exists, and its own uleaf binding, having
        // inherited a=0 from the tied-off `b`, was itself specialised into
        // leaf@a=0. Without the fix, `uleaf` in `mid@b=0` keeps a stale
        // `Signal("b")` binding and this second-level clone never appears.
        let mid_clone = specialised.modules.get("mid@b=0").expect("mid@b=0 clone exists");
        assert_eq!(mid_clone.instances.len(), 1);
        assert_eq!(mid_clone.instances[0].module, "leaf@a=0");
        assert!(specialised.modules.contains_key("leaf@a=0"), "the re-queued clone was specialised in turn");

        // No module anywhere in the result still carries a constant
        // binding: every tie was folded into a specialised module.
        for module in specialised.modules.values() {
            for instance in &module.instances {
                for binding in instance.ports.values() {
                    assert!(
                        matches!(binding, PortBinding::Signal(_)),
                        "instance `{}` still carries a constant binding after specialisation",
                        instance.name
                    );
                }
            }
        }

        specialised.flatten().expect("the fully specialised design flattens");
    }

    /// Regression for review finding "Critical 2": folding away every input
    /// of a gate would leave a `Nor(0)`/`Or(0)`, which is a hard-wired
    /// constant output. Nothing in this project can realise that (see
    /// `Context::build_cell` in `yosys_json.rs`), so it must be refused by
    /// name instead of silently produced.
    #[test]
    fn folding_all_inputs_of_a_gate_is_refused_by_name() {
        let mut modules = BTreeMap::new();
        modules.insert(
            "nor2".to_string(),
            Module {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into()],
                gates: vec![gate("g0", &["a", "b"], "y", GateKind::Nor(2))],
                instances: vec![],
            },
        );
        modules.insert(
            "top".to_string(),
            Module {
                inputs: vec![],
                outputs: vec!["z".into()],
                gates: vec![],
                instances: vec![ModuleInstance {
                    name: "u0".into(),
                    module: "nor2".into(),
                    ports: BTreeMap::from([
                        ("a".to_string(), PortBinding::Zero),
                        ("b".to_string(), PortBinding::Zero),
                        ("y".to_string(), PortBinding::Signal("z".into())),
                    ]),
                }],
            },
        );
        let design = HierarchicalNetlist { top: "top".into(), modules };
        match design.specialise_constants() {
            Err(HierarchyError::UnfoldableConstant { instance, .. }) => {
                assert_eq!(instance, "u0");
            }
            other => panic!("expected an unfoldable constant, got {other:?}"),
        }
    }

    /// Regression for review finding "Important 3": `rename` inside
    /// `flatten_into` is keyed by *port* names, so a gate whose `name`
    /// happens to equal one of its module's port names must not be
    /// alias-rewritten -- only path-prefixed, like every other gate name.
    #[test]
    fn a_gate_named_after_a_port_is_not_aliased() {
        let mut modules = BTreeMap::new();
        modules.insert(
            "inv2".to_string(),
            Module {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                // The gate's own name collides with its module's input port name.
                gates: vec![gate("a", &["a"], "y", GateKind::Nor(1))],
                instances: vec![],
            },
        );
        modules.insert(
            "top".to_string(),
            Module {
                inputs: vec!["x".into()],
                outputs: vec!["z".into()],
                gates: vec![],
                instances: vec![ModuleInstance {
                    name: "u0".into(),
                    module: "inv2".into(),
                    ports: BTreeMap::from([
                        ("a".to_string(), PortBinding::Signal("x".into())),
                        ("y".to_string(), PortBinding::Signal("z".into())),
                    ]),
                }],
            },
        );
        let design = HierarchicalNetlist { top: "top".into(), modules };
        let (flat, _) = design.flatten().expect("flattens");
        assert_eq!(
            flat.gates[0].name, "u0.a",
            "the gate's own name must be path-prefixed, not aliased to the port binding"
        );
        assert_eq!(flat.gates[0].inputs, vec!["x".to_string()], "the gate's inputs still alias through the port binding");
    }

    /// Regression for review finding "Important 4": `flatten` must enforce
    /// its `specialise_constants`-first precondition rather than silently
    /// renaming a constant-tied port into a signal nothing drives.
    #[test]
    fn flatten_refuses_an_unspecialised_constant_binding() {
        let mut design = two_level();
        design.modules.get_mut("top").unwrap().instances[0]
            .ports
            .insert("a".to_string(), PortBinding::Zero);
        match design.flatten() {
            Err(HierarchyError::UnspecialisedConstant { instance, port }) => {
                assert_eq!(instance, "u0");
                assert_eq!(port, "a");
            }
            other => panic!("expected an unspecialised constant, got {other:?}"),
        }
    }
}

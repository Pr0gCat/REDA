# Module Floorplan Implementation Plan

> **Status: implementation and verification complete (2026-09-05).** Tasks
> 1-13 landed through `14470a1`; Task 14's fresh flat, hierarchical, Verilog,
> pinned-IO and release integration runs are recorded in the acceptance report
> and SDD ledger. Final review found no structural floorplan defect and its four
> reachable frontend findings were fixed with regression tests.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile a Verilog module once as an unpinned, certified block, stamp it at every instance, place blocks as opaque macros in the parent, wire the gaps with the existing channel plan, and certify the flat union exactly as today.

**Architecture:** A `HierarchicalNetlist` keeps modules and instances; each module is lowered on its own and flattened for certification. Every distinct module is compiled by the unchanged unpinned seed into a `CompiledBlock` whose ports are the seed's automatic lever and lamp cells. The parent seed gets a `blocks` list on its `InstanceGraph`: blocks join the DAG analysis, placement (bounding-box envelope, fixed east facing, certified delay), channel plan and routing as macros with sources at lamp cells and sinks at lever cells. After routing, the parent's candidate is rebuilt flat: every block candidate is renumbered into the flattened netlist's instance space and translated to its placed origin, and every boundary is spliced into one route tree with a repeater at the join. The flat union goes through `validate_shape`, emission, verification and certification unchanged.

**Tech Stack:** Rust 2021, `serde`/`serde_json` (already dependencies), `std::thread::scope` for parallel block compiles (no new crates), Yosys via the existing `synth.py`.

**Spec:** `docs/superpowers/specs/2026-09-04-module-floorplan.md`

## Global Constraints

- Do not modify the A* router policy or raise `RouterLimits` (262144 expansions / 262144 queue entries).
- Do not change `SynthesisInput`, `compile_fragment_synth`, `ExpandedPhysicalCandidate`'s existing fields, or `PhysicalEndpointId`.
- The legacy front doors (`compile`, `compile_legacy`, `compile_planned`, `compile_grown`) stay untouched.
- A single-module design compiled through the new front door must produce the same candidate fingerprint as `compile_fragment_synth` today; the eleven pinned seven-segment `(Anchor, toward)` pairs stay byte-identical.
- Everything is deterministic: no wall-clock decisions, no randomness, no `HashMap` iteration order reaching a result; block compiles run in parallel but merge into `BTreeMap`s.
- Blocks are never rotated; every block keeps the unpinned seed's frame (inputs west at `-input_channel`, outputs east, ground `y = 1`).
- Blocks compile at `SynthesisBudget::Evaluations(0)`; the parent's fragment search never proposes a block.
- Strict RED/GREEN/REFACTOR: write the failing test, run it, implement, run it, commit. Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Never run `cargo test` while the `fragment_acceptance` harness binary is running (relink fails). Release-only ignored tests: `cargo test --release --lib <name> -- --ignored --nocapture`.
- Reply to the user in Taiwanese Mandarin, plainly; docs and code comments in English.

---

## File structure

| File | Responsibility |
|---|---|
| `src/compile/hierarchy.rs` (new) | `HierarchicalNetlist`, `Module`, `ModuleInstance`, `GatePath`, validation, constant specialisation, `boundary_netlist`, `flatten`, `lower_hierarchy`. |
| `src/frontend/yosys_json.rs` | Read every module; instance cells; `$paramod` names; type check before `Y`. |
| `src/frontend/mod.rs` | `synthesize_verilog_hierarchical`. |
| `src/circuits/hierarchical_builder.rs` (new) | `HierarchicalNetlistBuilder` test helper. |
| `src/compile/fragment_synth/relocate.rs` (new) | `translate` and `renumber` over `ExpandedPhysicalCandidate`. |
| `src/compile/fragment_synth/blocks.rs` (new) | `CompiledBlock`, `BlockPort`, `BlockBounds`, `compile_block`. |
| `src/compile/fragment_synth/instance_graph.rs` | `InstanceGraph.blocks`, `BlockInstance`, `with_blocks`. |
| `src/compile/fragment_synth/placement.rs` | Block envelopes, fixed facing, certified delay, blocks in the DAG. |
| `src/compile/fragment_synth/seed.rs` | `place_blocks`, block routes in `route_all`, parent attempt returning the planning candidate. |
| `src/compile/fragment_synth/union.rs` (new) | Splicing and the flat union candidate. |
| `src/compile/fragment_synth/hierarchy_api.rs` (new) | `compile_hierarchical`, recursion, parallel block compiles, case fingerprint. |
| `src/compile/fragment_synth/mod.rs`, `src/compile/mod.rs` | Module declarations and re-exports. |
| `tests/fixtures/ripple_adder8.v` (new) | Verilog fixture with a `full_adder` module. |
| `tests/hierarchical_synthesis.rs` (new) | Yosys-path integration test. |

---

### Task 1: Measure where wall time goes today

**Files:**
- Modify: `src/compile/fragment_synth/seed.rs:418-509` (`build_attempt`)
- Modify: `.superpowers/sdd/2026-09-01-topology-aware-seed-v2/acceptance-report.md`

**Interfaces:**
- Produces: env-gated timing lines `PHASE <name> <millis>` on stderr when `REDA_PHASE_TIMING` is set. Nothing else.

- [ ] **Step 1: Add env-gated phase timing to `build_attempt`**

In `build_attempt`, wrap the four phases. Add at the top of the function:

```rust
        let timing = std::env::var_os("REDA_PHASE_TIMING").is_some();
        let mut phase_started = std::time::Instant::now();
        let mut phase = |name: &str, started: &mut std::time::Instant| {
            if timing {
                eprintln!("PHASE {name} {}", started.elapsed().as_millis());
            }
            *started = std::time::Instant::now();
        };
```

Call `phase("placement", &mut phase_started);` after `plan_with_repairs`, `phase("layout+routing", &mut phase_started);` after `route_all`, `phase("emit+verify", &mut phase_started);` after `services.verifier.verify`, and wrap the final `certify` call so `phase("certify", &mut phase_started);` runs after it returns (bind the result to a local first).

- [ ] **Step 2: Run alu4 and alu4_full with timing**

Run:
```bash
REDA_PHASE_TIMING=1 REDA_EXTRA_CIRCUITS=alu4,alu4_full cargo test --release --lib every_large_circuit -- --ignored --nocapture 2>&1 | grep -E "PHASE|CIRCUIT"
```
Expected: one `PHASE` line per phase per seed attempt (several attempts per circuit if the channel widening loop retries), then `CIRCUIT ... OK`.

- [ ] **Step 3: Record the shares in the acceptance report**

Add a section "Where the time goes (2026-09-04)" to `.superpowers/sdd/2026-09-01-topology-aware-seed-v2/acceptance-report.md` with a table `circuit | placement | layout+routing | emit+verify | certify` in seconds, summing attempts. State the certify share as a percentage; this is the number the spec's §11 asks for.

- [ ] **Step 4: Commit**

```bash
git add src/compile/fragment_synth/seed.rs
git commit -m "chore(synthesis): env-gated phase timing for seed attempts" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: Hierarchical netlist types, validation and flattening

**Files:**
- Create: `src/compile/hierarchy.rs`
- Modify: `src/compile/mod.rs` (add `pub mod hierarchy;` next to the other module declarations, and `pub use hierarchy::{HierarchicalNetlist, Module, ModuleInstance, GatePath, HierarchyError};`)

**Interfaces:**
- Consumes: `crate::compile::{Gate, Netlist}`, `crate::compile::topology::GateKind`.
- Produces:
  - `pub struct HierarchicalNetlist { pub top: String, pub modules: BTreeMap<String, Module> }`
  - `pub struct Module { pub inputs: Vec<String>, pub outputs: Vec<String>, pub gates: Vec<Gate>, pub instances: Vec<ModuleInstance> }`
  - `pub struct ModuleInstance { pub name: String, pub module: String, pub ports: BTreeMap<String, PortBinding> }` with `pub enum PortBinding { Signal(String), Zero, One }`
  - `pub struct GatePath { pub path: Vec<String>, pub module: String, pub gate: usize }`
  - `pub enum HierarchyError { Cycle { modules: Vec<String> }, UnknownModule { instance: String, module: String }, PortMismatch { instance: String, port: String }, UnconnectedPort { instance: String, port: String }, UnknownTop(String) }`
  - `impl HierarchicalNetlist { pub fn validate(&self) -> Result<(), HierarchyError>; pub fn specialise_constants(&self) -> Result<HierarchicalNetlist, HierarchyError>; pub fn boundary_netlist(&self, module: &str) -> Netlist; pub fn flatten(&self) -> Result<(Netlist, Vec<GatePath>), HierarchyError>; pub fn module_order(&self) -> Result<Vec<String>, HierarchyError> }`
  - `pub fn instance_prefix(path: &[String]) -> String` (joins with `.`; sanitises `$paramod\a\P=1` to `a__P_1`).

- [ ] **Step 1: Write the failing tests**

Create `src/compile/hierarchy.rs` with the tests module first (the types are referenced but not yet defined, so the file will not compile; that is the RED state):

```rust
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
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib compile::hierarchy`
Expected: compile error (types not defined).

- [ ] **Step 3: Implement the types and functions**

Above the tests in `src/compile/hierarchy.rs`:

```rust
//! Module hierarchy kept from the front end to placement: a design is a
//! tree of module instances; each module owns gates and child instances.
//! Certification always sees the flattened netlist (`flatten`); only
//! placement and routing see blocks.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::compile::topology::GateKind;
use crate::compile::{Gate, Netlist};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HierarchicalNetlist {
    pub top: String,
    pub modules: BTreeMap<String, Module>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Module {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub gates: Vec<Gate>,
    pub instances: Vec<ModuleInstance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModuleInstance {
    pub name: String,
    pub module: String,
    /// Child port name -> what the parent connects to it.
    pub ports: BTreeMap<String, PortBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PortBinding {
    Signal(String),
    Zero,
    One,
}

/// Where a flattened gate came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}

/// `a.b.c` with Yosys's derived names made identifier-safe.
pub fn instance_prefix(path: &[String]) -> String {
    path.iter()
        .map(|segment| {
            segment
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
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
        enum Mark { Open, Done }
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
    pub fn flatten(&self) -> Result<(Netlist, Vec<GatePath>), HierarchyError> {
        self.validate()?;
        let top = &self.modules[&self.top];
        let mut gates = Vec::new();
        let mut paths = Vec::new();
        self.flatten_into(&self.top, &[], &BTreeMap::new(), &mut gates, &mut paths);
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
    ) {
        let module = &self.modules[module_name];
        let prefix = instance_prefix(path);
        let rename = |signal: &str| -> String {
            if let Some(alias) = aliases.get(signal) {
                return alias.clone();
            }
            if prefix.is_empty() { signal.to_string() } else { format!("{prefix}.{signal}") }
        };
        for (index, gate) in module.gates.iter().enumerate() {
            gates.push(Gate {
                name: rename(&gate.name),
                inputs: gate.inputs.iter().map(|s| rename(s)).collect(),
                output: rename(&gate.output),
                kind: gate.kind,
            });
            paths.push(GatePath { path: path.to_vec(), module: module_name.to_string(), gate: index });
        }
        for instance in &module.instances {
            let mut child_aliases = BTreeMap::new();
            for (port, binding) in &instance.ports {
                if let PortBinding::Signal(signal) = binding {
                    child_aliases.insert(port.clone(), rename(signal));
                }
            }
            let mut child_path = path.to_vec();
            child_path.push(instance.name.clone());
            self.flatten_into(&instance.module, &child_path, &child_aliases, gates, paths);
        }
    }
}

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
        gate.kind = match gate.kind {
            GateKind::Nor(_) => GateKind::Nor(inputs.len()),
            GateKind::Or(_) => GateKind::Or(inputs.len()),
            other => other,
        };
        gate.inputs = inputs;
    }
    Ok(clone)
}
```

Note for the implementer: `GateKind::Or(1)` is a bare wire in this project (see `yosys_json.rs` `build_cell`). If folding leaves an `Or` with one input, replace the gate by aliasing: drop the gate and rename its output to its single input throughout the clone (gates' inputs and the module's outputs). Write that as a small helper `alias_single_input_ors(&mut Module)` called at the end of `specialise_module`, and add a test for it (`an_or_left_with_one_input_becomes_an_alias`).

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib compile::hierarchy`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/compile/hierarchy.rs src/compile/mod.rs
git commit -m "feat(compile): hierarchical netlist with validation, constant specialisation and flattening" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Yosys reader keeps the module hierarchy

**Files:**
- Modify: `src/frontend/yosys_json.rs` (`netlist_from_json` at 334-453, `Context::build_cell` at 255-320, tests at 486+)
- Modify: `src/frontend/mod.rs:140-173` (add `synthesize_verilog_hierarchical`)

**Interfaces:**
- Consumes: `HierarchicalNetlist`, `Module`, `ModuleInstance`, `PortBinding` from Task 2.
- Produces:
  - `pub(super) fn hierarchical_netlist_from_json(json: &Value, top_module: &str) -> Result<(HierarchicalNetlist, HashMap<String, String>), FrontendError>` (the map is the top module's output port map, as today).
  - `pub fn synthesize_verilog_hierarchical(verilog_source: &str, top_module: &str) -> Result<(HierarchicalNetlist, HashMap<String, String>), FrontendError>` in `src/frontend/mod.rs`.
  - `netlist_from_json` is unchanged in behaviour for single-module JSON.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/frontend/yosys_json.rs`:

```rust
    fn inv_module(input_bit: u64, output_bit: u64) -> serde_json::Value {
        json!({
            "ports": {
                "a": { "direction": "input", "bits": [input_bit] },
                "y": { "direction": "output", "bits": [output_bit] }
            },
            "cells": {
                "n0": { "type": "$_NOT_", "connections": { "A": [input_bit], "Y": [output_bit] } }
            }
        })
    }

    #[test]
    fn a_submodule_instance_becomes_a_module_instance_with_a_port_map() {
        let json = json!({
            "modules": {
                "inv": inv_module(2, 3),
                "top": {
                    "ports": {
                        "x": { "direction": "input", "bits": [2] },
                        "z": { "direction": "output", "bits": [4] }
                    },
                    "cells": {
                        "u0": { "type": "inv", "connections": { "a": [2], "y": [3] } },
                        "u1": { "type": "inv", "connections": { "a": [3], "y": [4] } }
                    }
                }
            }
        });
        let (design, port_map) = hierarchical_netlist_from_json(&json, "top").expect("reads");
        assert_eq!(design.top, "top");
        assert_eq!(design.modules.len(), 2);
        let top = &design.modules["top"];
        assert_eq!(top.instances.len(), 2);
        assert_eq!(top.instances[0].name, "u0");
        assert_eq!(top.instances[0].module, "inv");
        assert_eq!(top.instances[0].ports["a"], PortBinding::Signal("x".into()));
        let mid = match &top.instances[0].ports["y"] { PortBinding::Signal(s) => s.clone(), other => panic!("{other:?}") };
        assert_eq!(top.instances[1].ports["a"], PortBinding::Signal(mid));
        assert_eq!(top.instances[1].ports["y"], PortBinding::Signal("z".into()));
        assert_eq!(port_map["z"], "z");
        let (flat, _) = design.flatten().expect("flattens");
        assert_eq!(flat.gates.len(), 2);
    }

    #[test]
    fn a_paramod_instance_resolves_to_its_derived_module() {
        let json = json!({
            "modules": {
                "$paramod\\inv\\W=1": inv_module(2, 3),
                "top": {
                    "ports": {
                        "x": { "direction": "input", "bits": [2] },
                        "z": { "direction": "output", "bits": [3] }
                    },
                    "cells": {
                        "u0": { "type": "$paramod\\inv\\W=1", "connections": { "a": [2], "y": [3] } }
                    }
                }
            }
        });
        let (design, _) = hierarchical_netlist_from_json(&json, "top").expect("reads");
        assert!(design.modules.contains_key("$paramod\\inv\\W=1"));
        assert_eq!(design.modules["top"].instances[0].module, "$paramod\\inv\\W=1");
    }

    #[test]
    fn a_constant_instance_port_is_a_constant_binding() {
        let json = json!({
            "modules": {
                "inv": inv_module(2, 3),
                "top": {
                    "ports": { "z": { "direction": "output", "bits": [3] } },
                    "cells": {
                        "u0": { "type": "inv", "connections": { "a": ["0"], "y": [3] } }
                    }
                }
            }
        });
        let (design, _) = hierarchical_netlist_from_json(&json, "top").expect("reads");
        assert_eq!(design.modules["top"].instances[0].ports["a"], PortBinding::Zero);
    }

    #[test]
    fn an_unknown_cell_type_is_reported_as_unknown_not_as_a_missing_y() {
        let json = json!({
            "modules": {
                "top": {
                    "ports": {
                        "x": { "direction": "input", "bits": [2] },
                        "z": { "direction": "output", "bits": [3] }
                    },
                    "cells": {
                        "w0": { "type": "$weird", "connections": { "A": [2], "Q": [3] } }
                    }
                }
            }
        });
        let error = hierarchical_netlist_from_json(&json, "top").unwrap_err().to_string();
        assert!(error.contains("$weird"), "{error}");
        assert!(!error.contains("no `Y` connection"), "{error}");
    }

    #[test]
    fn a_multi_bit_instance_port_maps_per_bit() {
        let json = json!({
            "modules": {
                "buf2": {
                    "ports": {
                        "a": { "direction": "input", "bits": [2, 3] },
                        "y": { "direction": "output", "bits": [4, 5] }
                    },
                    "cells": {
                        "n0": { "type": "$_NOT_", "connections": { "A": [2], "Y": [4] } },
                        "n1": { "type": "$_NOT_", "connections": { "A": [3], "Y": [5] } }
                    }
                },
                "top": {
                    "ports": {
                        "p": { "direction": "input", "bits": [2, 3] },
                        "q": { "direction": "output", "bits": [4, 5] }
                    },
                    "cells": {
                        "u0": { "type": "buf2", "connections": { "a": [2, 3], "y": [4, 5] } }
                    }
                }
            }
        });
        let (design, _) = hierarchical_netlist_from_json(&json, "top").expect("reads");
        let ports = &design.modules["top"].instances[0].ports;
        assert_eq!(ports["a[0]"], PortBinding::Signal("p[0]".into()));
        assert_eq!(ports["a[1]"], PortBinding::Signal("p[1]".into()));
        assert_eq!(ports["y[1]"], PortBinding::Signal("q[1]".into()));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib frontend::yosys_json`
Expected: compile error (`hierarchical_netlist_from_json`, `PortBinding` unresolved).

- [ ] **Step 3: Implement the multi-module reader**

Restructure `netlist_from_json` into a per-module reader. Concretely:

1. Extract the body of `netlist_from_json` from "`let ports = ...`" to the end into
   `fn module_from_json(module: &Map<String, Value>, module_names: &BTreeSet<String>, module_name: &str) -> Result<(Module, HashMap<String, String>), FrontendError>`.
2. In the `driver_of` loop (334-406 today), check the cell type **first**:
   - if `module_names.contains(cell_type)`: record the cell in a separate `instance_cells: Vec<(String, &str, &Map)>` and `continue` (no `Y` lookup);
   - else if `topology::gate_kind_for_yosys_cell(cell_type).is_none()`: return the existing unknown-type error from `build_cell` (move that `unsupported(...)` text into a helper `unknown_cell_error(cell_name, cell_type)` used by both places);
   - else proceed as today.
3. Instance cells need every net that a child *drives* to be resolvable by the parent's own cells: for each instance cell, for each output port of the child module (read `port_directions` from the child's JSON `ports`), register in `signal_of` the net bit → a fresh signal name `format!("{cell_name}__{port}")` (per bit: `{cell_name}__{port}[i]` for multi-bit), so `Context::resolve` treats it like a primary input. Child input ports are resolved through `ctx.resolve` when building the instance (they may be driven by gates, so resolve them **after** the output ports have been resolved; do it in a final pass: resolve each instance input bit with `resolve_input_pin`, mapping `Bit::Zero` → `PortBinding::Zero`, `Bit::One` → `PortBinding::One`).
4. Build `ModuleInstance { name: cell_name, module: cell_type.to_string(), ports }` where `ports` maps each child bit-name (`bit_names(port, bits)`) to the binding. Put the module-level `bit_names` helper at module scope so both call sites share it.
5. The "leftover cells" check (441-449) must not count instance cells.
6. `Module { inputs, outputs, gates: ctx.builder.into_gates(), instances }`.
7. `pub(super) fn hierarchical_netlist_from_json(json, top)`: collect `module_names` from `modules.keys()`, call `module_from_json` for every module, return `HierarchicalNetlist { top, modules }` plus the top module's port map. Run `design.validate()` and map `HierarchyError` to `unsupported(error.to_string())`.
8. `netlist_from_json(json, top)` becomes: `let (design, port_map) = hierarchical_netlist_from_json(json, top)?; let (flat, _) = design.flatten().map_err(|e| unsupported(e.to_string()))?; Ok((flat, port_map))`. Existing single-module tests must still pass byte-for-byte (gate names are unchanged because the top path is empty).

In `src/frontend/mod.rs`, add next to `synthesize_verilog`:

```rust
pub fn synthesize_verilog_hierarchical(
    verilog_source: &str,
    top_module: &str,
) -> Result<(HierarchicalNetlist, HashMap<String, String>), FrontendError> {
    let work_dir = make_work_dir()?;
    let verilog_path = work_dir.join("top.v");
    let synth_py_path = work_dir.join("synth.py");
    let output_json_path = work_dir.join("out.json");
    std::fs::write(&verilog_path, verilog_source)?;
    std::fs::write(&synth_py_path, SYNTH_PY)?;
    let result = run_synth(&synth_py_path, &verilog_path, top_module, &output_json_path);
    let design = match result {
        Ok(()) => {
            let json_text = std::fs::read_to_string(&output_json_path)?;
            let json: serde_json::Value = serde_json::from_str(&json_text)?;
            yosys_json::hierarchical_netlist_from_json(&json, top_module)
        }
        Err(err) => Err(err),
    };
    if design.is_ok() {
        let _ = std::fs::remove_dir_all(&work_dir);
    }
    design
}
```

and refactor `synthesize_verilog` to call it and flatten, so both share one path.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib frontend`
Expected: all pass, including the seven pre-existing reader tests.

- [ ] **Step 5: Commit**

```bash
git add src/frontend/yosys_json.rs src/frontend/mod.rs
git commit -m "feat(frontend): read every Yosys module and keep instances as blocks" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: Hierarchical netlist builder for tests

**Files:**
- Create: `src/circuits/hierarchical_builder.rs`
- Modify: `src/circuits/mod.rs` (add `pub(crate) mod hierarchical_builder;`)

**Interfaces:**
- Consumes: `NetlistBuilder` (`src/circuits/netlist_builder.rs`), Task 2 types.
- Produces:
  ```rust
  pub(crate) struct HierarchicalNetlistBuilder { modules: BTreeMap<String, Module> }
  impl HierarchicalNetlistBuilder {
      pub(crate) fn new() -> Self;
      /// Define a leaf or parent module from a closure that receives a ModuleBuilder.
      pub(crate) fn module(&mut self, name: &str, inputs: &[&str], outputs: &[&str], build: impl FnOnce(&mut ModuleBuilder));
      pub(crate) fn finish(self, top: &str) -> HierarchicalNetlist;
  }
  pub(crate) struct ModuleBuilder { pub gates: NetlistBuilder, instances: Vec<ModuleInstance> }
  impl ModuleBuilder {
      /// ports: (child port, parent signal)
      pub(crate) fn instance(&mut self, name: &str, module: &str, ports: &[(&str, &str)]);
      pub(crate) fn instance_with_constants(&mut self, name: &str, module: &str, ports: &[(&str, PortBinding)]);
  }
  ```
  and test-circuit constructors in the same file under `#[cfg(test)] pub(crate) mod circuits`: `full_adder_module(b: &mut HierarchicalNetlistBuilder)` (defines `full_adder` with inputs `a,b,cin`, outputs `sum,cout` using the same gates as `seed.rs`'s `full_adder` helper), `ripple_adder(bits) -> HierarchicalNetlist`, `alu4_full() -> HierarchicalNetlist` (module `slice` = one bit of `seed.rs`'s `alu4_full`, with `shift_in`, `cin` ports; bit 0's `shift_in` tied `Zero` and `cin` driven by the `sub` signal), `multiplier4() -> HierarchicalNetlist` (module `adder_row`), `alu8() -> HierarchicalNetlist` (module `alu4` = 4 slices + glue; top = 2 × `alu4` with carry chained and shared opcode).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ripple_adder8_is_eight_full_adder_instances_and_flattens_to_the_flat_builder_shape() {
        let design = circuits::ripple_adder(8);
        assert_eq!(design.modules["top"].instances.len(), 8);
        let (flat, paths) = design.flatten().expect("flattens");
        assert_eq!(flat.inputs.len(), 17);
        assert_eq!(flat.outputs.len(), 9);
        assert!(paths.iter().all(|p| p.module == "full_adder"));
        assert!(flat.combinational_order().is_some());
    }

    #[test]
    fn alu8_nests_three_levels() {
        let design = circuits::alu8();
        let order = design.module_order().unwrap();
        assert_eq!(order.iter().position(|m| m == "slice").unwrap() < order.iter().position(|m| m == "alu4").unwrap(), true);
        let (flat, paths) = design.flatten().expect("flattens");
        assert!(paths.iter().any(|p| p.path.len() == 2), "slice inside alu4 inside top");
        assert!(flat.combinational_order().is_some());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib circuits::hierarchical_builder`
Expected: compile error.

- [ ] **Step 3: Implement the builder and the circuits**

Implement `HierarchicalNetlistBuilder` as declared. `ModuleBuilder.gates` is a `NetlistBuilder::with_prefix("g".into())`; `finish` turns each `ModuleBuilder` into a `Module { inputs, outputs, gates: gates.into_gates(), instances }`. For the circuits, port the gate recipes from `seed.rs` `mod extra_circuits` (`xor`, `full_adder`, `alu4_full`, `multiplier4`) so each slice is one module; keep the flat versions in `seed.rs` untouched for the flat comparison. `ripple_adder(bits)`: top inputs `a0..a{n-1}, b0..b{n-1}, cin`, outputs `s0..s{n-1}, cout`; instance `fa{i}` of `full_adder` with `cin` bound to `cin` for i = 0 and to `c{i}` otherwise.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib circuits::hierarchical_builder`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add src/circuits/hierarchical_builder.rs src/circuits/mod.rs
git commit -m "test(circuits): hierarchical netlist builder with ripple, ALU and multiplier designs" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: Lower per module, flatten the lowered modules

**Files:**
- Modify: `src/compile/hierarchy.rs`

**Interfaces:**
- Consumes: `crate::compile::lowering::{lower_optimised, LowerError}`.
- Produces:
  ```rust
  pub struct LoweredHierarchy {
      /// Each module lowered on its own boundary netlist (Task 2 `boundary_netlist`).
      pub modules: BTreeMap<String, Module>,   // gates lowered, instances unchanged
      pub top: String,
      /// Flattening of the lowered modules: what certification sees.
      pub flat: Netlist,
      pub paths: Vec<GatePath>,
  }
  pub fn lower_hierarchy(design: &HierarchicalNetlist) -> Result<LoweredHierarchy, LowerHierarchyError>
  pub enum LowerHierarchyError { Hierarchy(HierarchyError), Lowering { module: String, source: LowerError } }
  impl LoweredHierarchy {
      /// The lowered module as the netlist a block compile takes (its real ports only).
      pub fn block_netlist(&self, module: &str) -> Netlist;
      pub fn as_hierarchical(&self) -> HierarchicalNetlist;
  }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn lowering_per_module_then_flattening_equals_flattening_the_lowered_single_module() {
        // A single-module design lowers exactly as lower_optimised on its netlist.
        let mut design = two_level();
        let inv = design.modules.remove("inv").unwrap();
        design.modules.clear();
        design.modules.insert("inv".into(), inv.clone());
        design.top = "inv".into();
        let lowered = lower_hierarchy(&design).expect("lowers");
        let expected = crate::compile::lowering::lower_optimised(
            &Netlist { inputs: inv.inputs, outputs: inv.outputs, gates: inv.gates },
        )
        .expect("lowers");
        assert_eq!(lowered.flat, expected);
    }

    #[test]
    fn two_instances_of_one_module_lower_identically() {
        let lowered = lower_hierarchy(&two_level()).expect("lowers");
        let inv = &lowered.modules["inv"];
        let first: Vec<_> = lowered.paths.iter().enumerate().filter(|(_, p)| p.path == ["u0"]).map(|(i, _)| &lowered.flat.gates[i]).collect();
        let second: Vec<_> = lowered.paths.iter().enumerate().filter(|(_, p)| p.path == ["u1"]).map(|(i, _)| &lowered.flat.gates[i]).collect();
        assert_eq!(first.len(), inv.gates.len());
        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(&second) {
            assert_eq!(a.kind, b.kind);
        }
        assert!(lowered.flat.gates.iter().all(|g| matches!(g.kind, GateKind::Nor(_) | GateKind::Or(_))));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib compile::hierarchy`
Expected: compile error.

- [ ] **Step 3: Implement `lower_hierarchy`**

For each module in `module_order()`: `let boundary = design.boundary_netlist(name); let lowered = lower_optimised(&boundary)?;` then store `Module { inputs: original.inputs, outputs: original.outputs, gates: lowered.gates, instances: original.instances.clone() }`. Lowering may rename internal signals but keeps port names (they are the boundary netlist's inputs/outputs), so instance port bindings stay valid. Then `flat, paths = as_hierarchical().flatten()`. Note that lowering an output that is directly an input adds buffer gates only in the Yosys reader, not in lowering; if `lower_optimised` refuses a boundary netlist whose output is an input (a module that just passes a signal through), wrap that module's pass-through in the boundary as the reader does (two inverters via `NetlistBuilder::not` twice) and add a test `a_pass_through_module_gets_a_buffer`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib compile::hierarchy`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add src/compile/hierarchy.rs
git commit -m "feat(compile): lower each module on its boundary and flatten the lowered modules" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Candidate translation and identity renumbering

**Files:**
- Create: `src/compile/fragment_synth/relocate.rs`
- Modify: `src/compile/fragment_synth/mod.rs` (add `pub(crate) mod relocate;`)

**Interfaces:**
- Consumes: `ExpandedPhysicalCandidate` and nested types (`candidate.rs:27-402`), `RealisedRouteTree` etc. (`routing.rs:99-137`), identity types (`identity.rs`).
- Produces:
  ```rust
  pub(crate) struct Offset { pub dx: i32, pub dy: i32, pub dz: i32 }
  pub(crate) fn translate(candidate: &mut ExpandedPhysicalCandidate, offset: Offset);
  pub(crate) fn translate_route(tree: &mut RealisedRouteTree, offset: Offset);
  pub(crate) struct IdMap { pub instances: BTreeMap<InstanceId, InstanceId>, pub route_offset: u32 }
  pub(crate) fn renumber(candidate: &mut ExpandedPhysicalCandidate, map: &IdMap) -> Result<(), RelocateError>;
  pub(crate) fn anchors_of(candidate: &ExpandedPhysicalCandidate) -> Vec<Anchor>;  // every anchor-carrying field, in a fixed order
  pub(crate) enum RelocateError { UnmappedInstance(InstanceId) }
  ```

- [ ] **Step 1: Write the failing tests**

Use the existing `full_adder` fixture compile (see `src/compile/fragment_synth/api.rs` tests for how a `SynthesisResult`/`CertifiedCandidate` is obtained; `crate::circuits::full_adder` provides the netlist) to get a real `ExpandedPhysicalCandidate` via `compile_sparse_seed_with_services` at budget zero. Write in `relocate.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::candidate::ExpandedPhysicalCandidate;

    fn full_adder_candidate() -> ExpandedPhysicalCandidate {
        // Reuse the seed test helper that compiles a netlist at budget zero
        // and returns the certified candidate (see seed.rs tests for
        // `certified_seed_for` or the equivalent); clone its candidate.
        crate::compile::fragment_synth::seed::tests::certified_full_adder().candidate().clone()
    }

    #[test]
    fn translate_moves_every_anchor_by_the_offset_and_nothing_else() {
        let before = full_adder_candidate();
        let mut after = before.clone();
        translate(&mut after, Offset { dx: 7, dy: 0, dz: -3 });
        let a = anchors_of(&before);
        let b = anchors_of(&after);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!((y.x - x.x, y.y - x.y, y.z - x.z), (7, 0, -3));
        }
        // The anchor walk covers every field the fingerprint sees: translating
        // back restores the fingerprint exactly.
        translate(&mut after, Offset { dx: -7, dy: 0, dz: 3 });
        assert_eq!(after.fingerprint(), before.fingerprint());
        // And a translated candidate is still a well-formed candidate.
        let mut moved = before.clone();
        translate(&mut moved, Offset { dx: 40, dy: 0, dz: 40 });
        moved.validate_shape().expect("translated candidate keeps its shape");
    }

    #[test]
    fn anchors_of_counts_the_fields_the_fingerprint_serialises() {
        let candidate = full_adder_candidate();
        let json = serde_json::to_string(&fingerprint_payload_for_test(&candidate)).unwrap();
        // Every `"x":` in the payload is one Anchor (Anchor is the only
        // struct with an `x` field in the payload).
        let anchors_in_payload = json.matches("\"x\":").count();
        assert_eq!(anchors_of(&candidate).len(), anchors_in_payload);
    }

    #[test]
    fn renumbering_shifts_instances_and_routes_consistently() {
        let before = full_adder_candidate();
        let mut after = before.clone();
        let map = IdMap {
            instances: before.instances.instances.iter().map(|i| (i.id, InstanceId(i.id.0 + 100))).collect(),
            route_offset: 50,
        };
        renumber(&mut after, &map).expect("renumbers");
        assert!(after.placements.keys().all(|p| p.instance.0 >= 100));
        assert!(after.routes.keys().all(|r| r.0 >= 50));
        assert!(after.routes.values().all(|t| t.branches.iter().all(|b| b.sink.route == t.id)));
        assert!(after.connections.values().all(|c| c.sink.route == c.route && c.route.0 >= 50));
        assert_eq!(after.instances.instances.len(), before.instances.instances.len());
        after.validate_shape().expect("renumbered candidate keeps its shape");
    }
}
```

`fingerprint_payload_for_test` needs the payload structs in `candidate.rs` to be reachable: add `#[cfg(test)] pub(crate) fn fingerprint_payload_for_test(&self) -> impl Serialize + '_` in `candidate.rs` next to `fingerprint()` that builds the same `CandidateFingerprintPayload` (factor the payload construction out of `fingerprint()` into `fn fingerprint_payload(&self) -> CandidateFingerprintPayload<'_>` and have both use it). If `seed::tests::certified_full_adder` does not exist, add it to `seed.rs`'s test module: compile `crate::circuits::full_adder::build_full_adder_netlist().0` (or whatever the fixture constructor is called; check `src/circuits/full_adder.rs`) lowered with `lower_optimised`, through `compile_sparse_seed_with_services` with the same services `api.rs:131-139` builds, at `SearchConfig::checked_defaults()`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib compile::fragment_synth::relocate`
Expected: compile error.

- [ ] **Step 3: Implement**

`translate` applies `shift(anchor)` to: every `PrimitivePlacement.{anchor, delayed.at, blocks[].at}`; every `BoundaryPlacement.{delayed.at, blocks[].at}`; every route via `translate_route` (`cells[].at`, `floors[].at`, `branches[].{root, path[], terminal.at}`); every `RealisedJunction.{at, cells[].at}`; every `VerifiedObservation.site.at`; every `pins` entry's `at` (rebuild the `PortPlacements` map); every `pin_contracts` value's `at`. `anchors_of` visits the same fields in the same order and pushes each anchor. Write both as one generic walker `fn for_each_anchor(candidate: &mut ExpandedPhysicalCandidate, f: &mut dyn FnMut(&mut Anchor))` so the two cannot drift; `anchors_of` clones and collects, `translate` mutates.

`renumber` maps: `InstanceId` inside `PrimitiveId`, `ConnectionId`, `PhysicalEndpointId::{PrimitiveOutput, Landing, Junction}`, `ObservationId::{PrimitiveOutput, InstanceOutput, JunctionOutput}`, `DelayedOwner::Primitive`, `RouteTarget::Connection`, `RealisedJunction.{id, contributors}`, the `InstanceGraph` (`Instance.id`, `expanded.instance`, every `PrimitiveId`/`ConnectionId` inside `expanded.topology.{primitives, connections, output}`, `SinkAssignment.sink`/`driver` instance ids and `terminals`/`contributors`), and route ids (`RouteId`, `RoutedSinkId.route`, `DelayedOwner::Route`, `TerminalRecord.sink.route`, `ConnectionBinding.{route, sink}`) by adding `route_offset`. Rebuild every `BTreeMap` keyed by a renumbered id. Ports (`PortId`) are not touched here. An instance id missing from `map.instances` is `RelocateError::UnmappedInstance`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib compile::fragment_synth::relocate`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add src/compile/fragment_synth/relocate.rs src/compile/fragment_synth/mod.rs src/compile/fragment_synth/candidate.rs src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): translate and renumber an expanded candidate" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: Compile a module as a block

**Files:**
- Create: `src/compile/fragment_synth/blocks.rs`
- Modify: `src/compile/fragment_synth/mod.rs` (add `pub(crate) mod blocks;`)

**Interfaces:**
- Consumes: `compile_sparse_seed_with_services` (`seed.rs:172`), `SeedServices` (`seed.rs:68`), `CertifiedCandidate` (`certification.rs:123`), `Netlist`.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub(crate) struct BlockBounds { pub min: Anchor, pub max: Anchor }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub(crate) struct BlockPort { pub cell: Anchor, pub toward: Facing }
  #[derive(Debug, Clone)]
  pub(crate) struct CompiledBlock {
      pub module: String,
      pub lowered: Netlist,                       // the block netlist it was compiled from
      pub candidate: ExpandedPhysicalCandidate,   // block-local coordinates as compiled
      pub bounds: BlockBounds,
      pub inputs: BTreeMap<String, BlockPort>,    // by port name (lowered.inputs order available via lowered)
      pub outputs: BTreeMap<String, BlockPort>,
      pub delay: ExactDelay,                      // metrics.quality.static_routed_delay
      pub metrics: CandidateMetrics,
  }
  pub(crate) fn compile_block(module: &str, lowered: &Netlist, services: SeedServices<'_>) -> Result<CompiledBlock, BlockError>;
  pub(crate) enum BlockError { Seed { module: String, source: SeedError }, MissingBoundary { module: String, port: String }, PinnedFrame { module: String } }
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_adder_block_exposes_lever_inputs_west_and_lamp_outputs_east() {
        let netlist = crate::compile::lowering::lower_optimised(&crate::circuits::full_adder::build_full_adder_netlist().0).unwrap();
        let (library, config) = crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        let block = compile_block("full_adder", &netlist, services).expect("compiles");
        assert_eq!(block.inputs.len(), 3);
        assert_eq!(block.outputs.len(), 2);
        for port in block.inputs.values() {
            assert_eq!(port.toward, Facing::East);
            assert!(port.cell.x < block.outputs.values().map(|p| p.cell.x).min().unwrap());
            assert_eq!(block.candidate.boundaries.values().flat_map(|b| &b.blocks).find(|b| b.at == port.cell).unwrap().state.kind, BlockKind::Lever);
        }
        for port in block.outputs.values() {
            assert_eq!(port.toward, Facing::East);
            assert_eq!(block.candidate.boundaries.values().flat_map(|b| &b.blocks).find(|b| b.at == port.cell).unwrap().state.kind, BlockKind::Lamp);
        }
        assert!(block.bounds.min.x >= 16 && block.bounds.min.z >= 16, "unpinned seed shifts to the origin margin");
        assert_eq!(block.bounds.min.y, 0, "floors under the ground row");
        assert!(block.bounds.max.y <= 4);
        assert_eq!(block.delay, block.metrics.quality.static_routed_delay);
    }
}
```

`seed::tests::default_services_parts` / `services` are small test helpers to add in `seed.rs`'s test module if absent: they build `Library::default_library()`, `SearchConfig::checked_defaults()` and the `SeedServices` exactly as `api.rs:131-139`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib compile::fragment_synth::blocks`
Expected: compile error.

- [ ] **Step 3: Implement `compile_block`**

```rust
pub(crate) fn compile_block(
    module: &str,
    lowered: &Netlist,
    services: SeedServices<'_>,
) -> Result<CompiledBlock, BlockError> {
    let input = SeedInput { lowered, source_provenance: None, pins: None };
    let certified = compile_sparse_seed_with_services(input, services)
        .map_err(|source| BlockError::Seed { module: module.to_string(), source })?;
    let candidate = certified.candidate().clone();
    let metrics = certified.metrics().clone();
    let mut inputs = BTreeMap::new();
    for (index, name) in lowered.inputs.iter().enumerate() {
        let endpoint = PhysicalEndpointId::PrimaryInput(PortId(index as u32));
        let lever = candidate.boundaries.get(&endpoint)
            .and_then(|b| b.blocks.iter().find(|block| block.state.kind == BlockKind::Lever))
            .ok_or_else(|| BlockError::MissingBoundary { module: module.into(), port: name.clone() })?;
        inputs.insert(name.clone(), BlockPort { cell: lever.at, toward: Facing::East });
    }
    let mut outputs = BTreeMap::new();
    for (index, name) in lowered.outputs.iter().enumerate() {
        let endpoint = PhysicalEndpointId::DeclaredOutput(PortId(index as u32));
        let lamp = candidate.boundaries.get(&endpoint)
            .and_then(|b| b.blocks.iter().find(|block| block.state.kind == BlockKind::Lamp))
            .ok_or_else(|| BlockError::MissingBoundary { module: module.into(), port: name.clone() })?;
        outputs.insert(name.clone(), BlockPort { cell: lamp.at, toward: Facing::East });
    }
    let bounds = bounds_of(&candidate);
    Ok(CompiledBlock { module: module.into(), lowered: lowered.clone(), delay: metrics.quality.static_routed_delay, candidate, bounds, inputs, outputs, metrics })
}

fn bounds_of(candidate: &ExpandedPhysicalCandidate) -> BlockBounds {
    let anchors = crate::compile::fragment_synth::relocate::anchors_of(candidate);
    let mut min = anchors[0];
    let mut max = anchors[0];
    for a in anchors {
        min = Anchor { x: min.x.min(a.x), y: min.y.min(a.y), z: min.z.min(a.z) };
        max = Anchor { x: max.x.max(a.x), y: max.y.max(a.y), z: max.z.max(a.z) };
    }
    BlockBounds { min, max }
}
```

Use the unpinned seed only: `pins: None` guarantees `automatic_boundary_direction` is `East` and the frame is direct (`placement.rs:1172-1179`). `PortId(index as u32)` must use `u32::try_from` with an `IdentityOverflow`-style error in real code; write it that way.

- [ ] **Step 4: Run the test**

Run: `cargo test --lib compile::fragment_synth::blocks`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add src/compile/fragment_synth/blocks.rs src/compile/fragment_synth/mod.rs src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): compile a module as an unpinned block and read its port table" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: Blocks in the instance graph

**Files:**
- Modify: `src/compile/fragment_synth/instance_graph.rs` (struct at 79-85, `with_variants` at 192-377, `validate` at 380)

**Interfaces:**
- Consumes: `CompiledBlock` (Task 7) for port names only.
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
  pub struct BlockInstance {
      pub id: InstanceId,
      pub block: u32,                       // index into the parent's compiled block list
      pub path: Vec<String>,                // instance path segment(s) inside this module (one name)
      pub inputs: Vec<LogicalSignalId>,     // block input k <- parent signal
      pub output_gates: Vec<GateIndex>,     // block output k = synthetic gate row in the planning netlist
  }
  // InstanceGraph gains:
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub blocks: Vec<BlockInstance>,
  // and the driver of block output k is
  //   PhysicalDriver::Instance(InstanceDriver::Primitive { logical_owner: block.id, terminals: vec![PrimitiveId { instance: block.id, node: TopologyNodeId(k as u16) }] })
  // so its endpoint is PhysicalEndpointId::PrimitiveOutput(PrimitiveId { instance: block.id, node: k }).
  pub struct BlockSpec<'a> { pub name: &'a str, pub block: u32, pub inputs: &'a [String] /* parent signals, block input order */, pub outputs: &'a [String] /* parent signals, block output order */ }
  impl InstanceGraph {
      pub(crate) fn with_blocks(planning: &Netlist, library: &Library, blocks: &[BlockSpec<'_>]) -> Result<Self, SynthesisError>;
  }
  impl InstanceGraph { pub fn is_block(&self, id: InstanceId) -> bool; pub fn block(&self, id: InstanceId) -> Option<&BlockInstance>; }
  ```
  The **planning netlist** is the parent's lowered own gates followed by one synthetic gate per block output: `Gate { name: "<inst>.<port>", inputs: <block input signals>, output: <parent signal of that output>, kind: GateKind::Buf }`. `with_blocks` instantiates only the gates before the first synthetic gate; synthetic gates exist so `signal_table` assigns every block output a `LogicalSignalId::GateOutput`. Block instance ids start at `planning.gates.len()`.

- [ ] **Step 1: Write the failing test**

```rust
    /// top: x -> [block u0: inputs a; outputs y, w] ; y -> nor g0 -> z ; w declared output.
    /// Shared with the placement tests (Task 9).
    pub(crate) fn planning_with_one_block() -> (Netlist, Vec<(String, u32, Vec<String>, Vec<String>)>) {
        let planning = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["z".into(), "w".into()],
            gates: vec![
                Gate { name: "g0".into(), inputs: vec!["y".into()], output: "z".into(), kind: GateKind::Nor(1) },
                Gate { name: "u0.y".into(), inputs: vec!["x".into()], output: "y".into(), kind: GateKind::Buf },
                Gate { name: "u0.w".into(), inputs: vec!["x".into()], output: "w".into(), kind: GateKind::Buf },
            ],
        };
        (planning, vec![("u0".into(), 0, vec!["x".into()], vec!["y".into(), "w".into()])])
    }

    pub(crate) fn specs_of(owned: &[(String, u32, Vec<String>, Vec<String>)]) -> Vec<BlockSpec<'_>> {
        owned.iter().map(|(name, block, inputs, outputs)| BlockSpec { name, block: *block, inputs, outputs }).collect()
    }

    #[test]
    fn a_block_joins_the_graph_with_one_driver_per_output_and_one_sink_per_input() {
        let (planning, owned) = planning_with_one_block();
        let library = Library::default_library();
        let graph = InstanceGraph::with_blocks(&planning, &library, &specs_of(&owned)).expect("builds");
        assert_eq!(graph.instances.len(), 1, "only the real gate is instantiated");
        assert_eq!(graph.blocks.len(), 1);
        let block = &graph.blocks[0];
        assert_eq!(block.id, InstanceId(3));
        assert_eq!(block.inputs, vec![LogicalSignalId::PrimaryInput(PortId(0))]);
        assert_eq!(block.output_gates, vec![GateIndex(1), GateIndex(2)]);
        let to_block = graph.assignments.iter().find(|a| a.sink == PhysicalSink::InstanceInput { instance: block.id, input_index: 0 }).unwrap();
        assert_eq!(to_block.driver, PhysicalDriver::PrimaryInput(PortId(0)));
        let from_block = graph.assignments.iter().find(|a| a.sink == PhysicalSink::InstanceInput { instance: InstanceId(0), input_index: 0 }).unwrap();
        assert_eq!(
            endpoint_for_driver(&from_block.driver),
            Some(PhysicalEndpointId::PrimitiveOutput(PrimitiveId { instance: block.id, node: TopologyNodeId(0) }))
        );
        let w = graph.assignments.iter().find(|a| a.sink == PhysicalSink::DeclaredOutput(PortId(1))).unwrap();
        assert_eq!(
            endpoint_for_driver(&w.driver),
            Some(PhysicalEndpointId::PrimitiveOutput(PrimitiveId { instance: block.id, node: TopologyNodeId(1) }))
        );
        assert!(graph.is_block(block.id));
    }

    #[test]
    fn a_graph_without_blocks_serialises_exactly_as_before() {
        let netlist = crate::circuits::and4::build_and4_netlist().0; // any existing fixture
        let lowered = crate::compile::lowering::lower_optimised(&netlist).unwrap();
        let graph = InstanceGraph::one_to_one(&lowered, &Library::default_library()).unwrap();
        let json = serde_json::to_string(&graph).unwrap();
        assert!(!json.contains("\"blocks\""));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib compile::fragment_synth::instance_graph`
Expected: compile error.

- [ ] **Step 3: Implement**

Add the field with `#[serde(default, skip_serializing_if = "Vec::is_empty")] pub blocks: Vec<BlockInstance>` and `blocks: Vec::new()` in `with_variants`'s final struct literal and in the five other `InstanceGraph {` literals in the tree (`grep -rn "InstanceGraph {" src tests`). Implement `with_blocks`:

1. `let real_gates = planning.gates.len() - blocks.iter().map(|b| b.outputs.len()).sum::<usize>();` Build instances for `planning.gates[..real_gates]` exactly as `with_variants` does (reuse it by calling `Self::with_variants` on a copy of the planning netlist truncated to real gates? No: the signal table must see the synthetic gates. Instead, factor the per-gate instantiation loop of `with_variants` (192-233) into `fn instantiate_gates(netlist, library, implementations, upto: usize) -> Result<Vec<Instance>, SynthesisError>` and call it with `upto = real_gates`.)
2. `let (signals, primary_inputs) = signal_table(planning)?;` (sees synthetic gates).
3. For each `BlockSpec` in order: `id = InstanceId(planning.gates.len() as u32 + k)`; `inputs = spec.inputs.iter().map(|s| signals[s])`; `output_gates = spec.outputs.iter().map(|s| match signals[s] { LogicalSignalId::GateOutput(g) => g, _ => error })`.
4. Drivers: write `fn driver_for_signal_with_blocks(signal, instances, blocks) -> Result<PhysicalDriver, _>`: if `signal` is a `GateOutput(g)` that is some block's `output_gates[k]`, return the block driver described in Interfaces; else fall back to `driver_for_signal`.
5. Assignments: for real instances as today; for each block input k: `SinkAssignment { sink: InstanceInput { instance: block.id, input_index: k }, signal: block.inputs[k], driver }`; for declared outputs as today but with the block-aware driver. Sort by sink as today.
6. `validate`: extend so a `GateOutput` whose gate is a synthetic block-output gate is accepted (its "instance" is the block); simplest is to run the existing validation on the truncated netlist for instances and check block assignments separately (`validate_driver` must accept block drivers: extend it to look up `graph.blocks` when `instance_by_id` misses and check `LogicalSignalId::GateOutput(block.output_gates[k])`).

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib compile::fragment_synth::instance_graph && cargo test --lib compile::fragment_synth::candidate`
Expected: pass (the candidate fingerprint tests must still pass because the empty `blocks` field is not serialised).

- [ ] **Step 5: Commit**

```bash
git add src/compile/fragment_synth/instance_graph.rs src/compile/fragment_synth
git commit -m "feat(synthesis): block instances in the instance graph" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: Blocks in the DAG analysis and the placer

**Files:**
- Modify: `src/compile/fragment_synth/placement.rs` (`analyse_instance_dag` 1928-2040, `plan_in_frame` 329-345 envelopes and facing loop, `macro_envelope` 1619, `choose_instance_facing` call site, `macro_output_direction` call sites, `LayoutOwner` unaffected)

**Interfaces:**
- Consumes: `InstanceGraph.blocks`, `BlockInstance` (Task 8).
- Produces:
  ```rust
  /// Per-block facts the placer needs, supplied by the seed.
  #[derive(Debug, Clone, Copy)]
  pub(crate) struct BlockFacts { pub width: i32 /* max.x - min.x + 1 */, pub depth: i32 /* max.z - min.z + 1 */, pub delay_ticks: u64 }
  // SeedPlacementRequest gains:
  pub block_facts: &'a BTreeMap<InstanceId, BlockFacts>,
  // analyse_instance_dag gains a parameter:
  pub(crate) fn analyse_instance_dag(graph: &InstanceGraph, block_delays: &BTreeMap<InstanceId, u64>) -> Result<SeedPlacementAnalysis, SeedPlacementError>
  ```
  A block's envelope is `HorizontalBounds { min_x: 0, max_x: width - 1, min_z: 0, max_z: depth - 1 }` for all four facings; its facing is always `CellFacing::EAST`; its `PreferredInstancePose.preferred_origin` is the world cell that the block's `bounds.min` (x, z) lands on.

- [ ] **Step 1: Write the failing tests**

In `placement.rs` tests:

```rust
    #[test]
    fn a_block_is_a_level_node_with_its_certified_delay_and_a_box_envelope() {
        use crate::compile::fragment_synth::instance_graph::tests::{planning_with_one_block, specs_of};
        let (planning, owned) = planning_with_one_block();
        let graph = InstanceGraph::with_blocks(&planning, &Library::default_library(), &specs_of(&owned)).unwrap();
        let block = graph.blocks[0].id;
        let delays = BTreeMap::from([(block, 37u64)]);
        let analysis = analyse_instance_dag(&graph, &delays).expect("analyses");
        assert_eq!(analysis.nodes[&block].forward_level, 0);
        assert_eq!(analysis.nodes[&InstanceId(0)].forward_level, 1);
        assert_eq!(analysis.nodes[&block].head_ticks, 37);
        assert!(analysis.nodes[&InstanceId(0)].head_ticks > 37);
        let facts = BTreeMap::from([(block, BlockFacts { width: 30, depth: 20, delay_ticks: 37 })]);
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest { graph: &graph, analysis: &analysis, pins: &BTreeMap::new(), block_facts: &facts })
            .expect("plans");
        let pose = plan.instances[&block];
        assert_eq!(pose.facing, CellFacing::EAST);
        let gate = plan.instances[&InstanceId(0)];
        assert!(gate.preferred_origin.x >= pose.preferred_origin.x + 30, "the gate's column starts after the block's width plus a channel");
    }
```

Also update every existing call of `analyse_instance_dag(graph)` (seed.rs:429 and tests) to pass `&BTreeMap::new()`, and every `SeedPlacementRequest {` literal to add `block_facts: &BTreeMap::new()` (grep for the struct name).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib compile::fragment_synth::placement`
Expected: compile error.

- [ ] **Step 3: Implement**

- `analyse_instance_dag`: the id set is `graph.instances` ids ∪ `graph.blocks` ids (duplicate check across both). `instance_delays`: instances via `topology_delay_ticks` as today; blocks from `block_delays[&id]` (missing → `SeedPlacementError::UnresolvedTopology { instance }`). The rest is unchanged because it only reads assignments.
- `plan_in_frame`: `envelopes` = instances via `macro_envelope` ∪ blocks via `block_envelope(facts)`; in the facing loop, blocks get `CellFacing::EAST` without calling `choose_instance_facing`/`macro_output_direction`. Every other use of `request.graph.instances` that means "every placed node" (the level bounds union, `legalize_laterals`, the fold, the per-instance pose output) must iterate instances **and** blocks; search `plan_in_frame` and its helpers for `.instances.iter()` and decide each one. `automatic_input_ports`/`automatic_output_ports` are unchanged.
- The `SeedPlacementPlan` needs no new field: blocks appear in `plan.instances` as `PreferredInstancePose`.
- A block wider than the lateral window cannot be folded: in the fold (426-476) a group that consists of one block and still exceeds the window returns `SeedPlacementError::LateralWindowTooNarrow` (existing variant; add the instance id to its payload if it has none) instead of cutting.
- `plan_fingerprint` must include block ids and poses (it hashes `instances`, so nothing extra if blocks are in that map).

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib compile::fragment_synth`
Expected: all pass (169 existing + new).

- [ ] **Step 5: Commit**

```bash
git add src/compile/fragment_synth
git commit -m "feat(synthesis): place blocks as fixed-facing box macros with certified delays" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 10: Parent seed attempt with blocks

**Files:**
- Modify: `src/compile/fragment_synth/seed.rs` (`build_attempt` 418-509, `place_instances` 904, `reservations_for_components` 2297, `route_all` 1823-2151, `SeedInput` 61)

**Interfaces:**
- Consumes: `CompiledBlock` (Task 7), `BlockFacts` (Task 9), `InstanceGraph.blocks` (Task 8).
- Produces:
  ```rust
  /// What the parent knows about each block it places.
  pub(crate) struct ParentBlocks<'a> { pub compiled: &'a [CompiledBlock] }
  /// The parent's routed planning candidate plus where each block landed.
  pub(crate) struct PlannedParent {
      pub candidate: ExpandedPhysicalCandidate,          // parent gates, boundaries, routes; block bodies as placeholder placements
      pub block_offsets: BTreeMap<InstanceId, Offset>,    // block instance -> translation from block-local to parent coordinates
      pub lowered: Netlist,                               // the planning netlist
  }
  pub(crate) fn plan_parent_with_services(input: SeedInput<'_>, services: SeedServices<'_>, graph: InstanceGraph, blocks: ParentBlocks<'_>, placements: &BTreeMap<InstanceId, InstancePlacementOverride>) -> Result<PlannedParent, SeedError>;
  ```
  `plan_parent_with_services` runs `build_attempt`'s pipeline up to and including `route_all` (with the channel-widening repair loop of `build_variant`), then returns without `validate_shape`/emission/certification. `SeedError` gains `BlockFrameTurned { block: InstanceId }` (the parent's frame is not the direct east frame) and `BlockTooWide { block: InstanceId }`.

- [ ] **Step 1: Write the failing test**

In `seed.rs` tests:

```rust
    #[test]
    fn a_parent_routes_into_and_out_of_a_block_with_repeaters_at_the_boundary() {
        let (library, config) = default_services_parts();
        let services = services(&library, &config);
        let fa = crate::compile::lowering::lower_optimised(&crate::circuits::full_adder::build_full_adder_netlist().0).unwrap();
        let block = crate::compile::fragment_synth::blocks::compile_block("full_adder", &fa, services).unwrap();
        // top: inputs a, b, c ; u0 = full_adder(a, b, c) ; z = NOR(u0.sum) ; cout exported.
        let planning = Netlist {
            inputs: vec!["a".into(), "b".into(), "c".into()],
            outputs: vec!["z".into(), "cout".into()],
            gates: vec![
                Gate { name: "g0".into(), inputs: vec!["sum".into()], output: "z".into(), kind: GateKind::Nor(1) },
                Gate { name: "u0.sum".into(), inputs: vec!["a".into(), "b".into(), "c".into()], output: "sum".into(), kind: GateKind::Buf },
                Gate { name: "u0.cout".into(), inputs: vec!["a".into(), "b".into(), "c".into()], output: "cout".into(), kind: GateKind::Buf },
            ],
        };
        let graph = InstanceGraph::with_blocks(&planning, &library, &[BlockSpec { name: "u0", block: 0, inputs: &fa.inputs.iter().map(|s| s.clone()).collect::<Vec<_>>(), outputs: &["sum".into(), "cout".into()] }]).unwrap();
        let planned = plan_parent_with_services(
            SeedInput { lowered: &planning, source_provenance: None, pins: None },
            services,
            graph,
            ParentBlocks { compiled: std::slice::from_ref(&block) },
            &BTreeMap::new(),
        )
        .expect("plans");
        let block_id = planned.candidate.instances.blocks[0].id;
        let offset = planned.block_offsets[&block_id];
        // Every parent route into the block ends in a repeater facing east on the block's lever cell.
        for (name, port) in &block.inputs {
            let lever = Anchor { x: port.cell.x + offset.dx, y: port.cell.y + offset.dy, z: port.cell.z + offset.dz };
            let terminal = planned.candidate.routes.values().flat_map(|r| &r.branches).find(|b| b.terminal.at == lever)
                .unwrap_or_else(|| panic!("no parent route reaches block input {name}"));
            assert_eq!(terminal.terminal.state.kind, BlockKind::Repeater);
            assert_eq!(terminal.terminal.state.facing, Some(Facing::East));
        }
        // Every parent route out of the block starts on the block's lamp cell.
        for (name, port) in &block.outputs {
            let lamp = Anchor { x: port.cell.x + offset.dx, y: port.cell.y + offset.dy, z: port.cell.z + offset.dz };
            assert!(planned.candidate.routes.values().any(|r| r.branches.iter().any(|b| b.root == lamp)), "no parent route leaves block output {name}");
        }
        // The block body is reserved: no parent route cell lies inside the block's box.
        let inside = |a: Anchor| a.x >= block.bounds.min.x + offset.dx && a.x <= block.bounds.max.x + offset.dx && a.z >= block.bounds.min.z + offset.dz && a.z <= block.bounds.max.z + offset.dz;
        for route in planned.candidate.routes.values() {
            for cell in &route.cells {
                let on_port = block.inputs.values().chain(block.outputs.values()).any(|p| Anchor { x: p.cell.x + offset.dx, y: p.cell.y + offset.dy, z: p.cell.z + offset.dz } == cell.at);
                assert!(!inside(cell.at) || on_port, "route cell {:?} inside the block", cell.at);
            }
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib compile::fragment_synth::seed::tests::a_parent_routes_into_and_out_of_a_block`
Expected: compile error.

- [ ] **Step 3: Implement**

1. **`block_facts`**: from `ParentBlocks` and `graph.blocks`: `BlockFacts { width: max.x - min.x + 1, depth: max.z - min.z + 1, delay_ticks: block.delay.0 }`; `block_delays` likewise. Pass both into `analyse_instance_dag` and `SeedPlacementRequest`.
2. **Frame check**: after `plan_with_repairs`, if `!graph.blocks.is_empty() && placement_plan.frame.forward != Facing::East` return `SeedError::BlockFrameTurned { block: graph.blocks[0].id }`.
3. **`place_blocks`** (new, called right after `place_boundaries`): for each block instance, `pose = plan.instances[&block.id]`, `origin = plan_translation.apply(pose.preferred_origin)`, `offset = Offset { dx: origin.x - compiled.bounds.min.x, dy: placement_plan.frame.origin.y - 1, dz: origin.z - compiled.bounds.min.z }`. Record `block_offsets`. Insert one placeholder `PrimitivePlacement { id: PrimitiveId { instance: block.id, node: TopologyNodeId(0) }, variant: 0, facing: CellFacing::EAST, anchor: origin, delayed: None, blocks: <every block-owned block, translated by offset> }` into `candidate.placements` so channel layout occupancy (`channel_layout.rs:220-225`) and `reservations_for_components` see the body; `claim_blocks(occupied, ...)` for all of them. Register geometry: for output k (`compiled.lowered.outputs[k]`): `sources.insert(PhysicalEndpointId::PrimitiveOutput(PrimitiveId { instance: block.id, node: TopologyNodeId(k) }), SourceGeometry { route_anchor: lamp + offset, allowed_exit: Facing::East })`; for input k: `targets.insert(PhysicalSink::InstanceInput { instance: block.id, input_index: k }, TargetGeometry { terminal: lever + offset, allowed_entry: Facing::West, support: step(lever + offset, Facing::East) /* the block's root dust */, requirement: TerminalRequirement::Exact(RouteTerminalKind::OutputTerminalRepeater) })`. The lever cell itself must **not** be in the placeholder placement's blocks (the parent's route terminal will claim it) and neither must the lamp cell (the parent's route root claims it): filter those two cells out of the placeholder, but keep the stone under the lever and every other cell.
4. **Keep-out above the block**: `reservations_for_components` already closes the cell above every placement block (2329-2340), which covers the block body because it is a placement. Verify by reading that function; if it only closes `y + 1` of component cells and the block's top layer is at `max.y`, that is exactly the spec's "one cell above the block's own top".
5. **`route_all` grouping**: after the instance-connection loop (1836-1877) add a block loop: for each block, for each input k: source = `endpoint_for_driver(&assignment.driver)` for the assignment with sink `InstanceInput { block, k }`; geometry = `targets[InstanceInput { block, k }]`; push `PendingTarget::Connection(ConnectionId::External { instance: block.id, input_index: k }, geometry)` under that source. Block outputs need nothing: their sources are already registered, and consumers find them through assignments (the declared-output loop and the instance loop both go through `endpoint_for_driver`).
6. `route_source_level`/`route_target_level`/`route_target_slack` (used at 1918+) resolve levels through `analysis.nodes`, which now contains block ids, so `PrimitiveOutput(PrimitiveId { instance: block, .. })` and `Landing(External { instance: block, .. })` resolve without change. Check `route_source_instance` (1750) returns the block id for the placeholder `PrimitiveId` (it does: `primitive.instance`).
7. `assign_torch_sockets` and `refresh_primitive_targets` iterate `candidate.instances.instances` only, so blocks are skipped; `primitive_input_geometry` is never asked about a block.
8. **`plan_parent_with_services`**: factor `build_attempt` into `fn plan_attempt(...) -> Result<(ExpandedPhysicalCandidate, BTreeMap<InstanceId, Offset>), SeedError>` (everything through `route_all`) and `fn finish_attempt(candidate, input, services) -> Result<CertifiedCandidate, SeedError>` (validate_shape onward); `build_attempt` = both; `plan_parent_with_services` = the widening loop of `build_variant` around `plan_attempt` with the given graph. The existing flat path must stay byte-identical: `build_variant` keeps constructing its graph with `with_variants` and passes empty `ParentBlocks`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib compile::fragment_synth`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add src/compile/fragment_synth/seed.rs
git commit -m "feat(synthesis): plan a parent seed around compiled blocks" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 11: The flat union with spliced routes

**Files:**
- Create: `src/compile/fragment_synth/union.rs`
- Modify: `src/compile/fragment_synth/mod.rs` (add `pub(crate) mod union;`), `src/compile/fragment_synth/seed.rs` (make `refresh_exact_route_delays` `pub(crate)`, expose `finish_attempt` as `pub(crate) fn certify_planned(candidate, lowered, services) -> Result<CertifiedCandidate, SeedError>`)

**Interfaces:**
- Consumes: `PlannedParent` (Task 10), `CompiledBlock` (Task 7), `relocate::{translate, renumber, IdMap, Offset}` (Task 6), flat netlist + `GatePath`s (Task 5).
- Produces:
  ```rust
  pub(crate) struct UnionInput<'a> {
      pub parent: &'a PlannedParent,
      pub blocks: &'a [CompiledBlock],
      /// For every block instance: which compiled block, and its instance path prefix.
      pub flat: &'a Netlist,                 // flattened lowered netlist of this module
      pub paths: &'a [GatePath],             // one per flat gate
      pub library: &'a Library,
  }
  pub(crate) fn union_candidate(input: UnionInput<'_>) -> Result<ExpandedPhysicalCandidate, UnionError>;
  pub(crate) enum UnionError { Relocate(RelocateError), InstanceGraph(SynthesisError), MissingRoute { block: InstanceId, port: String }, Shape(CandidateError) }
  ```

- [ ] **Step 1: Write the failing test**

In `union.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// gate -> block -> gate, certified as one flat circuit.
    #[test]
    fn a_gate_block_gate_chain_unions_into_one_certified_flat_candidate() {
        let (library, config) = crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        // Hierarchical design: top(x, b, c) : n = NOT x ; u0 = full_adder(n, b, c) ; z = NOT u0.sum ; cout = u0.cout
        let mut hb = crate::circuits::hierarchical_builder::HierarchicalNetlistBuilder::new();
        crate::circuits::hierarchical_builder::circuits::full_adder_module(&mut hb);
        hb.module("top", &["x", "b", "c"], &["z", "cout"], |m| {
            let n = m.gates.not("x");
            m.instance("u0", "full_adder", &[("a", &n), ("b", "b"), ("cin", "c"), ("sum", "sum"), ("cout", "cout")]);
            m.gates.nor_named("z", "z", &["sum".to_string()]);
        });
        let design = hb.finish("top");
        let lowered = crate::compile::hierarchy::lower_hierarchy(&design).unwrap();
        let fa = lowered.block_netlist("full_adder");
        let block = crate::compile::fragment_synth::blocks::compile_block("full_adder", &fa, services).unwrap();
        let ordered = vec![block.clone()];
        let (planning, owned) = planning_netlist(&lowered, "top", &ordered);
        let graph = InstanceGraph::with_blocks(&planning, &library, &owned.iter().map(BlockSpecOwned::as_spec).collect::<Vec<_>>()).unwrap();
        let planned = crate::compile::fragment_synth::seed::plan_parent_with_services(
            SeedInput { lowered: &planning, source_provenance: None, pins: None }, services, graph,
            crate::compile::fragment_synth::seed::ParentBlocks { compiled: std::slice::from_ref(&block) },
            &BTreeMap::new(),
        ).unwrap();
        let union = union_candidate(UnionInput { parent: &planned, blocks: std::slice::from_ref(&block), flat: &lowered.flat, paths: &lowered.paths, library: &library }).expect("unions");
        union.validate_shape().expect("flat shape");
        assert!(union.instances.blocks.is_empty());
        assert_eq!(union.instances.instances.len(), lowered.flat.gates.len());
        // No block boundary objects survive.
        assert_eq!(union.boundaries.len(), lowered.flat.inputs.len() + lowered.flat.outputs.len());
        // Boundary repeaters are counted in route delays.
        let repeaters: u64 = union.routes.values().flat_map(|r| &r.branches).map(|b| b.terminal.repeaters).sum();
        assert!(repeaters >= 3 + 2, "three input joins and two output joins add repeaters");
        let certified = crate::compile::fragment_synth::seed::certify_planned(union, &lowered.flat, services).expect("certifies");
        assert!(certified.metrics().quality.observed_settle > 0);
    }
}
```

Note: the top module's output `z` is built with `nor_named` so it is a gate output. `planning_netlist` and `BlockSpecOwned` live in `union.rs` (they describe the parent's view of its blocks) and Task 12 reuses them:

```rust
pub(crate) struct BlockSpecOwned { pub name: String, pub block: u32, pub inputs: Vec<String>, pub outputs: Vec<String> }
impl BlockSpecOwned { pub(crate) fn as_spec(&self) -> BlockSpec<'_> { BlockSpec { name: &self.name, block: self.block, inputs: &self.inputs, outputs: &self.outputs } } }

/// The parent's lowered own gates followed by one `Buf` gate per block output, so
/// `signal_table` gives every block output a `LogicalSignalId`.
pub(crate) fn planning_netlist(lowered: &LoweredHierarchy, module: &str, ordered_blocks: &[CompiledBlock]) -> (Netlist, Vec<BlockSpecOwned>) {
    let parent = &lowered.modules[module];
    let mut gates = parent.gates.clone();
    let mut specs = Vec::new();
    for instance in &parent.instances {
        let block_index = ordered_blocks.iter().position(|b| b.module == instance.module).expect("every instantiated module was compiled");
        let compiled = &ordered_blocks[block_index];
        let signal = |port: &str| match &instance.ports[port] { PortBinding::Signal(s) => s.clone(), _ => unreachable!("constants were specialised away") };
        let inputs: Vec<String> = compiled.lowered.inputs.iter().map(|p| signal(p)).collect();
        let outputs: Vec<String> = compiled.lowered.outputs.iter().map(|p| signal(p)).collect();
        for (port, output) in compiled.lowered.outputs.iter().zip(&outputs) {
            gates.push(Gate { name: format!("{}.{port}", instance.name), inputs: inputs.clone(), output: output.clone(), kind: GateKind::Buf });
        }
        specs.push(BlockSpecOwned { name: instance.name.clone(), block: block_index as u32, inputs, outputs });
    }
    (Netlist { inputs: parent.inputs.clone(), outputs: parent.outputs.clone(), gates }, specs)
}
```

The planning netlist's inputs are the module's real inputs only (block outputs are driven by the synthetic gates) and its outputs the module's real outputs.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib compile::fragment_synth::union`
Expected: compile error.

- [ ] **Step 3: Implement `union_candidate`**

Algorithm (all maps `BTreeMap`, all iteration in id/path order):

1. **Flat instance graph.** Collect the implementation keys the blocks and the parent chose: for each flat gate `i`, `paths[i]` says (path, module, local gate); if `path` is empty it is a parent gate whose planning instance id is its index in the planning netlist (parent gates come first there); else it is block instance `path[0]`'s gate `local` (nested paths belong to a child's own union and appear flat inside that child's `CompiledBlock`, so at this level treat `path[0]` as the block instance and the remainder as the block's local flat index: the block's `lowered` netlist is itself flat, so map by position within the block: `local index = position of this flat gate among flat gates with the same path[0]`). Build `implementations: BTreeMap<InstanceId, ImplementationKey>` from `parent.candidate.instances.instances[..].implementation` (parent gates) and `block.candidate.instances.instances[..].implementation` (block gates), keyed by the flat instance id `InstanceId(i)`. Then `InstanceGraph::one_to_one_with_implementations(flat, library, &implementations)`.
2. **Id maps.** `parent_map: IdMap { instances: planning id -> flat id for parent gates, route_offset: 0 }`; for each block instance `B` in path order: `block_map: IdMap { instances: block-local id -> flat id, route_offset: next_route_id }` where `next_route_id` starts at `parent.candidate.routes.len()` and grows by each block's route count.
3. **Relocate blocks.** For each block instance: `let mut c = block.candidate.clone(); translate(&mut c, parent.block_offsets[&B]); renumber(&mut c, &block_map)?;`.
4. **Start the union** from `parent.candidate.clone()`: `renumber(&mut union, &parent_map)?` for its gates (block placeholder placements are removed first: drop every `placements` entry whose `id.instance` is a block id, and every `sources`-related placeholder is gone with them); `union.instances = flat graph`.
5. **Merge block contents**: extend `union.placements`, `union.connections`, `union.junctions`, `union.observations` (except `ObservationId::PrimaryInput/DeclaredOutput` of the block) and `union.routes` with the relocated block's, **except** the routes handled by splicing below. Block `boundaries`, `pins`, `pin_contracts`, `pin_name_bindings` are dropped.
6. **Splice inputs.** For each block instance `B`, input k (name = `block.lowered.inputs[k]`), the block's route tree `T` with `T.source == PrimaryInput(PortId(k))` (after relocation it still carries that source; look it up before step 5 removes it). Find the parent branch `pb` whose `terminal.at == lever + offset` (planning candidate; target `Connection(External { B, k })`). Its tree `R`:
   - `R.cells.extend(T.cells)`, `R.floors.extend(T.floors)`, plus the stone under the lever (from the block's `PrimaryInput` boundary blocks, minus the lever) into `R.floors`;
   - remove `pb` from `R.branches`; for each branch `tb` of `T`: push `RealisedRouteBranch { sink: RoutedSinkId { route: R.id, ordinal: next }, target: tb.target, root: pb.root, path: pb.path.clone() ++ tb.path, terminal: TerminalRecord { sink: same new id, ..tb.terminal } }`; update `union.connections[tb.target connection].{route: R.id, sink: new id}`;
   - if a block input is driven by a parent primary input or another block output, `R` is whatever tree the parent laid for that source; the lookup by terminal cell finds it.
   - Fanout across two blocks: the same `R` receives branches from both `T`s; ordinals keep growing.
7. **Splice outputs.** For each block instance `B`, output q (name = `block.lowered.outputs[q]`): block tree `T` containing the branch `ob` with `target == DeclaredOutput(PortId(q))`; parent tree `R` with `R.source == PrimitiveOutput(PrimitiveId { B_planning, node: q })` (look up before renumbering, or match on `branches[..].root == lamp + offset`).
   - New tree `U` with `id: R.id` (after offset; keep the parent's id so parent connections stay valid), `source: T.source`, `cells: T.cells ++ R.cells`, `floors: T.floors ++ R.floors` (the lamp is gone: it was a boundary block, not a route cell; the lamp cell is `R`'s root and already a dust cell in `R.cells`), `branches: T.branches without ob` then for each `rb` of `R`: `RealisedRouteBranch { sink: rb.sink, target: rb.target, root: ob.root, path: ob.path.clone() ++ rb.path, terminal: rb.terminal }`; the surviving block branches are renumbered onto `U.id` with fresh ordinals and their connections updated.
   - Replace `union.routes[R.id]` by `U`; remove `T` from the block routes to merge (if `T` also had internal branches they are now in `U`).
   - A block output that feeds the parent's declared output is the same operation: `rb.target` is `DeclaredOutput(parent port)`.
   - Chained (block A output → parent route → block B input): perform all output splices first, then all input splices; the input splice looks the parent branch up by terminal cell, which is unchanged by the output splice.
8. **Refresh delays**: `refresh_exact_route_delays(&mut tree)` for every tree in `union.routes`.
9. `union.validate_shape().map_err(UnionError::Shape)?`; return.

Route ids of block trees that were spliced away are simply unused; `RouteId`s need not be contiguous (check `validate_shape` does not require contiguity; it keys by map).

- [ ] **Step 4: Run the test**

Run: `cargo test --lib compile::fragment_synth::union`
Expected: pass. If `validate_shape` or certification rejects the join, read the reported `collection` and fix the splice; the likely suspects are the `route.source`/`ConnectionBinding.source` for spliced-output trees (must be the block gate's `PrimitiveOutput` after renumbering) and `RealisedRouteTree::validate`'s path continuity at the join.

- [ ] **Step 5: Commit**

```bash
git add src/compile/fragment_synth/union.rs src/compile/fragment_synth/mod.rs src/compile/fragment_synth/seed.rs src/circuits/hierarchical_builder.rs
git commit -m "feat(synthesis): flat union of a parent and its blocks with spliced boundaries" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 12: `compile_hierarchical` front door

**Files:**
- Create: `src/compile/fragment_synth/hierarchy_api.rs`
- Modify: `src/compile/fragment_synth/mod.rs` (`mod hierarchy_api;` and `pub use hierarchy_api::compile_hierarchical;`), `src/compile/mod.rs` re-exports (add `compile_hierarchical` to the `pub use fragment_synth::{...}` list at 99-102)

**Interfaces:**
- Consumes: everything above; `compile_fragment_synth_with_case_fingerprint` (`api.rs:123`), `synthesis_case_fingerprint` (`api.rs`), `compiled_from_certified` (`api.rs:161`), `run_budgeted_proposals`/`FragmentProposalStream` (as in `api.rs:141-158`).
- Produces:
  ```rust
  pub fn compile_hierarchical(design: &HierarchicalNetlist, budget: SynthesisBudget, pins: Option<&PortPlacements>) -> Result<SynthesisResult, SynthesisError>;
  pub(crate) fn planning_netlist(lowered: &LoweredHierarchy, module: &str, blocks: &BTreeMap<String, CompiledBlock>) -> (Netlist, Vec<BlockSpecOwned>)  // BlockSpecOwned holds owned Strings and derefs to BlockSpec
  ```
  `SynthesisError` gains `Hierarchy(String)` and `Block { module: String, first_path: String, source: String }`.

- [ ] **Step 1: Write the failing tests**

In `hierarchy_api.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn single_module(netlist: &Netlist, name: &str) -> HierarchicalNetlist {
        let mut modules = BTreeMap::new();
        modules.insert(name.to_string(), Module { inputs: netlist.inputs.clone(), outputs: netlist.outputs.clone(), gates: netlist.gates.clone(), instances: vec![] });
        HierarchicalNetlist { top: name.to_string(), modules }
    }

    #[test]
    fn a_single_module_design_is_the_flat_compile_byte_for_byte() {
        for (name, netlist) in [("and4", crate::circuits::and4::build_and4_netlist().0), ("full_adder", crate::circuits::full_adder::build_full_adder_netlist().0)] {
            let lowered = crate::compile::lowering::lower_optimised(&netlist).unwrap();
            let flat = compile_fragment_synth(SynthesisInput { lowered: &lowered, source_provenance: None, pins: None }, SynthesisBudget::Evaluations(0)).unwrap();
            let hier = compile_hierarchical(&single_module(&netlist, name), SynthesisBudget::Evaluations(0), None).unwrap();
            assert_eq!(hier.candidate_fingerprint, flat.candidate_fingerprint, "{name}");
            assert_eq!(hier.case_fingerprint, flat.case_fingerprint, "{name}");
            assert_eq!(hier.metrics, flat.metrics, "{name}");
        }
    }

    #[test]
    fn a_two_level_design_certifies_and_reports_block_reuse() {
        let design = crate::circuits::hierarchical_builder::circuits::ripple_adder(2);
        let result = compile_hierarchical(&design, SynthesisBudget::Evaluations(0), None).expect("certifies");
        assert!(result.metrics.quality.observed_settle > 0);
        assert!(result.compiled.output_positions.contains_key("s1"));
    }

    #[test]
    fn parallel_and_sequential_block_compiles_agree() {
        let design = crate::circuits::hierarchical_builder::circuits::ripple_adder(2);
        let many = compile_hierarchical_with_threads(&design, SynthesisBudget::Evaluations(0), None, 4).unwrap();
        let one = compile_hierarchical_with_threads(&design, SynthesisBudget::Evaluations(0), None, 1).unwrap();
        assert_eq!(many.candidate_fingerprint, one.candidate_fingerprint);
    }
}
```

The pinned seven-segment equality lives in `tests/build_circuit_pins.rs`: add a test there that builds the checked seven-segment fixture as a single-module `HierarchicalNetlist` (reuse the fixture's netlist and pins exactly as the existing `topology_aware_seed_preserves_the_checked_seven_segment_pin_contract` test does), compiles both ways, and asserts equal candidate fingerprints and the eleven pin `(Anchor, toward)` pairs.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib compile::fragment_synth::hierarchy_api`
Expected: compile error.

- [ ] **Step 3: Implement**

First give the fragment proposal stream a pluggable compiler, so the budgeted search can re-plan a parent instead of a flat seed. In `fragment.rs`:

```rust
/// How a proposal's variant is turned into a certified candidate.
pub(crate) type VariantCompiler<'a> = dyn Fn(&SeedVariant) -> Result<CertifiedCandidate, SeedError> + 'a;

pub(crate) struct FragmentProposalStream<'a> {
    input: SeedInput<'a>,
    services: SeedServices<'a>,
    compile: Box<VariantCompiler<'a>>,
    variants: BTreeMap<Fingerprint, SeedVariant>,
    next_single_index: u64,
    next_duplicate_index: u64,
}

impl<'a> FragmentProposalStream<'a> {
    pub(crate) fn new(input: SeedInput<'a>, services: SeedServices<'a>) -> Self {
        Self::with_compiler(input, services, Box::new(move |variant| {
            compile_sparse_seed_variant_with_services(input, services, variant)
        }))
    }
    pub(crate) fn with_compiler(input: SeedInput<'a>, services: SeedServices<'a>, compile: Box<VariantCompiler<'a>>) -> Self {
        Self { input, services, compile, variants: BTreeMap::new(), next_single_index: 0, next_duplicate_index: 0 }
    }
}
```

and replace the one call `compile_sparse_seed_variant_with_services(self.input, self.services, &variant)` in `next` by `(self.compile)(&variant)`. The flat path is unchanged.

Then `hierarchy_api.rs`:

```rust
pub fn compile_hierarchical(design: &HierarchicalNetlist, budget: SynthesisBudget, pins: Option<&PortPlacements>) -> Result<SynthesisResult, SynthesisError> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    compile_hierarchical_with_threads(design, budget, pins, threads)
}

pub(crate) fn compile_hierarchical_with_threads(design: &HierarchicalNetlist, budget: SynthesisBudget, pins: Option<&PortPlacements>, threads: usize) -> Result<SynthesisResult, SynthesisError> {
    let design = design.specialise_constants().map_err(|e| SynthesisError::Hierarchy(e.to_string()))?;
    let lowered = lower_hierarchy(&design).map_err(|e| SynthesisError::Hierarchy(e.to_string()))?;
    if lowered.modules[&lowered.top].instances.is_empty() {
        // Exactly today's path: same case fingerprint, same candidate.
        return compile_fragment_synth(SynthesisInput { lowered: &lowered.flat, source_provenance: None, pins }, budget);
    }
    let library = Library::default_library();
    let search_config = SearchConfig::checked_defaults();
    let certification_config = CertificationConfig::from_search(&search_config);
    let services = SeedServices {
        library: &library,
        placer: &TopologyAwareSeedPlacer,
        router: &GuardedPhysicalRouter,
        emitter: &DurableSeedEmitter,
        verifier: &DurableSeedVerifier,
        certifier: &CompleteCandidateCertifier,
        search_config: &search_config,
    };
    let blocks = compile_blocks(&lowered, services, threads)?;               // BTreeMap<String, CompiledBlock>
    let ordered: Vec<CompiledBlock> = ordered_blocks(&lowered, &lowered.top, &blocks);   // the top's direct children, in module_order
    let top_flat = &lowered.flat;
    let compile_variant = |variant: &SeedVariant| -> Result<CertifiedCandidate, SeedError> {
        compile_module_with_blocks(&lowered, &lowered.top, &ordered, pins, services, variant)
    };
    let certified = compile_variant(&SeedVariant::default()).map_err(|e| SynthesisError::Seed(format!("{}: {e}", lowered.top)))?;
    let flat_input = SynthesisInput { lowered: top_flat, source_provenance: None, pins };
    let flat_case = synthesis_case_fingerprint(&flat_input, &search_config, &certification_config, &library);
    let case_fingerprint = SynthesisCaseFingerprint::from_fingerprint(canonical_fingerprint(
        &[flat_case.as_str().as_bytes(), &serde_json::to_vec(&design).expect("design serialises")].concat(),
    ));
    let clock = SystemMonotonicClock::start();
    let seed_input = SeedInput::from(&flat_input);
    let mut proposals = FragmentProposalStream::with_compiler(seed_input, services, Box::new(compile_variant));
    let summary = run_budgeted_proposals(certified, budget, &clock, &mut proposals);
    let compiled = compiled_from_certified(&summary.best, top_flat)?;
    let metrics = summary.best.metrics().clone();
    let candidate_fingerprint = metrics.candidate_fingerprint.clone();
    Ok(SynthesisResult { compiled, metrics, trace: summary.trace, evaluations_used: summary.evaluations_used, case_fingerprint, candidate_fingerprint, stop_reason: summary.stop_reason })
}

/// Plan the parent around its blocks with `variant` applied to the parent's own gates,
/// build the flat union and certify it.
fn compile_module_with_blocks(lowered: &LoweredHierarchy, module: &str, ordered: &[CompiledBlock], pins: Option<&PortPlacements>, services: SeedServices<'_>, variant: &SeedVariant) -> Result<CertifiedCandidate, SeedError> {
    let (planning, owned) = planning_netlist(lowered, module, ordered);
    let parent_gates = lowered.modules[module].gates.len() as u32;
    // A proposal that names a block gate (flat id beyond the parent's own gates) cannot apply here.
    if variant.implementations.keys().chain(variant.placements.keys()).any(|id| id.0 >= parent_gates)
        || variant.duplicates.iter().any(|d| d.canonical.0 >= parent_gates)
    {
        return Err(SeedError::Incomplete("proposal targets a block gate"));
    }
    let specs: Vec<BlockSpec<'_>> = owned.iter().map(BlockSpecOwned::as_spec).collect();
    let graph = InstanceGraph::with_blocks_and_variants(&planning, services.library, &specs, &variant.implementations, &variant.duplicates)?;
    let planned = plan_parent_with_services(SeedInput { lowered: &planning, source_provenance: None, pins }, services, graph, ParentBlocks { compiled: ordered }, &variant.placements)?;
    let module_flat = module_flattening(lowered, module);   // (Netlist, Vec<GatePath>) with `module` as top
    let union = union_candidate(UnionInput { parent: &planned, blocks: ordered, flat: &module_flat.0, paths: &module_flat.1, library: services.library })
        .map_err(|e| SeedError::Union(e.to_string()))?;
    certify_planned(union, &module_flat.0, services)
}
```

Add `SeedError::Union(String)` (`#[error("flat union failed: {0}")]`). `InstanceGraph::with_blocks_and_variants` is `with_blocks` plus the `implementations`/`duplicates` handling of `with_variants` (factor so `with_blocks` = `with_blocks_and_variants(.., &BTreeMap::new(), &[])`); `plan_parent_with_services` forwards its `placements` parameter to `place_instances`.

- `compile_blocks(lowered, services, threads)`: walk `lowered.as_hierarchical().module_order()` (children first), skipping the top and modules nobody instantiates. Leaves (no instances) are compiled by `compile_block` on `threads` scoped threads (`std::thread::scope`) pulling names from a `Mutex<VecDeque<String>>` and pushing `(name, CompiledBlock)` into a `Mutex<BTreeMap<String, CompiledBlock>>`; a thread error is returned as `SynthesisError::Block`. Parents among the children are compiled after all their own children are present, sequentially, by `compile_module_with_blocks(lowered, name, &ordered_blocks(lowered, name, &done), None, services, &SeedVariant::default())` and wrapped with `CompiledBlock::from_certified(name, &lowered.block_netlist(name), certified)` (factor this constructor out of Task 7's `compile_block`). Deterministic: results are keyed by module name; nothing depends on which thread finished first.
- `ordered_blocks(lowered, module, done)`: the distinct modules instantiated by `module`, in `module_order`, cloned out of `done`.
- `module_flattening(lowered, module)`: `let mut h = lowered.as_hierarchical(); h.top = module.into(); h.flatten()`.
- Error mapping: `BlockError` → `SynthesisError::Block { module, first_path: first instance path using it, source: error.to_string() }`; parent `SeedError` → `SynthesisError::Seed(format!("{module}: {error}"))`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib compile::fragment_synth::hierarchy_api && cargo test --release --test build_circuit_pins`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add src/compile/fragment_synth/hierarchy_api.rs src/compile/fragment_synth/mod.rs src/compile/fragment_synth/api.rs src/compile/mod.rs tests/build_circuit_pins.rs
git commit -m "feat(synthesis): compile_hierarchical front door with parallel block compiles" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 13: Acceptance circuits and the Verilog fixture

**Files:**
- Modify: `src/compile/fragment_synth/seed.rs` tests (`mod extra_circuits`, `run_cases`)
- Create: `tests/fixtures/ripple_adder8.v`, `tests/hierarchical_synthesis.rs`

**Interfaces:**
- Consumes: `compile_hierarchical`, `synthesize_verilog_hierarchical`, `hierarchical_builder::circuits`.

- [ ] **Step 1: Add the release-only hierarchical acceptance test**

In `seed.rs` `mod extra_circuits`, add `run_hierarchical_cases(cases: Vec<(String, HierarchicalNetlist)>)` mirroring `run_cases` (same `REDA_EXTRA_CIRCUITS` filter, prints `CIRCUIT <name> (hierarchical): OK gates=<flat gates> blocks_compiled=<distinct modules> ticks=.. blocks=.. in ..`), and

```rust
        #[test]
        #[ignore = "release-only: cargo test --release --lib every_hierarchical_circuit -- --ignored --nocapture"]
        fn every_hierarchical_circuit_certifies_through_module_floorplan() {
            use crate::circuits::hierarchical_builder::circuits as h;
            run_hierarchical_cases(vec![
                ("ripple_adder8".into(), h::ripple_adder(8)),
                ("alu4_full".into(), h::alu4_full()),
                ("multiplier4".into(), h::multiplier4()),
                ("alu8".into(), h::alu8()),
            ]);
        }
```

- [ ] **Step 2: Write the Verilog fixture and its test**

`tests/fixtures/ripple_adder8.v`:

```verilog
module full_adder(input a, input b, input cin, output sum, output cout);
  assign sum = a ^ b ^ cin;
  assign cout = (a & b) | (b & cin) | (a & cin);
endmodule

module ripple_adder8(input [7:0] a, input [7:0] b, input cin, output [7:0] s, output cout);
  wire [8:0] c;
  assign c[0] = cin;
  genvar i;
  generate
    for (i = 0; i < 8; i = i + 1) begin : bit
      full_adder fa(.a(a[i]), .b(b[i]), .cin(c[i]), .sum(s[i]), .cout(c[i+1]));
    end
  endgenerate
  assign cout = c[8];
endmodule
```

`tests/hierarchical_synthesis.rs`:

```rust
use reda::compile::{compile_hierarchical, SynthesisBudget};
use reda::frontend::synthesize_verilog_hierarchical;

#[test]
#[ignore = "release-only: needs python + yosys; cargo test --release --test hierarchical_synthesis -- --ignored"]
fn the_verilog_ripple_adder8_keeps_eight_full_adder_instances_and_certifies() {
    let source = std::fs::read_to_string("tests/fixtures/ripple_adder8.v").expect("fixture");
    let (design, _) = synthesize_verilog_hierarchical(&source, "ripple_adder8").expect("synthesizes");
    let top = &design.modules["ripple_adder8"];
    assert_eq!(top.instances.len(), 8);
    assert!(design.modules.keys().any(|m| m.contains("full_adder")));
    let result = compile_hierarchical(&design, SynthesisBudget::Evaluations(0), None).expect("certifies");
    assert!(result.metrics.quality.observed_settle > 0);
}
```

Yosys may inline the generate-loop instances under names like `bit[0].fa`; the reader keeps them as instance names (sanitised by `instance_prefix`). If Yosys flattens the `full_adder` because it is small, add `(* keep_hierarchy *)` to the module in the fixture and note it in the report.

- [ ] **Step 3: Run the hierarchical circuits**

Run:
```bash
cargo test --release --lib every_hierarchical_circuit -- --ignored --nocapture 2>&1 | grep CIRCUIT
```
Expected: four `OK` lines. Then:
```bash
cargo test --release --test hierarchical_synthesis -- --ignored
```
Expected: pass.

If a circuit fails, this is the algorithm-design loop the project uses: find the root cause (systematic-debugging skill), fix the general rule, never the case. Record each rule in the design doc (Task 14).

- [ ] **Step 4: Commit**

```bash
git add src/compile/fragment_synth/seed.rs tests/fixtures/ripple_adder8.v tests/hierarchical_synthesis.rs
git commit -m "test(synthesis): hierarchical acceptance circuits and a Verilog ripple adder fixture" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 14: Verification chain, report, memory

**Files:**
- Modify: `.superpowers/sdd/2026-09-01-topology-aware-seed-v2/channel-routing-design.md` (new section "Module floorplan"), `.superpowers/sdd/2026-09-01-topology-aware-seed-v2/acceptance-report.md`, memory `seed-v2-status.md`.

- [ ] **Step 1: Run the full chain**

In this order, never overlapping with the harness:

```bash
cargo test --lib compile::fragment_synth && cargo test --lib compile::routing && cargo test --lib compile::hierarchy && cargo test --lib frontend && cargo test --lib circuits
```
```bash
cargo test --release --no-fail-fast --lib --test build_circuit_pins --test fragment_synth_acceptance --test channel_safety --test terminal_handover --test fragment_synth_architecture --test reference_circuits
```
```bash
cargo test --release --lib every_extra_circuit -- --ignored --nocapture 2>&1 | grep -E "CIRCUIT|test result"
```
```bash
cargo test --release --lib every_large_circuit -- --ignored --nocapture 2>&1 | grep -E "CIRCUIT|test result"
```
```bash
cargo test --release --lib every_hierarchical_circuit -- --ignored --nocapture 2>&1 | grep -E "CIRCUIT|test result"
```
```bash
cargo run --release --bin fragment_acceptance -- --baseline tests/fixtures/fragment_synth_baseline.json --output %TEMP%/acceptance-floorplan.json --shipping-source src/compile/fragment_synth/shipping_config.rs --shuffle-seed 0x5245444120260831
```
Expected: every suite green; the six harness cases certified with the same budget-zero numbers as `acceptance-2026-09-04.json` (and4 34/563, verilog:and4 32/451, full_adder 84/2598, segment_a 117/7173, seven_segment 152/16045, pinned 344/24050). Any difference is a regression to explain or fix before proceeding.

```bash
cargo clippy --lib 2>&1 | grep -E "^(warning|error)" | sort | uniq -c
```
Expected: only the pre-existing warnings (`LayoutRepair` variants, too-many-arguments, one `clone` on `Copy`).

- [ ] **Step 2: Write the report**

In `acceptance-report.md` add "Module floorplan (2026-09-xx)": a table `circuit | flat ticks | hierarchical ticks | flat blocks | hierarchical blocks | flat time | hierarchical time | modules compiled` for the four circuits, the Verilog fixture result, the six-case harness confirmation, and the phase-time table from Task 1. In `channel-routing-design.md` add "Module floorplan" listing every rule added while making the four circuits pass (each as a decision with its reason, in the existing style).

- [ ] **Step 3: Update memory**

Rewrite `C:\Users\LTY\.claude\projects\C--Users-LTY-Desktop-REDA\memory\seed-v2-status.md`'s state paragraph with the floorplan outcome, the new test commands (`every_hierarchical_circuit`, `hierarchical_synthesis`), and what is still open.

- [ ] **Step 4: Commit and report to the user**

```bash
git add -A src tests docs
git commit -m "docs(synthesis): module floorplan acceptance" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

Report in Taiwanese Mandarin: what certifies, the numbers table, any rule that had to be added, and what was left out.

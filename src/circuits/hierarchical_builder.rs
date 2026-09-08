//! A Rust-side builder for `HierarchicalNetlist` test circuits, so later
//! tasks can construct hierarchical designs without going through the
//! Yosys JSON reader. Mirrors `NetlistBuilder`'s "one gate/instance at a
//! time" style, but at the module level.
//!
//! `HierarchicalNetlist::flatten` aliases a child module's signals by
//! literal port-name match: wherever a module's own gates reference the
//! exact string of one of its declared ports (as a gate input or a gate's
//! own `output` field), that string is what the parent's binding rewrites.
//! `NetlistBuilder`'s reduction helpers (`and_reduce`/`or_reduce`/`xor`
//! below) always invent their own generated names, so whenever a value
//! computed by a module's *own* gates (as opposed to passed straight
//! through from a child instance) is meant to become one of that module's
//! declared outputs, [`expose`] gives it the literal name the port
//! declares.

use std::collections::BTreeMap;

use crate::circuits::netlist_builder::NetlistBuilder;
use crate::compile::{HierarchicalNetlist, Module, ModuleInstance, PortBinding};

pub(crate) struct HierarchicalNetlistBuilder {
    modules: BTreeMap<String, Module>,
}

impl HierarchicalNetlistBuilder {
    pub(crate) fn new() -> Self {
        HierarchicalNetlistBuilder {
            modules: BTreeMap::new(),
        }
    }

    /// Define a leaf or parent module from a closure that receives a
    /// `ModuleBuilder`. `inputs`/`outputs` are the module's declared port
    /// names, in the literal form its own gates must use to produce or
    /// consume them (see the module doc comment).
    pub(crate) fn module(
        &mut self,
        name: &str,
        inputs: &[&str],
        outputs: &[&str],
        build: impl FnOnce(&mut ModuleBuilder),
    ) {
        let mut builder = ModuleBuilder {
            gates: NetlistBuilder::with_prefix("g".to_string()),
            instances: Vec::new(),
        };
        build(&mut builder);
        let module = Module {
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
            gates: builder.gates.into_gates(),
            instances: builder.instances,
        };
        self.modules.insert(name.to_string(), module);
    }

    pub(crate) fn finish(self, top: &str) -> HierarchicalNetlist {
        HierarchicalNetlist {
            top: top.to_string(),
            modules: self.modules,
        }
    }
}

pub(crate) struct ModuleBuilder {
    pub(crate) gates: NetlistBuilder,
    instances: Vec<ModuleInstance>,
}

impl ModuleBuilder {
    /// Instantiate `module` under `name`. `ports` is (child port, parent
    /// signal): the parent-side wire each child port is tied to.
    pub(crate) fn instance(&mut self, name: &str, module: &str, ports: &[(&str, &str)]) {
        let ports = ports
            .iter()
            .map(|(port, signal)| (port.to_string(), PortBinding::Signal(signal.to_string())))
            .collect();
        self.instances.push(ModuleInstance {
            name: name.to_string(),
            module: module.to_string(),
            ports,
        });
    }

    /// Like [`ModuleBuilder::instance`], but a binding may also be a
    /// constant. A constant binding only survives to `flatten` if a prior
    /// `specialise_constants` folded it into a NOR/OR input somewhere in
    /// the child module's own gates -- see `compile::hierarchy` for the
    /// rule.
    pub(crate) fn instance_with_constants(
        &mut self,
        name: &str,
        module: &str,
        ports: &[(&str, PortBinding)],
    ) {
        let ports = ports
            .iter()
            .map(|(port, binding)| (port.to_string(), binding.clone()))
            .collect();
        self.instances.push(ModuleInstance {
            name: name.to_string(),
            module: module.to_string(),
            ports,
        });
    }
}

/// Force `value` (already computed under some `NetlistBuilder`-generated
/// name) to also be available under the literal signal name `name`.
/// Needed wherever a module's own gates compute a value that must become
/// one of that module's declared output ports -- `flatten`'s aliasing is
/// driven entirely by literal port-name matches within the module's own
/// gate list (see the module doc comment), while every reduction helper
/// invents its own name for its final gate.
///
/// Realised as `NOT(NOT(value))`: two ordinary single-input NOR gates
/// (exactly what every other `NOT` in this codebase is), with the second
/// one carrying the literal `name`. A pure pass-through from a child
/// instance's own output binding never needs this -- only a value a
/// module computes with its *own* gates does.
fn expose(gates: &mut NetlistBuilder, value: &str, name: &str) -> String {
    let inverted = gates.not(value);
    gates.nor_named(name, name, &[inverted])
}

/// `y = a XOR b`, ported from `fragment_synth::seed`'s `extra_circuits::xor`
/// test helper (kept untouched there for the flat/hierarchical comparison).
fn xor(gates: &mut NetlistBuilder, a: &str, b: &str) -> String {
    let na = gates.not(a);
    let nb = gates.not(b);
    let left = gates.and_reduce(vec![a.to_string(), nb]);
    let right = gates.and_reduce(vec![na, b.to_string()]);
    gates.or_reduce(vec![left, right])
}

/// One bit of ripple-carry addition: `sum = a XOR b XOR cin`,
/// `cout = majority(a, b, cin)`. Ported from `extra_circuits::full_adder`.
/// `sum_name`/`cout_name` are the literal names the caller wants the two
/// results exposed under (see [`expose`]); pass `None` when the value is
/// purely internal (not one of the enclosing module's own declared
/// outputs).
fn full_adder_gates(
    gates: &mut NetlistBuilder,
    a: &str,
    b: &str,
    cin: &str,
    sum_name: Option<&str>,
    cout_name: Option<&str>,
) -> (String, String) {
    let ab = gates.and_reduce(vec![a.to_string(), b.to_string()]);
    let bc = gates.and_reduce(vec![b.to_string(), cin.to_string()]);
    let ac = gates.and_reduce(vec![a.to_string(), cin.to_string()]);
    let cout = gates.or_reduce(vec![ab, bc, ac]);
    let cout = match cout_name {
        Some(name) => expose(gates, &cout, name),
        None => cout,
    };
    let s1 = xor(gates, a, b);
    let sum = xor(gates, &s1, cin);
    let sum = match sum_name {
        Some(name) => expose(gates, &sum, name),
        None => sum,
    };
    (sum, cout)
}

/// `full_adder`: inputs `a, b, cin`; outputs `sum, cout`. Same gate
/// structure as `extra_circuits::full_adder`, packaged as its own module.
pub(crate) fn full_adder_module(builder: &mut HierarchicalNetlistBuilder) {
    builder.module("full_adder", &["a", "b", "cin"], &["sum", "cout"], |mb| {
        full_adder_gates(&mut mb.gates, "a", "b", "cin", Some("sum"), Some("cout"));
    });
}

#[cfg(test)]
pub(crate) mod circuits {
    use super::*;

    /// `full_adder` instantiated `bits` times, cout chained bit to bit.
    /// Top inputs: `a0..a{bits-1}, b0..b{bits-1}, cin`; outputs:
    /// `s0..s{bits-1}, cout`. Bit 0's `cin` is the top's own `cin` input,
    /// so no constant is needed anywhere in this design.
    pub(crate) fn ripple_adder(bits: usize) -> HierarchicalNetlist {
        let mut builder = HierarchicalNetlistBuilder::new();
        full_adder_module(&mut builder);

        let mut inputs: Vec<String> = (0..bits).map(|i| format!("a{i}")).collect();
        inputs.extend((0..bits).map(|i| format!("b{i}")));
        inputs.push("cin".to_string());
        let mut outputs: Vec<String> = (0..bits).map(|i| format!("s{i}")).collect();
        outputs.push("cout".to_string());
        let input_refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let output_refs: Vec<&str> = outputs.iter().map(String::as_str).collect();

        builder.module("top", &input_refs, &output_refs, |mb| {
            for i in 0..bits {
                let a_i = format!("a{i}");
                let b_i = format!("b{i}");
                let cin_i = if i == 0 {
                    "cin".to_string()
                } else {
                    format!("c{}", i - 1)
                };
                let sum_i = format!("s{i}");
                let cout_i = if i + 1 == bits {
                    "cout".to_string()
                } else {
                    format!("c{i}")
                };
                mb.instance(
                    &format!("fa{i}"),
                    "full_adder",
                    &[
                        ("a", a_i.as_str()),
                        ("b", b_i.as_str()),
                        ("cin", cin_i.as_str()),
                        ("sum", sum_i.as_str()),
                        ("cout", cout_i.as_str()),
                    ],
                );
            }
        });
        builder.finish("top")
    }

    /// One bit of `extra_circuits::alu4_full`'s 8-way opcode ALU: `sel0..
    /// sel7` are the pre-decoded one-hot opcode selects, `sub`/`cin` carry
    /// the shared subtract-mode and ripple-carry state, and `shift_in` is
    /// the SHL chain input (the previous bit's `a`; zero for bit 0). The
    /// opcode decode and the zero flag stay in `top`'s own glue, matching
    /// `alu4_full`'s "decode once, reuse per bit" structure.
    fn slice_module(builder: &mut HierarchicalNetlistBuilder) {
        let sel: Vec<String> = (0..8).map(|k| format!("sel{k}")).collect();
        let mut inputs: Vec<String> = vec!["a".into(), "b".into()];
        inputs.extend(sel.iter().cloned());
        inputs.extend(["sub".to_string(), "cin".to_string(), "shift_in".to_string()]);
        let input_refs: Vec<&str> = inputs.iter().map(String::as_str).collect();

        builder.module("slice", &input_refs, &["r", "cout"], |mb| {
            let gates = &mut mb.gates;
            let nsub = gates.not("sub");
            let ny = gates.not("b");
            let keep = gates.and_reduce(vec!["b".to_string(), nsub.clone()]);
            let flip = gates.and_reduce(vec![ny, "sub".to_string()]);
            let operand = gates.or_reduce(vec![keep, flip]);

            let (sum, _cout) = full_adder_gates(gates, "a", &operand, "cin", None, Some("cout"));

            let and_ = gates.and_reduce(vec!["a".to_string(), "b".to_string()]);
            let or_ = gates.or_reduce(vec!["a".to_string(), "b".to_string()]);
            let xo = xor(gates, "a", "b");
            let nx = gates.not("a");

            // NOT(shift_in) built so the constant Zero bit-0 ties (see
            // `alu4_full` below) always lands on a >=2-input Nor/Or
            // gate: folding a lone-input Nor's only input away leaves
            // arity 0, which `specialise_module` refuses outright (a
            // hard-wired constant, unrealisable). Mixing in `zero` -- a
            // genuinely-computed always-0 signal, never a constant --
            // keeps the gate at arity >=1 after the fold.
            let zero = gates.and_reduce(vec!["a".to_string(), nx.clone()]);
            let not_shift_in = gates.nor(&["shift_in".to_string(), zero]);
            let not_sel6 = gates.not(&sel[6]);
            let shifted_term = gates.nor(&[not_shift_in, not_sel6]); // AND(shift_in, sel6)

            let picks = vec![
                gates.and_reduce(vec![and_, sel[0].clone()]),
                gates.and_reduce(vec![or_, sel[1].clone()]),
                gates.and_reduce(vec![xo, sel[2].clone()]),
                gates.and_reduce(vec![nx, sel[3].clone()]),
                gates.and_reduce(vec![sum.clone(), sel[4].clone()]),
                gates.and_reduce(vec![sum, sel[5].clone()]),
                shifted_term,
                gates.and_reduce(vec!["a".to_string(), sel[7].clone()]),
            ];
            let r = gates.or_reduce(picks);
            expose(gates, &r, "r");
        });
    }

    /// `extra_circuits::alu4_full`, split into a `slice` module (one ALU
    /// bit) instantiated four times plus `top`'s own glue: the 8-way
    /// opcode decode, the shared subtract signal, and the zero flag.
    /// Exercises `specialise_constants`: bit 0's `shift_in` is tied to
    /// `PortBinding::Zero` (there is no bit -1 to shift in from).
    pub(crate) fn alu4_full() -> HierarchicalNetlist {
        let mut builder = HierarchicalNetlistBuilder::new();
        slice_module(&mut builder);

        let mut inputs: Vec<String> = (0..4).map(|i| format!("a{i}")).collect();
        inputs.extend((0..4).map(|i| format!("b{i}")));
        inputs.extend(["s0".to_string(), "s1".to_string(), "s2".to_string()]);
        let outputs: Vec<String> = vec![
            "r0".into(),
            "r1".into(),
            "r2".into(),
            "r3".into(),
            "cout".into(),
            "zero".into(),
        ];
        let input_refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let output_refs: Vec<&str> = outputs.iter().map(String::as_str).collect();

        builder.module("top", &input_refs, &output_refs, |mb| {
            let (sel, sub) = {
                let gates = &mut mb.gates;
                let n0 = gates.not("s0");
                let n1 = gates.not("s1");
                let n2 = gates.not("s2");
                let s = ["s0", "s1", "s2"];
                let n = [n0, n1, n2];
                let mut sel = Vec::new();
                for op in 0..8u8 {
                    let bit = |k: usize| -> String {
                        if (op >> k) & 1 == 1 {
                            s[k].to_string()
                        } else {
                            n[k].clone()
                        }
                    };
                    sel.push(gates.and_reduce(vec![bit(0), bit(1), bit(2)]));
                }
                let sub = gates.and_reduce(vec!["s2".to_string(), n[1].clone(), "s0".to_string()]);
                (sel, sub)
            };

            for i in 0..4 {
                let a_i = format!("a{i}");
                let b_i = format!("b{i}");
                let cin_i = if i == 0 {
                    sub.clone()
                } else {
                    format!("c{}", i - 1)
                };
                let cout_i = if i == 3 {
                    "cout".to_string()
                } else {
                    format!("c{i}")
                };
                let r_i = format!("r{i}");
                let shift_in: PortBinding = if i == 0 {
                    PortBinding::Zero
                } else {
                    PortBinding::Signal(format!("a{}", i - 1))
                };
                let mut ports: Vec<(&str, PortBinding)> = vec![
                    ("a", PortBinding::Signal(a_i.clone())),
                    ("b", PortBinding::Signal(b_i.clone())),
                    ("sub", PortBinding::Signal(sub.clone())),
                    ("cin", PortBinding::Signal(cin_i.clone())),
                    ("shift_in", shift_in),
                    ("r", PortBinding::Signal(r_i.clone())),
                    ("cout", PortBinding::Signal(cout_i.clone())),
                ];
                let sel_names: Vec<String> = (0..8).map(|k| format!("sel{k}")).collect();
                for (k, name) in sel_names.iter().enumerate() {
                    ports.push((name.as_str(), PortBinding::Signal(sel[k].clone())));
                }
                mb.instance_with_constants(&format!("slice{i}"), "slice", &ports);
            }

            let gates = &mut mb.gates;
            let any = gates.or_reduce(vec![
                "r0".to_string(),
                "r1".to_string(),
                "r2".to_string(),
                "r3".to_string(),
            ]);
            let zero = gates.not(&any);
            expose(gates, &zero, "zero");
        });
        builder.finish("top")
    }

    /// One row of 4-bit ripple-carry summation for `multiplier4`: `x0..x3`
    /// (the shifted partial-sum accumulator from the previous row, or a
    /// row's own partial products for row 0) plus `y0..y3` (this row's
    /// partial products), `cin` tied 0 inside via an ordinary computed
    /// signal (never a constant -- see the module doc comment on
    /// avoiding `PortBinding` where a plain wire will do).
    fn adder_row_module(builder: &mut HierarchicalNetlistBuilder) {
        builder.module(
            "adder_row",
            &["x0", "x1", "x2", "x3", "y0", "y1", "y2", "y3"],
            &["s0", "s1", "s2", "s3", "cout"],
            |mb| {
                let zero = {
                    let gates = &mut mb.gates;
                    let nx0 = gates.not("x0");
                    gates.and_reduce(vec!["x0".to_string(), nx0])
                };
                mb.instance(
                    "fa0",
                    "full_adder",
                    &[
                        ("a", "x0"),
                        ("b", "y0"),
                        ("cin", &zero),
                        ("sum", "s0"),
                        ("cout", "c0"),
                    ],
                );
                mb.instance(
                    "fa1",
                    "full_adder",
                    &[
                        ("a", "x1"),
                        ("b", "y1"),
                        ("cin", "c0"),
                        ("sum", "s1"),
                        ("cout", "c1"),
                    ],
                );
                mb.instance(
                    "fa2",
                    "full_adder",
                    &[
                        ("a", "x2"),
                        ("b", "y2"),
                        ("cin", "c1"),
                        ("sum", "s2"),
                        ("cout", "c2"),
                    ],
                );
                mb.instance(
                    "fa3",
                    "full_adder",
                    &[
                        ("a", "x3"),
                        ("b", "y3"),
                        ("cin", "c2"),
                        ("sum", "s3"),
                        ("cout", "cout"),
                    ],
                );
            },
        );
    }

    /// `extra_circuits::multiplier4`, split into an `adder_row` module (one
    /// row of partial-product summation) instantiated three times (rows
    /// 1..3; row 0 is the raw partial products, no addition needed) plus
    /// `top`'s own glue computing the 16 partial products.
    pub(crate) fn multiplier4() -> HierarchicalNetlist {
        let mut builder = HierarchicalNetlistBuilder::new();
        full_adder_module(&mut builder);
        adder_row_module(&mut builder);

        let mut inputs: Vec<String> = (0..4).map(|i| format!("a{i}")).collect();
        inputs.extend((0..4).map(|i| format!("b{i}")));
        let outputs: Vec<String> = (0..8).map(|i| format!("o{i}")).collect();
        let input_refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let output_refs: Vec<&str> = outputs.iter().map(String::as_str).collect();

        builder.module("top", &input_refs, &output_refs, |mb| {
            let pp = |gates: &mut NetlistBuilder, i: usize, j: usize| {
                gates.and_reduce(vec![format!("a{i}"), format!("b{j}")])
            };
            let gates = &mut mb.gates;
            let pp00 = pp(gates, 0, 0);
            let pp10 = pp(gates, 1, 0);
            let pp20 = pp(gates, 2, 0);
            let pp30 = pp(gates, 3, 0);
            expose(gates, &pp00, "o0");

            let zero = {
                let na0 = gates.not("a0");
                gates.and_reduce(vec!["a0".to_string(), na0])
            };

            let pp01 = pp(gates, 0, 1);
            let pp11 = pp(gates, 1, 1);
            let pp21 = pp(gates, 2, 1);
            let pp31 = pp(gates, 3, 1);
            mb.instance(
                "row1",
                "adder_row",
                &[
                    ("x0", pp10.as_str()),
                    ("x1", pp20.as_str()),
                    ("x2", pp30.as_str()),
                    ("x3", zero.as_str()),
                    ("y0", pp01.as_str()),
                    ("y1", pp11.as_str()),
                    ("y2", pp21.as_str()),
                    ("y3", pp31.as_str()),
                    ("s0", "o1"),
                    ("s1", "row1_s1"),
                    ("s2", "row1_s2"),
                    ("s3", "row1_s3"),
                    ("cout", "row1_cout"),
                ],
            );

            let pp02 = pp(&mut mb.gates, 0, 2);
            let pp12 = pp(&mut mb.gates, 1, 2);
            let pp22 = pp(&mut mb.gates, 2, 2);
            let pp32 = pp(&mut mb.gates, 3, 2);
            mb.instance(
                "row2",
                "adder_row",
                &[
                    ("x0", "row1_s1"),
                    ("x1", "row1_s2"),
                    ("x2", "row1_s3"),
                    ("x3", "row1_cout"),
                    ("y0", pp02.as_str()),
                    ("y1", pp12.as_str()),
                    ("y2", pp22.as_str()),
                    ("y3", pp32.as_str()),
                    ("s0", "o2"),
                    ("s1", "row2_s1"),
                    ("s2", "row2_s2"),
                    ("s3", "row2_s3"),
                    ("cout", "row2_cout"),
                ],
            );

            let pp03 = pp(&mut mb.gates, 0, 3);
            let pp13 = pp(&mut mb.gates, 1, 3);
            let pp23 = pp(&mut mb.gates, 2, 3);
            let pp33 = pp(&mut mb.gates, 3, 3);
            mb.instance(
                "row3",
                "adder_row",
                &[
                    ("x0", "row2_s1"),
                    ("x1", "row2_s2"),
                    ("x2", "row2_s3"),
                    ("x3", "row2_cout"),
                    ("y0", pp03.as_str()),
                    ("y1", pp13.as_str()),
                    ("y2", pp23.as_str()),
                    ("y3", pp33.as_str()),
                    ("s0", "o3"),
                    ("s1", "o4"),
                    ("s2", "o5"),
                    ("s3", "o6"),
                    ("cout", "o7"),
                ],
            );
        });
        builder.finish("top")
    }

    /// One bit of `extra_circuits::alu4`'s 4-way opcode ALU (AND/OR/XOR/
    /// ADD, selected by `sel0..sel3`, pre-decoded by the enclosing `alu4`
    /// module -- distinct from `alu4_full`'s `slice` above, which lives in
    /// a separate `HierarchicalNetlist`).
    fn alu8_slice_module(builder: &mut HierarchicalNetlistBuilder) {
        builder.module(
            "slice",
            &["a", "b", "sel0", "sel1", "sel2", "sel3", "cin"],
            &["r", "cout"],
            |mb| {
                let gates = &mut mb.gates;
                let and_ = gates.and_reduce(vec!["a".to_string(), "b".to_string()]);
                let or_ = gates.or_reduce(vec!["a".to_string(), "b".to_string()]);
                let xo = xor(gates, "a", "b");
                let (sum, _cout) = full_adder_gates(gates, "a", "b", "cin", None, Some("cout"));
                let picks = vec![
                    gates.and_reduce(vec![and_, "sel0".to_string()]),
                    gates.and_reduce(vec![or_, "sel1".to_string()]),
                    gates.and_reduce(vec![xo, "sel2".to_string()]),
                    gates.and_reduce(vec![sum, "sel3".to_string()]),
                ];
                let r = gates.or_reduce(picks);
                expose(gates, &r, "r");
            },
        );
    }

    /// `extra_circuits::alu4`, split into `slice` (one bit) instantiated
    /// four times plus the 2-to-4 opcode decode as `alu4`'s own glue.
    fn alu4_module(builder: &mut HierarchicalNetlistBuilder) {
        alu8_slice_module(builder);
        let mut inputs: Vec<String> = (0..4).map(|i| format!("a{i}")).collect();
        inputs.extend((0..4).map(|i| format!("b{i}")));
        inputs.extend(["s1".to_string(), "s0".to_string(), "cin".to_string()]);
        let mut outputs: Vec<String> = (0..4).map(|i| format!("r{i}")).collect();
        outputs.push("cout".to_string());
        let input_refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let output_refs: Vec<&str> = outputs.iter().map(String::as_str).collect();

        builder.module("alu4", &input_refs, &output_refs, |mb| {
            let gates = &mut mb.gates;
            let ns0 = gates.not("s0");
            let ns1 = gates.not("s1");
            let sel = [
                gates.and_reduce(vec![ns1.clone(), ns0.clone()]),
                gates.and_reduce(vec![ns1.clone(), "s0".to_string()]),
                gates.and_reduce(vec!["s1".to_string(), ns0.clone()]),
                gates.and_reduce(vec!["s1".to_string(), "s0".to_string()]),
            ];
            for i in 0..4 {
                let a_i = format!("a{i}");
                let b_i = format!("b{i}");
                let cin_i = if i == 0 {
                    "cin".to_string()
                } else {
                    format!("c{}", i - 1)
                };
                let cout_i = if i == 3 {
                    "cout".to_string()
                } else {
                    format!("c{i}")
                };
                let r_i = format!("r{i}");
                mb.instance(
                    &format!("slice{i}"),
                    "slice",
                    &[
                        ("a", a_i.as_str()),
                        ("b", b_i.as_str()),
                        ("sel0", sel[0].as_str()),
                        ("sel1", sel[1].as_str()),
                        ("sel2", sel[2].as_str()),
                        ("sel3", sel[3].as_str()),
                        ("cin", cin_i.as_str()),
                        ("r", r_i.as_str()),
                        ("cout", cout_i.as_str()),
                    ],
                );
            }
        });
    }

    /// An 8-bit ALU: two `alu4` instances (`slice` inside `alu4` inside
    /// `top` -- three levels of nesting), the low nibble's `cout` chained
    /// into the high nibble's `cin`, opcode shared between both.
    pub(crate) fn alu8() -> HierarchicalNetlist {
        let mut builder = HierarchicalNetlistBuilder::new();
        alu4_module(&mut builder);

        let mut inputs: Vec<String> = (0..8).map(|i| format!("a{i}")).collect();
        inputs.extend((0..8).map(|i| format!("b{i}")));
        inputs.extend(["s1".to_string(), "s0".to_string(), "cin".to_string()]);
        let mut outputs: Vec<String> = (0..8).map(|i| format!("r{i}")).collect();
        outputs.push("cout".to_string());
        let input_refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let output_refs: Vec<&str> = outputs.iter().map(String::as_str).collect();

        builder.module("top", &input_refs, &output_refs, |mb| {
            mb.instance(
                "lo",
                "alu4",
                &[
                    ("a0", "a0"),
                    ("a1", "a1"),
                    ("a2", "a2"),
                    ("a3", "a3"),
                    ("b0", "b0"),
                    ("b1", "b1"),
                    ("b2", "b2"),
                    ("b3", "b3"),
                    ("s1", "s1"),
                    ("s0", "s0"),
                    ("cin", "cin"),
                    ("r0", "r0"),
                    ("r1", "r1"),
                    ("r2", "r2"),
                    ("r3", "r3"),
                    ("cout", "c4"),
                ],
            );
            mb.instance(
                "hi",
                "alu4",
                &[
                    ("a0", "a4"),
                    ("a1", "a5"),
                    ("a2", "a6"),
                    ("a3", "a7"),
                    ("b0", "b4"),
                    ("b1", "b5"),
                    ("b2", "b6"),
                    ("b3", "b7"),
                    ("s1", "s1"),
                    ("s0", "s0"),
                    ("cin", "c4"),
                    ("r0", "r4"),
                    ("r1", "r5"),
                    ("r2", "r6"),
                    ("r3", "r7"),
                    ("cout", "cout"),
                ],
            );
        });
        builder.finish("top")
    }
}

/// A tiny Boolean evaluator for a flat, combinational [`crate::compile::Netlist`].
///
/// Every existing test in this file (and in `compile::hierarchy`) only checks
/// *structure*: instance counts, `validate()`/`module_order()`/`flatten()`
/// succeeding, port counts, `combinational_order()` existing. None of that
/// proves a circuit computes what its name claims -- a swapped opcode index,
/// or row 2 reusing row 1's zero-tie, would pass every structural test here
/// silently. This evaluator exists so the `equivalence` tests below can
/// actually run the netlist and compare it, bit for bit, against an
/// independent reference.
///
/// Kept test-only and local to this file rather than promoted somewhere more
/// shareable: nothing outside this file's own tests needs to evaluate a
/// netlist yet, and putting a tiny, single-purpose evaluator next to its only
/// callers is easier to audit than a new shared module with one user. If a
/// later task wants netlist evaluation for its own tests, this is the
/// natural thing to lift into a shared `#[cfg(test)]` location (e.g.
/// `circuits::netlist_builder` or a new `compile::eval`) rather than
/// duplicating it.
#[cfg(test)]
pub(crate) mod eval {
    use std::collections::BTreeMap;

    use crate::compile::topology::GateKind;
    use crate::compile::Netlist;

    /// Evaluate `netlist` on a full assignment of its declared inputs
    /// (`assignment` must have an entry for every name in `netlist.inputs`),
    /// returning every input's and every gate's boolean value keyed by
    /// signal name.
    ///
    /// Walks gates in `netlist.combinational_order()` (a valid topological
    /// order, so every gate's inputs are already in `values` by the time it
    /// is evaluated). `NetlistBuilder`'s reduction helpers
    /// (`and_reduce`/`or_reduce`/`not`/`nor`) only ever produce two gate
    /// kinds (see `Gate::kind`'s doc comment): `GateKind::Nor` (true iff
    /// *no* input is true) and `GateKind::Or` (true iff *any* input is
    /// true). Any other kind means this evaluator does not know how to
    /// interpret the netlist, so it asserts rather than risking a silently
    /// wrong answer -- the entire point of this evaluator is to be a
    /// trustworthy oracle.
    pub(crate) fn evaluate(
        netlist: &Netlist,
        assignment: &BTreeMap<String, bool>,
    ) -> BTreeMap<String, bool> {
        let mut values: BTreeMap<String, bool> = BTreeMap::new();
        for name in &netlist.inputs {
            let value = *assignment
                .get(name)
                .unwrap_or_else(|| panic!("evaluate: no assignment given for input {name:?}"));
            values.insert(name.clone(), value);
        }

        let order = netlist
            .combinational_order()
            .expect("evaluate: netlist must be an acyclic combinational circuit");
        for index in order {
            let gate = &netlist.gates[index];
            let true_inputs = gate
                .inputs
                .iter()
                .filter(|input| {
                    *values.get(input.as_str()).unwrap_or_else(|| {
                        panic!(
                            "evaluate: signal {input:?} feeding gate {:?} was not yet defined \
                             -- combinational_order() should make this unreachable",
                            gate.name
                        )
                    })
                })
                .count();
            let value = match gate.kind {
                GateKind::Nor(_) => true_inputs == 0,
                GateKind::Or(_) => true_inputs > 0,
                other => panic!(
                    "evaluate: gate {:?} has kind {other:?} -- this evaluator only understands \
                     Nor/Or, the only two kinds NetlistBuilder's reduction helpers ever produce; \
                     extend it deliberately rather than guessing at a new kind's semantics",
                    gate.name
                ),
            };
            values.insert(gate.output.clone(), value);
        }
        values
    }
}

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
        assert_eq!(
            order.iter().position(|m| m == "slice").unwrap()
                < order.iter().position(|m| m == "alu4").unwrap(),
            true
        );
        let (flat, paths) = design.flatten().expect("flattens");
        assert!(
            paths.iter().any(|p| p.path.len() == 2),
            "slice inside alu4 inside top"
        );
        assert!(flat.combinational_order().is_some());
    }

    #[test]
    fn ripple_adder_validates_and_orders_children_before_parents() {
        let design = circuits::ripple_adder(4);
        design.validate().expect("validates");
        let order = design.module_order().expect("orders");
        assert_eq!(order, vec!["full_adder".to_string(), "top".to_string()]);
        let (flat, _) = design.flatten().expect("flattens");
        assert!(flat.combinational_order().is_some());
    }

    #[test]
    fn alu4_full_validates_orders_and_flattens_after_specialising_its_constant() {
        let design = circuits::alu4_full();
        design.validate().expect("validates");
        let order = design.module_order().expect("orders");
        assert_eq!(order.iter().position(|m| m == "slice").unwrap(), 0);
        assert_eq!(order.last().unwrap(), "top");

        // flatten refuses the raw `PortBinding::Zero` on bit 0's shift_in.
        assert!(
            design.flatten().is_err(),
            "flatten must refuse an unspecialised constant"
        );

        let specialised = design.specialise_constants().expect("specialises");
        let (flat, paths) = specialised.flatten().expect("flattens after specialising");
        assert!(paths.iter().any(|p| p.module.starts_with("slice")));
        assert!(flat.combinational_order().is_some());
    }

    #[test]
    fn multiplier4_validates_orders_and_flattens() {
        let design = circuits::multiplier4();
        design.validate().expect("validates");
        let order = design.module_order().expect("orders");
        assert!(
            order.iter().position(|m| m == "full_adder").unwrap()
                < order.iter().position(|m| m == "adder_row").unwrap()
        );
        assert!(
            order.iter().position(|m| m == "adder_row").unwrap()
                < order.iter().position(|m| m == "top").unwrap()
        );
        let (flat, paths) = design.flatten().expect("flattens");
        assert_eq!(flat.inputs.len(), 8);
        assert_eq!(flat.outputs.len(), 8);
        assert!(paths.iter().any(|p| p.module == "adder_row"));
        assert!(flat.combinational_order().is_some());
    }
}

/// Boolean-equivalence tests: proves each hierarchical circuit above
/// actually computes what its name claims, by evaluating it (via [`eval`])
/// against an independent reference and comparing every declared output --
/// not just checking that it validates, orders, and flattens (every existing
/// test above only checks that).
///
/// - `ripple_adder(4)`, `alu4_full()`, `multiplier4()` are compared
///   exhaustively against `fragment_synth::seed`'s own flat reference
///   circuits (widened to `pub(crate)` there for this -- see the
///   `pub(crate) mod tests` / `pub(crate) mod extra_circuits` comments in
///   `seed.rs`; their logic is untouched).
/// - `alu8()` has no flat counterpart in `seed.rs`, so it is checked against
///   8-bit arithmetic directly: fix the opcode to ADD and confirm
///   `r0..r7`/`cout` equal `(a + b + cin)`'s low 8 bits and its carry out,
///   over a deterministic sample.
/// - `ripple_adder(8)` is checked the same arithmetic way rather than
///   against its flat counterpart -- 17 inputs (2^17 cases) is too slow to
///   enumerate exhaustively in a debug build.
///
/// **Name matching.** Every *input* name here is literally the same string
/// on both the hierarchical and the flat side (`a0..`, `b0..`, `cin`,
/// `s0`/`s1`/`s2`). `alu4_full`'s flat inputs list the opcode as
/// `s2, s1, s0` while the hierarchical `top` lists `s0, s1, s2` -- but
/// since [`eval::evaluate`] takes a name-keyed assignment rather than a
/// positional one, that reordering does not matter: one assignment map,
/// built from either side's input names, drives both netlists identically.
/// *Output* names are never literally equal: the hierarchical side exposes
/// its declared port names (`s0`, `cout`, `r0`, ...) while the flat side's
/// outputs are `NetlistBuilder`-generated gate names (`g12`, ...). Both
/// sides build their `outputs` vector in the same semantic order (bit 0's
/// result, bit 1's, ..., the final carry -- confirmed by reading each
/// circuit's construction in `hierarchical_builder.rs` and `seed.rs`), so
/// outputs are matched **by position**.
#[cfg(test)]
mod equivalence {
    use std::collections::BTreeMap;

    use super::circuits;
    use super::eval::evaluate;
    use crate::compile::fragment_synth::seed::tests::extra_circuits as flat;
    use crate::compile::Netlist;

    /// Every assignment of `names` to booleans -- `names[bit]` is bit `bit`
    /// of a `0..2^names.len()` counter. A complete, deterministic
    /// enumeration (no randomness).
    fn exhaustive(names: &[String]) -> Vec<BTreeMap<String, bool>> {
        let total = 1usize << names.len();
        (0..total)
            .map(|mask| {
                names
                    .iter()
                    .enumerate()
                    .map(|(bit, name)| (name.clone(), (mask >> bit) & 1 == 1))
                    .collect()
            })
            .collect()
    }

    /// `count` assignments of `names`, drawn from the full `2^names.len()`
    /// space by a fixed odd-multiplier stride (`index * ODD_STEP mod
    /// space`) instead of exhaustively or randomly: an odd multiplier times
    /// a power-of-two modulus is a bijection over that range, so this is a
    /// deterministic, reproducible scatter across the whole input space --
    /// the same sequence every run, never a seeded/random number generator.
    fn deterministic_sample(names: &[String], count: usize) -> Vec<BTreeMap<String, bool>> {
        const ODD_STEP: usize = 0x9E37_79B1;
        let space = 1usize << names.len();
        let count = count.min(space);
        (0..count)
            .map(|i| {
                let mask = i.wrapping_mul(ODD_STEP) & (space - 1);
                names
                    .iter()
                    .enumerate()
                    .map(|(bit, name)| (name.clone(), (mask >> bit) & 1 == 1))
                    .collect()
            })
            .collect()
    }

    /// Evaluate both netlists on `assignment` and assert every declared
    /// output agrees, matched by position (see the module doc comment).
    fn assert_same_outputs(
        case: usize,
        assignment: &BTreeMap<String, bool>,
        hier: &Netlist,
        flat: &Netlist,
    ) {
        assert_eq!(
            hier.outputs.len(),
            flat.outputs.len(),
            "case {case}: output counts differ"
        );
        let hier_values = evaluate(hier, assignment);
        let flat_values = evaluate(flat, assignment);
        for (position, (hier_name, flat_name)) in
            hier.outputs.iter().zip(flat.outputs.iter()).enumerate()
        {
            let hier_bit = hier_values[hier_name];
            let flat_bit = flat_values[flat_name];
            assert_eq!(
                hier_bit, flat_bit,
                "case {case} ({assignment:?}): output #{position} disagrees -- hierarchical \
                 {hier_name:?}={hier_bit}, flat {flat_name:?}={flat_bit}"
            );
        }
    }

    #[test]
    fn ripple_adder4_matches_the_flat_builder_exhaustively() {
        let design = circuits::ripple_adder(4);
        let (hier, _) = design.flatten().expect("flattens");
        let flat_netlist = flat::ripple_adder(4);
        assert_eq!(hier.inputs.len(), 9, "ripple_adder(4) should have 9 inputs");

        for (case, assignment) in exhaustive(&hier.inputs).into_iter().enumerate() {
            assert_same_outputs(case, &assignment, &hier, &flat_netlist);
        }
    }

    #[test]
    fn alu4_full_matches_the_flat_builder_exhaustively() {
        let design = circuits::alu4_full()
            .specialise_constants()
            .expect("specialises");
        let (hier, _) = design.flatten().expect("flattens after specialising");
        let flat_netlist = flat::alu4_full();
        assert_eq!(hier.inputs.len(), 11, "alu4_full() should have 11 inputs");

        // Input names are the same *set* on both sides (see the module doc
        // comment) even though the two lists order `s0`/`s1`/`s2`
        // differently -- `hier.inputs` alone is enough to build every
        // assignment, since `evaluate` looks values up by name.
        for (case, assignment) in exhaustive(&hier.inputs).into_iter().enumerate() {
            assert_same_outputs(case, &assignment, &hier, &flat_netlist);
        }
    }

    #[test]
    fn multiplier4_matches_the_flat_builder_exhaustively() {
        let design = circuits::multiplier4();
        let (hier, _) = design.flatten().expect("flattens");
        let flat_netlist = flat::multiplier4();
        assert_eq!(hier.inputs.len(), 8, "multiplier4() should have 8 inputs");

        for (case, assignment) in exhaustive(&hier.inputs).into_iter().enumerate() {
            assert_same_outputs(case, &assignment, &hier, &flat_netlist);
        }
    }

    /// Reads an 8-bit little-endian value (`{prefix}0` is bit 0) out of an
    /// `evaluate()` result.
    fn read_u8(values: &BTreeMap<String, bool>, prefix: &str) -> u32 {
        (0..8).fold(0u32, |acc, bit| {
            acc | ((values[&format!("{prefix}{bit}")] as u32) << bit)
        })
    }

    /// The 17 non-opcode inputs `alu8`/`ripple_adder(8)` share: two 8-bit
    /// operands plus a carry-in.
    fn operand_and_carry_names() -> Vec<String> {
        (0..8)
            .map(|i| format!("a{i}"))
            .chain((0..8).map(|i| format!("b{i}")))
            .chain(std::iter::once("cin".to_string()))
            .collect()
    }

    #[test]
    fn alu8_add_opcode_matches_8_bit_arithmetic_over_a_deterministic_sample() {
        let design = circuits::alu8();
        let (hier, _) = design
            .flatten()
            .expect("flattens (alu8 has no constants to specialise)");

        let names = operand_and_carry_names();
        assert_eq!(names.len(), 17);

        for (case, mut assignment) in deterministic_sample(&names, 256).into_iter().enumerate() {
            // ADD is opcode s1=1, s0=1 -- see `extra_circuits::alu4`'s
            // `sel[3]` and `hierarchical_builder`'s `alu4_module`, which
            // decode the same way.
            assignment.insert("s1".to_string(), true);
            assignment.insert("s0".to_string(), true);

            let values = evaluate(&hier, &assignment);
            let a = read_u8(&values, "a");
            let b = read_u8(&values, "b");
            let cin = values["cin"] as u32;
            let sum = a + b + cin;
            let expected_r = sum & 0xFF;
            let expected_cout = (sum >> 8) & 1 == 1;

            let actual_r = read_u8(&values, "r");
            let actual_cout = values["cout"];
            assert_eq!(
                actual_r, expected_r,
                "case {case}: a={a} b={b} cin={cin} -> r={actual_r:#04x}, expected {expected_r:#04x}"
            );
            assert_eq!(
                actual_cout, expected_cout,
                "case {case}: a={a} b={b} cin={cin} -> cout={actual_cout}, expected {expected_cout}"
            );
        }
    }

    #[test]
    fn ripple_adder8_matches_8_bit_arithmetic_over_a_deterministic_sample() {
        let design = circuits::ripple_adder(8);
        let (hier, _) = design.flatten().expect("flattens");

        let names = operand_and_carry_names();
        assert_eq!(names.len(), 17);

        for (case, assignment) in deterministic_sample(&names, 256).into_iter().enumerate() {
            let values = evaluate(&hier, &assignment);
            let a = read_u8(&values, "a");
            let b = read_u8(&values, "b");
            let cin = values["cin"] as u32;
            let sum = a + b + cin;
            let expected_s = sum & 0xFF;
            let expected_cout = (sum >> 8) & 1 == 1;

            let actual_s = read_u8(&values, "s");
            let actual_cout = values["cout"];
            assert_eq!(
                actual_s, expected_s,
                "case {case}: a={a} b={b} cin={cin} -> s={actual_s:#04x}, expected {expected_s:#04x}"
            );
            assert_eq!(
                actual_cout, expected_cout,
                "case {case}: a={a} b={b} cin={cin} -> cout={actual_cout}, expected {expected_cout}"
            );
        }
    }
}

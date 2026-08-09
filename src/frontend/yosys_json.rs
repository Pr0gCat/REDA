//! Read Yosys's `write_json` output -- a **gate-level** netlist in Yosys's
//! own simple-cell vocabulary (`$_AND_`, `$_NAND_`, `$_XOR_`, `$_MUX_`,
//! ...) -- into a [`Netlist`].
//!
//! This module used to read a netlist Yosys had already technology-mapped
//! onto `redstone_nor.genlib`'s NOR and OR cells, and it built the redstone
//! realisation of each one as it went. Both halves of that were wrong in
//! the same way: ABC chose the realisation (it has no idea what redstone
//! costs), and this reader hard-coded what a mapped cell became. What
//! arrives here now is the gate level as Yosys left it, one [`Netlist`]
//! gate per Yosys cell, and `compile::lowering` -- consulting
//! `compile::topology`'s expansions -- is what turns each into torches and
//! merges.
//!
//! Yosys's JSON schema (see its own `docs/source/cmd/write_json.rst`) is
//! deliberately general: nets are just integers ("bits"), a constant is a
//! `"0"`/`"1"`/`"x"`/`"z"` string in the same array a real net id would sit
//! in, and cells reference nets by name-keyed connection lists. This module
//! only has to handle the slice of that schema `abc`'s gate mapping can
//! actually produce -- but within that slice, every construct is either
//! handled correctly or rejected with a specific error. Nothing is silently
//! dropped: a dropped cell is a netlist that still compiles, to the wrong
//! circuit.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::circuits::netlist_builder::NetlistBuilder;
use crate::compile::topology::{self, GateKind};
use crate::compile::Netlist;

use super::FrontendError;

/// Yosys's own pin names for each simple cell type this frontend accepts,
/// in declaration order (see Yosys's `techlibs/common/simcells.v`). The
/// output pin is always `Y` and is not listed.
///
/// This is a JSON-reading fact -- which key names to look up in a cell's
/// `connections` object -- not a redstone one, so it lives here rather than
/// in `topology`, which has no reason to know Yosys's pin names. The two
/// tables are held to covering the same set of cell types by
/// `every_known_cell_type_has_pin_names`.
///
/// Note `$_MUX_`/`$_NMUX_`: their third pin is `S`, not `C`, and the order
/// `A, B, S` is what `GateKind::Mux`'s `Y = S ? B : A` is written against.
const CELL_PINS: &[(&str, &[&str])] = &[
    ("$_NOT_", &["A"]),
    ("$_BUF_", &["A"]),
    ("$_AND_", &["A", "B"]),
    ("$_NAND_", &["A", "B"]),
    ("$_OR_", &["A", "B"]),
    ("$_NOR_", &["A", "B"]),
    ("$_XOR_", &["A", "B"]),
    ("$_XNOR_", &["A", "B"]),
    ("$_ANDNOT_", &["A", "B"]),
    ("$_ORNOT_", &["A", "B"]),
    ("$_AOI3_", &["A", "B", "C"]),
    ("$_OAI3_", &["A", "B", "C"]),
    ("$_AOI4_", &["A", "B", "C", "D"]),
    ("$_OAI4_", &["A", "B", "C", "D"]),
    ("$_MUX_", &["A", "B", "S"]),
    ("$_NMUX_", &["A", "B", "S"]),
];

fn pins_for(cell_type: &str) -> Option<&'static [&'static str]> {
    CELL_PINS.iter().find(|&&(name, _)| name == cell_type).map(|&(_, pins)| pins)
}

fn unsupported(message: impl Into<String>) -> FrontendError {
    FrontendError::Unsupported(message.into())
}

/// One bit of a Yosys "bits" array: either a real net id, or a literal
/// constant. Nothing in this project can realize a hard-wired driver of any
/// kind (there is no "always on" or "always off" cell in real redstone), so
/// every constant this reader meets is a candidate for either folding away
/// (a `0` feeding a NOR or OR pin, which changes nothing) or a hard error
/// (any other use).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bit {
    Net(i64),
    Zero,
    One,
}

fn parse_bit(value: &Value) -> Result<Bit, FrontendError> {
    match value {
        Value::Number(n) => {
            let id = n.as_i64().ok_or_else(|| unsupported(format!("net id `{n}` is not an integer")))?;
            Ok(Bit::Net(id))
        }
        Value::String(s) => match s.as_str() {
            "0" => Ok(Bit::Zero),
            "1" => Ok(Bit::One),
            other => Err(unsupported(format!(
                "constant bit `{other}` -- there is no way to drive a hard-wired x/z value onto \
                 a real net in redstone"
            ))),
        },
        other => Err(unsupported(format!("unexpected bit value in yosys JSON: {other}"))),
    }
}

fn as_object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, FrontendError> {
    value.as_object().ok_or_else(|| unsupported(format!("expected {what} to be a JSON object")))
}

fn as_array<'a>(value: &'a Value, what: &str) -> Result<&'a Vec<Value>, FrontendError> {
    value.as_array().ok_or_else(|| unsupported(format!("expected {what} to be a JSON array")))
}

fn as_str<'a>(value: &'a Value, what: &str) -> Result<&'a str, FrontendError> {
    value.as_str().ok_or_else(|| unsupported(format!("expected {what} to be a JSON string")))
}

/// The single bit on `pin` of a cell's `connections` object. Every Yosys
/// simple cell is scalar -- one bit per pin, by definition of the
/// `$_*_` family -- so a pin that is not exactly 1 bit wide means Yosys
/// emitted something this frontend does not expect (a word-level `$and`,
/// say, which means `techmap` did not run).
fn single_bit<'a>(connections: &'a Map<String, Value>, pin: &str, cell_name: &str) -> Result<&'a Value, FrontendError> {
    let bits = connections
        .get(pin)
        .ok_or_else(|| unsupported(format!("cell `{cell_name}` has no `{pin}` connection")))?;
    let bits = as_array(bits, &format!("cell `{cell_name}` pin `{pin}`"))?;
    if bits.len() != 1 {
        return Err(unsupported(format!(
            "cell `{cell_name}` pin `{pin}` is {}-bit; every Yosys `$_*_` simple cell is scalar",
            bits.len()
        )));
    }
    Ok(&bits[0])
}

/// One cell as extracted from the JSON, kept around only long enough for
/// `Context::resolve` to build it on demand (see that function for why this
/// has to be demand-driven rather than a single top-to-bottom pass).
struct CellInfo<'a> {
    name: String,
    cell_type: &'a str,
    connections: &'a Map<String, Value>,
}

struct Context<'a> {
    /// net id -> the cell whose `Y` output drives it. Built once, up front,
    /// from every cell in the module -- this is the reverse index that lets
    /// `resolve` find a net's driver regardless of which order Yosys listed
    /// cells in.
    driver_of: HashMap<i64, CellInfo<'a>>,
    /// net id -> already-resolved signal name. Seeded with every primary
    /// input before any cell is processed; filled in with gate outputs (and
    /// the primary-input names themselves) as `resolve` walks the design.
    signal_of: HashMap<i64, String>,
    /// Net ids currently being resolved, to catch a combinational cycle
    /// instead of recursing forever.
    in_progress: HashSet<i64>,
    /// The exact set of primary input names -- needed to tell a genuine
    /// gate-driven output from a direct `assign out = in;` passthrough,
    /// which needs a synthesized buffer (see `resolve_output`).
    input_names: HashSet<String>,
    /// Dedups the 2-inverter buffer synthesized for a direct
    /// input-to-output assignment, keyed by the input's net id, so two
    /// output ports both directly wired to the same input share one buffer
    /// instead of each building their own.
    buffer_of_input: HashMap<i64, String>,
    builder: NetlistBuilder,
}

impl<'a> Context<'a> {
    /// The signal name driving `net_id`, building whatever gate(s) that
    /// takes along the way. Memoized in `signal_of`, and safe to call in any
    /// order regardless of how Yosys happened to list cells in the JSON --
    /// this recurses into a cell's own inputs before building the cell
    /// itself, so it always produces inputs before the gates that consume
    /// them no matter what the JSON's own iteration order is.
    fn resolve(&mut self, net_id: i64) -> Result<String, FrontendError> {
        if let Some(name) = self.signal_of.get(&net_id) {
            return Ok(name.clone());
        }
        if !self.in_progress.insert(net_id) {
            return Err(unsupported(format!(
                "combinational cycle through net {net_id} -- this frontend only supports \
                 combinational logic, and yosys should never emit a cycle for it"
            )));
        }

        let cell = self
            .driver_of
            .remove(&net_id)
            .ok_or_else(|| unsupported(format!("net {net_id} is driven by neither a primary input nor a cell")))?;
        let name = self.build_cell(&cell)?;

        self.in_progress.remove(&net_id);
        self.signal_of.insert(net_id, name.clone());
        Ok(name)
    }

    /// Resolve a pin's bit to a real input signal, or `None` if it was a
    /// constant 0. A constant 1 is always a hard error: nothing in this
    /// project can drive a hard-wired signal, so a gate forced permanently
    /// high or low has no realisation.
    ///
    /// A constant 0 is only foldable on the two kinds where 0 is the
    /// neutral element and the arity is free to shrink -- `NOR(x, 0)` is
    /// `NOR(x)`, `OR(x, 0)` is `OR(x)`. Every other kind is fixed-arity and
    /// its recipe is written against all of its pins, so [`Context::inputs_of`]
    /// rejects a constant there by name rather than guessing which
    /// simplification Yosys meant. In practice `opt` has already removed
    /// them; this is the path that says so out loud if it ever has not.
    fn resolve_input_pin(&mut self, connections: &Map<String, Value>, pin: &str, cell_name: &str) -> Result<Option<String>, FrontendError> {
        let bit = single_bit(connections, pin, cell_name)?;
        match parse_bit(bit)? {
            Bit::Net(id) => Ok(Some(self.resolve(id)?)),
            Bit::Zero => Ok(None),
            Bit::One => Err(unsupported(format!(
                "cell `{cell_name}` pin `{pin}` is tied to constant 1 -- there is no tie-off cell \
                 in real redstone, so a gate with a hard-wired input has no realization here"
            ))),
        }
    }

    /// Every pin of `cell`, resolved, in the cell type's own declaration
    /// order. `None` entries are constant-0 pins, left in place so the
    /// caller can decide whether folding one is sound for its kind.
    fn inputs_of(&mut self, cell: &CellInfo<'a>, pins: &[&str]) -> Result<Vec<Option<String>>, FrontendError> {
        let mut resolved = Vec::with_capacity(pins.len());
        for &pin in pins {
            resolved.push(self.resolve_input_pin(cell.connections, pin, &cell.name)?);
        }
        Ok(resolved)
    }

    /// Look `cell`'s type up in `topology::gate_kind_for_yosys_cell` and
    /// build one netlist gate of whichever `GateKind` it names -- the
    /// boundary this reader crosses instead of matching a cell's type string
    /// and separately knowing what to build for it. How that kind becomes
    /// redstone is `compile::topology`'s decision, applied later by
    /// `compile::lowering`; nothing is realised here.
    ///
    /// A cell type the table has never heard of is a hard, loud error naming
    /// the cell and its type: a dropped cell is a netlist that still
    /// compiles, to the wrong circuit.
    fn build_cell(&mut self, cell: &CellInfo<'a>) -> Result<String, FrontendError> {
        let Some(kind) = topology::gate_kind_for_yosys_cell(cell.cell_type) else {
            let known: Vec<&str> = topology::known_yosys_cell_types().map(|(name, _)| name).collect();
            return Err(unsupported(format!(
                "cell `{}` has type `{}`, which this project's topology library has no realization \
                 for. Supported: {known:?}. (A `$__ZERO`/`$__ONE` drives a hard-wired constant and \
                 a `$_DFF_*_` holds state -- neither has any realization in real redstone. A \
                 word-level type such as `$and` means `techmap` did not run.)",
                cell.name, cell.cell_type
            )));
        };
        let pins = pins_for(cell.cell_type)
            .unwrap_or_else(|| panic!("`{}` has a GateKind but no pin names -- CELL_PINS is out of step", cell.cell_type));
        let resolved = self.inputs_of(cell, pins)?;

        match kind {
            // The two realisable kinds are also the two whose arity is free
            // to shrink: a constant-0 pin is the neutral element of both,
            // so it simply drops out and a smaller real gate is built.
            GateKind::Nor(_) => {
                let inputs: Vec<String> = resolved.into_iter().flatten().collect();
                if inputs.is_empty() {
                    // Every pin was tied to 0. `NOR()` of nothing is a
                    // hard-wired 1, which nothing here can drive.
                    return Err(unsupported(format!(
                        "cell `{}` has no real inputs left after folding constant-0 pins; a \
                         hard-wired-1 output has no realization in redstone",
                        cell.name
                    )));
                }
                Ok(self.builder.nor(&inputs))
            }
            GateKind::Or(_) => {
                let inputs: Vec<String> = resolved.into_iter().flatten().collect();
                match inputs.len() {
                    0 => Err(unsupported(format!(
                        "cell `{}` has no real inputs left after folding constant-0 pins; a \
                         hard-wired-0 output has no realization in redstone",
                        cell.name
                    ))),
                    // `OR(x) == x` is a bare wire, not a gate of any kind --
                    // this cell's net becomes a plain alias for its one
                    // surviving input rather than a new gate.
                    1 => Ok(inputs.into_iter().next().expect("checked: exactly one input")),
                    _ => Ok(self.builder.merge(&inputs)),
                }
            }
            // Every gate-level kind is fixed-arity, and its expansion is
            // written against all of its pins.
            _ => {
                let mut inputs = Vec::with_capacity(resolved.len());
                for (pin, value) in pins.iter().zip(resolved) {
                    inputs.push(value.ok_or_else(|| {
                        unsupported(format!(
                            "cell `{}` ({}) has pin `{pin}` tied to constant 0. Folding a constant \
                             into a {kind:?} would mean guessing which smaller gate Yosys meant, \
                             and guessing wrong is a netlist that still compiles to the wrong \
                             circuit -- run `opt` before `write_json` instead",
                            cell.name, cell.cell_type
                        ))
                    })?);
                }
                Ok(self.builder.cell(kind, &inputs))
            }
        }
    }

    /// A direct `assign out = in;` at the top level is a pass-through with
    /// no cell in between at all, but `compile` requires every declared
    /// output to be driven by a real gate. `GateKind::Buf` is exactly that
    /// gate -- and, unlike before, this frontend does not decide what it
    /// becomes: `topology::expansion_for(Buf)` says two chained inverters,
    /// because there is no wire-only primitive in redstone and every real
    /// signal path here ends at a torch.
    fn synthesize_buffer(&mut self, input: &str) -> String {
        self.builder.cell(GateKind::Buf, &[input.to_string()])
    }
}

/// Convert Yosys's JSON (as produced by `write_json` after `abc`, see
/// `synth.py`) into a gate-level [`Netlist`] for `top_module`.
///
/// Returns the netlist together with a lookup from each declared output
/// port's name (bit-indexed as `"name[i]"` for a multi-bit port) to that
/// output's actual signal name in `netlist.outputs`.
pub(super) fn netlist_from_json(json: &Value, top_module: &str) -> Result<(Netlist, HashMap<String, String>), FrontendError> {
    let modules = as_object(json.get("modules").ok_or_else(|| unsupported("yosys JSON has no `modules` key"))?, "`modules`")?;

    let module = modules.get(top_module).ok_or_else(|| {
        let available: Vec<&str> = modules.keys().map(String::as_str).collect();
        unsupported(format!("yosys JSON has no module named `{top_module}`; found: {available:?}"))
    })?;

    let ports = as_object(module.get("ports").ok_or_else(|| unsupported("module has no `ports` key"))?, "`ports`")?;
    let cells = as_object(module.get("cells").ok_or_else(|| unsupported("module has no `cells` key"))?, "`cells`")?;

    // One bit-name per bit of a port: bare `name` for a 1-bit port (every
    // port in this project's own reference circuits), `name[i]` for a wider
    // one (LSB-first, matching Yosys's own "bits" convention) so this
    // frontend is not silently wrong the first time someone hands it a
    // multi-bit port.
    fn bit_names(port_name: &str, bits: &[Value]) -> Vec<String> {
        if bits.len() == 1 {
            vec![port_name.to_string()]
        } else {
            (0..bits.len()).map(|i| format!("{port_name}[{i}]")).collect()
        }
    }

    let mut inputs: Vec<String> = Vec::new();
    let mut signal_of: HashMap<i64, String> = HashMap::new();
    let mut input_names: HashSet<String> = HashSet::new();

    for (port_name, port) in ports {
        let direction = as_str(port.get("direction").ok_or_else(|| unsupported(format!("port `{port_name}` has no `direction`")))?, "port direction")?;
        if direction != "input" {
            continue;
        }
        let bits = as_array(port.get("bits").ok_or_else(|| unsupported(format!("port `{port_name}` has no `bits`")))?, "port bits")?;
        for (name, bit) in bit_names(port_name, bits).into_iter().zip(bits.iter()) {
            match parse_bit(bit)? {
                Bit::Net(id) => {
                    signal_of.insert(id, name.clone());
                    input_names.insert(name.clone());
                    inputs.push(name);
                }
                Bit::Zero | Bit::One => {
                    return Err(unsupported(format!("input port `{port_name}` has a constant bit, which yosys should never emit for a real input")));
                }
            }
        }
    }

    // Build the net-id -> driving-cell reverse index up front. Cells in
    // Yosys's JSON are a JSON object -- serde_json orders that
    // alphabetically by key, not topologically -- so this index (not
    // iteration order) is what lets `Context::resolve` find a cell's driver
    // regardless of how Yosys happened to name it.
    let mut driver_of: HashMap<i64, CellInfo> = HashMap::new();
    for (cell_name, cell) in cells {
        let cell_type = as_str(cell.get("type").ok_or_else(|| unsupported(format!("cell `{cell_name}` has no `type`")))?, "cell type")?;
        let connections = as_object(cell.get("connections").ok_or_else(|| unsupported(format!("cell `{cell_name}` has no `connections`")))?, "cell connections")?;
        let out_bit = single_bit(connections, "Y", cell_name)?;
        match parse_bit(out_bit)? {
            Bit::Net(id) => {
                driver_of.insert(id, CellInfo { name: cell_name.clone(), cell_type, connections });
            }
            Bit::Zero | Bit::One => {
                return Err(unsupported(format!("cell `{cell_name}` ({cell_type}) has a constant output, which is never something ABC needs written to a real net")));
            }
        }
    }

    let mut ctx = Context {
        driver_of,
        signal_of,
        in_progress: HashSet::new(),
        input_names,
        buffer_of_input: HashMap::new(),
        builder: NetlistBuilder::new(),
    };

    let mut outputs: Vec<String> = Vec::new();
    let mut seen_outputs: HashSet<String> = HashSet::new();
    let mut port_map: HashMap<String, String> = HashMap::new();

    for (port_name, port) in ports {
        let direction = as_str(port.get("direction").ok_or_else(|| unsupported(format!("port `{port_name}` has no `direction`")))?, "port direction")?;
        if direction != "output" {
            continue;
        }
        let bits = as_array(port.get("bits").ok_or_else(|| unsupported(format!("port `{port_name}` has no `bits`")))?, "port bits")?;
        for (name, bit) in bit_names(port_name, bits).into_iter().zip(bits.iter()) {
            let signal = match parse_bit(bit)? {
                Bit::Net(id) => resolve_output_net(&mut ctx, id)?,
                Bit::Zero | Bit::One => {
                    return Err(unsupported(format!("output port `{port_name}` is tied to a constant -- there is no hard-wired driver in real redstone")));
                }
            };
            if seen_outputs.insert(signal.clone()) {
                outputs.push(signal.clone());
            }
            port_map.insert(name, signal);
        }
    }

    if !ctx.driver_of.is_empty() {
        // Every cell that wasn't already consumed while resolving the
        // outputs above is genuinely dead: `opt_clean -purge` (see
        // synth.py) should already have removed dead logic, so this
        // signals a real gap in this reader's assumptions rather than
        // ordinary leftover synthesis debris.
        let leftover: Vec<&str> = ctx.driver_of.values().map(|c| c.name.as_str()).collect();
        return Err(unsupported(format!("{} cell(s) are never used to drive any output: {leftover:?}", leftover.len())));
    }

    let netlist = Netlist { inputs, outputs, gates: ctx.builder.into_gates() };
    Ok((netlist, port_map))
}

/// Resolve an output port's net to a signal name that is guaranteed to be
/// some gate's output -- never a bare primary input name.
///
/// `compile()` requires every declared [`Netlist`] output to be driven by a
/// real gate (`compile::compile`'s output-lamp loop looks up
/// `netlist.gates.iter().position(|gate| &gate.output == output_name)` and
/// panics if it does not find one), so a direct `assign out = in;` --
/// which Yosys is free to leave as a bare port-to-port net alias with no
/// cell in between at all -- needs a real gate synthesized for it here, the
/// same 2-inverter buffer `Context::synthesize_buffer` builds for an
/// explicit `BUF` cell.
fn resolve_output_net(ctx: &mut Context<'_>, net_id: i64) -> Result<String, FrontendError> {
    if ctx.driver_of.contains_key(&net_id) {
        return ctx.resolve(net_id);
    }
    if let Some(name) = ctx.signal_of.get(&net_id).cloned() {
        if ctx.input_names.contains(&name) {
            if let Some(buffered) = ctx.buffer_of_input.get(&net_id) {
                return Ok(buffered.clone());
            }
            let buffered = ctx.synthesize_buffer(&name);
            ctx.buffer_of_input.insert(net_id, buffered.clone());
            return Ok(buffered);
        }
        // Already resolved to a gate output by an earlier output port
        // sharing the same net (fan-out to two output ports).
        return Ok(name);
    }
    Err(unsupported(format!("output net {net_id} is driven by neither a primary input nor a cell")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Both tables that key off a Yosys cell type -- `topology`'s
    /// type-to-kind map and this module's type-to-pin-names map -- have to
    /// cover exactly the same set, or a cell would resolve to a `GateKind`
    /// and then have no pins to read (or the reverse).
    #[test]
    fn every_known_cell_type_has_pin_names_and_vice_versa() {
        for (cell_type, kind) in topology::known_yosys_cell_types() {
            let pins = pins_for(cell_type).unwrap_or_else(|| panic!("{cell_type} has no CELL_PINS entry"));
            assert!(!pins.contains(&"Y"), "{cell_type}: `Y` is the output, not an input pin");
            if let Some(fixed) = kind.fixed_arity() {
                assert_eq!(pins.len(), fixed, "{cell_type}: {kind:?} takes {fixed} input(s)");
            } else {
                // `Nor`/`Or` carry a declared arity that constant folding is
                // free to shrink; the pin list is the *declared* one.
                assert_eq!(pins.len(), kind.arity(), "{cell_type}");
            }
        }
        for &(cell_type, _) in CELL_PINS {
            assert!(
                topology::gate_kind_for_yosys_cell(cell_type).is_some(),
                "{cell_type} has pin names but no GateKind"
            );
        }
    }

    /// A `$_BUF_` cell becomes exactly one `GateKind::Buf` gate -- not the
    /// two NOR gates it will eventually be realised as. That decision moved
    /// to `topology::expansion_for`, which is the whole point: this reader
    /// reports what Yosys said, and nothing more.
    #[test]
    fn a_buf_cell_becomes_one_buf_gate_not_a_realisation_of_one() {
        let json = json!({
            "modules": {
                "top": {
                    "ports": {
                        "a": { "direction": "input", "bits": [2] },
                        "y": { "direction": "output", "bits": [3] }
                    },
                    "cells": {
                        "buf0": {
                            "type": "$_BUF_",
                            "connections": { "A": [2], "Y": [3] }
                        }
                    }
                }
            }
        });

        let (netlist, port_map) = netlist_from_json(&json, "top").expect("a lone $_BUF_ cell must synthesize");

        assert_eq!(netlist.inputs, vec!["a".to_string()]);
        assert_eq!(netlist.gates.len(), 1, "one Yosys cell is one netlist gate");
        assert_eq!(netlist.gates[0].kind, GateKind::Buf);
        assert_eq!(netlist.gates[0].inputs, vec!["a".to_string()]);
        assert_eq!(port_map["y"], netlist.gates[0].output);

        // ...and lowering is what makes it two chained inverters.
        let lowered = crate::compile::lowering::lower(&netlist).expect("lowers");
        assert_eq!(lowered.gates.len(), 2, "NOT(NOT(x)) == x: two real torches");
        assert!(lowered.gates.iter().all(|g| g.kind == GateKind::Nor(1)));
    }

    /// A gate-level cell keeps its kind and its pin order verbatim.
    #[test]
    fn a_mux_cell_keeps_its_kind_and_its_a_b_s_pin_order() {
        let json = json!({
            "modules": {
                "top": {
                    "ports": {
                        "a": { "direction": "input", "bits": [2] },
                        "b": { "direction": "input", "bits": [3] },
                        "s": { "direction": "input", "bits": [4] },
                        "y": { "direction": "output", "bits": [5] }
                    },
                    "cells": {
                        "mux0": {
                            "type": "$_MUX_",
                            "connections": { "A": [2], "B": [3], "S": [4], "Y": [5] }
                        }
                    }
                }
            }
        });

        let (netlist, _) = netlist_from_json(&json, "top").expect("a lone $_MUX_ cell must synthesize");
        assert_eq!(netlist.gates.len(), 1);
        assert_eq!(netlist.gates[0].kind, GateKind::Mux);
        assert_eq!(
            netlist.gates[0].inputs,
            vec!["a".to_string(), "b".to_string(), "s".to_string()],
            "pin order is A, B, S -- `Y = S ? B : A` is written against it"
        );
    }

    /// Constant-0 folding can leave a `$_NOR_` cell with fewer real inputs
    /// than its declared arity: 0 is the neutral element of NOR and its
    /// arity is free to shrink, so `NOR(a, 0)` is a real 1-input NOR.
    #[test]
    fn a_nor_cell_with_a_constant_zero_pin_folds_to_a_real_1_input_nor() {
        let json = json!({
            "modules": {
                "top": {
                    "ports": {
                        "a": { "direction": "input", "bits": [2] },
                        "y": { "direction": "output", "bits": [4] }
                    },
                    "cells": {
                        "nor0": {
                            "type": "$_NOR_",
                            "connections": { "A": [2], "B": ["0"], "Y": [4] }
                        }
                    }
                }
            }
        });

        let (netlist, port_map) = netlist_from_json(&json, "top").expect("NOR with one constant-0 pin must synthesize");

        assert_eq!(netlist.gates.len(), 1, "folding must not synthesize an extra gate");
        assert_eq!(netlist.gates[0].inputs, vec!["a".to_string()], "the folded B pin must not appear");
        assert_eq!(netlist.gates[0].kind, GateKind::Nor(1));
        assert_eq!(port_map["y"], netlist.gates[0].output);
    }

    /// A fixed-arity gate-level cell is the opposite case: its expansion is
    /// written against all of its pins, so a constant there is rejected by
    /// name rather than folded into a guess.
    #[test]
    fn a_constant_pin_on_a_fixed_arity_cell_is_rejected_by_name() {
        let json = json!({
            "modules": {
                "top": {
                    "ports": {
                        "a": { "direction": "input", "bits": [2] },
                        "y": { "direction": "output", "bits": [4] }
                    },
                    "cells": {
                        "and0": {
                            "type": "$_AND_",
                            "connections": { "A": [2], "B": ["0"], "Y": [4] }
                        }
                    }
                }
            }
        });

        let message = match netlist_from_json(&json, "top") {
            Ok(_) => panic!("a constant pin on an $_AND_ must not be silently folded"),
            Err(error) => error.to_string(),
        };
        assert!(message.contains("and0"), "error must name the cell: {message}");
        assert!(message.contains('B'), "error must name the pin: {message}");
    }

    /// A cell type this library has no realisation for is a hard, loud
    /// error naming both the cell and its type -- never a silently dropped
    /// gate (which would be a netlist that still compiles, to the wrong
    /// circuit).
    #[test]
    fn an_unmapped_cell_type_fails_loudly_and_names_the_cell() {
        let json = json!({
            "modules": {
                "top": {
                    "ports": {
                        "a": { "direction": "input", "bits": [2] },
                        "y": { "direction": "output", "bits": [3] }
                    },
                    "cells": {
                        "dff0": {
                            "type": "$_DFF_P_",
                            "connections": { "D": [2], "Y": [3] }
                        }
                    }
                }
            }
        });

        let message = match netlist_from_json(&json, "top") {
            Ok(_) => panic!("an unmapped cell type must not silently synthesize"),
            Err(error) => error.to_string(),
        };
        assert!(message.contains("dff0"), "error must name the cell: {message}");
        assert!(message.contains("$_DFF_P_"), "error must name the cell's type: {message}");
    }
}

//! Read Yosys's `write_json` output -- after `abc -genlib redstone_nor.genlib`
//! has mapped every bit of logic onto NOR1/NOR2/NOR3/BUF -- into a
//! [`Netlist`].
//!
//! Yosys's JSON schema (see its own `docs/source/cmd/write_json.rst`) is
//! deliberately general: nets are just integers ("bits"), a constant is a
//! `"0"`/`"1"`/`"x"`/`"z"` string in the same array a real net id would sit
//! in, and cells reference nets by name-keyed connection lists. This module
//! only has to handle the slice of that schema `redstone_nor.genlib` can
//! actually produce -- but within that slice, every construct is either
//! handled correctly or rejected with a specific error. Nothing is silently
//! dropped: a dropped cell is a netlist that still compiles, to the wrong
//! circuit.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::circuits::netlist_builder::NetlistBuilder;
use crate::compile::topology::{self, GateKind, Library, Primitive, Template, TemplateNode};
use crate::compile::Netlist;

use super::FrontendError;

/// Genlib/Yosys's own pin-naming convention for a NOR cell's inputs, in
/// declaration order -- `A`, `B`, `C` for a 1-, 2-, and 3-input NOR
/// respectively. This is a JSON-reading fact (which key names to look up in
/// a cell's `connections` object), not a redstone one, so it lives here
/// rather than in `topology`, which has no reason to know Yosys's pin names.
const NOR_PIN_NAMES: [&str; 3] = ["A", "B", "C"];

fn unsupported(message: impl Into<String>) -> FrontendError {
    FrontendError::Unsupported(message.into())
}

/// One bit of a Yosys "bits" array: either a real net id, or a literal
/// constant. `redstone_nor.genlib` cannot realize a hard-wired driver of any
/// kind (there is no "always on" or "always off" cell in real redstone), so
/// every constant this reader meets is a candidate for either folding away
/// (a `0` feeding a NOR pin, which changes nothing) or a hard error (any
/// other use).
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
                "constant bit `{other}` -- the redstone NOR library has no way to drive a \
                 hard-wired x/z value onto a real net"
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

/// The single bit on `pin` of a cell's `connections` object. Every cell type
/// this reader supports (`NOR1`/`NOR2`/`NOR3`/`BUF`) has exactly one bit per
/// pin -- `redstone_nor.genlib` never declares a multi-bit pin -- so a pin
/// that is not exactly 1 bit wide means either a different, unsupported
/// genlib produced this JSON, or Yosys did something this frontend does not
/// expect.
fn single_bit<'a>(connections: &'a Map<String, Value>, pin: &str, cell_name: &str) -> Result<&'a Value, FrontendError> {
    let bits = connections
        .get(pin)
        .ok_or_else(|| unsupported(format!("cell `{cell_name}` has no `{pin}` connection")))?;
    let bits = as_array(bits, &format!("cell `{cell_name}` pin `{pin}`"))?;
    if bits.len() != 1 {
        return Err(unsupported(format!(
            "cell `{cell_name}` pin `{pin}` is {}-bit; every cell in redstone_nor.genlib is scalar",
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
    /// The redstone realisation of every gate type this frontend knows how
    /// to build -- the boundary `build_cell` crosses instead of matching a
    /// cell's type string and separately knowing what to build for it. See
    /// `topology::gate_kind_for_yosys_cell` for the table that names which
    /// Yosys cell type is which entry here.
    library: Library,
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

    /// Resolve a NOR pin's bit to a real input signal, or `None` if it was a
    /// constant 0 (which just drops out of a NOR's input list: `NOR(x, 0)`
    /// is `NOR(x)`). A constant 1 forces the whole gate's output to a
    /// permanent 0, which this library has no way to drive, so that is a
    /// hard error rather than a silent fold.
    fn resolve_input_pin(&mut self, connections: &Map<String, Value>, pin: &str, cell_name: &str) -> Result<Option<String>, FrontendError> {
        let bit = single_bit(connections, pin, cell_name)?;
        match parse_bit(bit)? {
            Bit::Net(id) => Ok(Some(self.resolve(id)?)),
            Bit::Zero => Ok(None),
            Bit::One => Err(unsupported(format!(
                "cell `{cell_name}` pin `{pin}` is tied to constant 1, which would force its \
                 output permanently low -- the redstone NOR library has no tie-off cell to \
                 realize that"
            ))),
        }
    }

    /// Look `cell`'s type up in `topology::gate_kind_for_yosys_cell` and
    /// realize whichever `GateKind` it names -- the boundary this reader
    /// crosses instead of matching a cell's type string and separately
    /// knowing what to build for it. A cell type the table has never heard
    /// of (including the constant drivers, which this library has no way to
    /// realize) is a hard, loud error naming the cell and its type: a
    /// dropped cell is a netlist that still compiles, to the wrong circuit.
    fn build_cell(&mut self, cell: &CellInfo<'a>) -> Result<String, FrontendError> {
        match topology::gate_kind_for_yosys_cell(cell.cell_type) {
            Some(GateKind::Nor(arity)) => self.build_nor(cell, &NOR_PIN_NAMES[..arity]),
            Some(GateKind::Buf) => self.build_buf(cell),
            // `gate_kind_for_yosys_cell` never actually returns this today
            // (see that function's own doc comment: no genlib line produces
            // `$_OR_` yet), so this arm exists only so the match stays
            // exhaustive as `GateKind` grows -- mapping `$_OR_` onto the
            // library's wire-merge entries is step 4 of the referenced
            // spec's Order, not this one.
            Some(GateKind::Or(_)) => Err(unsupported(format!(
                "cell `{}` has type `{}`, which names a `GateKind::Or` -- the topology library can \
                 realize that, but this frontend cannot reach it yet: `redstone_nor.genlib` has no \
                 GATE line that produces it, so `gate_kind_for_yosys_cell` never actually returns it \
                 today either. Mapping `$_OR_` onto the library's wire-merge entries is a later task.",
                cell.name, cell.cell_type
            ))),
            None => Err(unsupported(format!(
                "cell `{}` has type `{}`, which `topology::gate_kind_for_yosys_cell` does not map to any \
                 redstone realization. Only NOR1/NOR2/NOR3/BUF from redstone_nor.genlib are supported -- \
                 was `abc -genlib redstone_nor.genlib` actually run, and did it fully map the design? (If \
                 the type is `$__ZERO`/`$__ONE`: those drive a hard-wired constant, which this library has \
                 no way to realize in real redstone.)",
                cell.name, cell.cell_type
            ))),
        }
    }

    fn build_nor(&mut self, cell: &CellInfo<'a>, pins: &[&str]) -> Result<String, FrontendError> {
        let mut inputs = Vec::with_capacity(pins.len());
        for &pin in pins {
            if let Some(name) = self.resolve_input_pin(cell.connections, pin, &cell.name)? {
                inputs.push(name);
            }
        }

        if inputs.is_empty() {
            // Every input pin was tied to constant 0. NOR() of nothing is a
            // hard-wired 1, which (like constant 0) this library cannot
            // drive.
            return Err(unsupported(format!(
                "cell `{}` has no real inputs left after folding constant-0 pins; a hard-wired-1 \
                 output has no realization in the redstone NOR library",
                cell.name
            )));
        }

        // Constant-0 folding can leave fewer real inputs than the cell's own
        // declared arity (a NOR3 with one pin tied to 0 is a real 2-input
        // NOR) -- `pins.len()` is always <= 3 (it is one of `NOR_PIN_NAMES`'s
        // own prefixes, one per registered `GateKind::Nor` arity), so
        // `inputs.len()` is too, and always names a real library entry.
        let kind = GateKind::Nor(inputs.len());
        let entry = self
            .library
            .choose(kind)
            .unwrap_or_else(|| panic!("{kind:?} has no library entry -- default_library() should always register 1..=3"));
        Ok(realize_template(&mut self.builder, &entry.template, inputs))
    }

    fn build_buf(&mut self, cell: &CellInfo<'a>) -> Result<String, FrontendError> {
        let input = self
            .resolve_input_pin(cell.connections, "A", &cell.name)?
            .ok_or_else(|| unsupported(format!("cell `{}` (BUF) is tied to constant 0 on its input", cell.name)))?;
        Ok(self.synthesize_buffer(&input))
    }

    /// A BUF cell (and a direct `assign out = in;` at the top level) is a
    /// pass-through -- there is no native "wire only" primitive in
    /// redstone, every real signal path here ends in a torch, so this
    /// realizes it as `topology::GateKind::Buf`'s own entry: two chained
    /// 1-input NOR gates, `NOT(NOT(x)) == x`. See `redstone_nor.genlib`'s
    /// `BUF` entry for why ABC needs a BUF cell to exist in the library at
    /// all even though it costs two real gates.
    fn synthesize_buffer(&mut self, input: &str) -> String {
        let entry = self
            .library
            .choose(GateKind::Buf)
            .expect("GateKind::Buf is always registered by Library::default_library()");
        realize_template(&mut self.builder, &entry.template, vec![input.to_string()])
    }
}

/// Realize `template` -- a `topology::Template` whose every node is a
/// `Primitive::Torch` (the only primitive a NOR-only `NetlistBuilder` knows
/// how to build) -- as concrete NOR gates: one `builder.nor(..)` call per
/// node, landing `inputs` (in the same order as `template.inputs`) on the
/// nodes they name and wiring each `internal_edges` source into its target
/// as an extra input, then returning the signal name driving
/// `template.output`.
///
/// This is what "the frontend maps a Yosys cell type onto a library entry"
/// actually means at the point a gate gets built, not just chosen: every
/// entry this frontend ever realizes (`topology::gate_kind_for_yosys_cell`'s
/// `Nor` and `Buf` targets) is built entirely out of `Torch` nodes today, so
/// this one function replaces what used to be a hand-written NOR call for
/// `NOR1`/`NOR2`/`NOR3` and a separate hand-written two-gate chain for
/// `BUF` -- both are now the same generic walk over whatever the library
/// says a technique is built from. A future entry built from a `Repeater`
/// or `Comparator` would need this frontend to grow the ability to build
/// one (there is no such cell yet -- see `topology::Primitive`'s doc
/// comment), so `resolve_node` panics rather than silently mis-realizing it.
fn realize_template(builder: &mut NetlistBuilder, template: &Template, inputs: Vec<String>) -> String {
    assert_eq!(
        inputs.len(),
        template.inputs.len(),
        "template declares {} input(s), got {}",
        template.inputs.len(),
        inputs.len()
    );

    let mut incoming: HashMap<TemplateNode, Vec<String>> = HashMap::new();
    for (&role, input) in template.inputs.iter().zip(inputs) {
        incoming.entry(role).or_default().push(input);
    }

    let mut built: HashMap<TemplateNode, String> = HashMap::new();
    let output = template.output.expect(
        "realize_template only ever runs on GateKind::Nor/Buf entries, which always name a real \
         output primitive -- an entry with none (a wire-merge OR) is not reachable through this \
         frontend yet (see build_cell's own GateKind::Or arm)",
    );
    resolve_node(output, template, &incoming, &mut built, builder)
}

/// One node of `realize_template`'s walk: build `node`'s own NOR gate (its
/// external inputs plus, recursively, whatever internal edges feed it),
/// memoized in `built` so a node with more than one internal-edge consumer
/// -- none exist yet, but nothing here assumes otherwise -- is built once.
fn resolve_node(
    node: TemplateNode,
    template: &Template,
    incoming: &HashMap<TemplateNode, Vec<String>>,
    built: &mut HashMap<TemplateNode, String>,
    builder: &mut NetlistBuilder,
) -> String {
    if let Some(name) = built.get(&node) {
        return name.clone();
    }

    let primitive = template
        .nodes
        .iter()
        .find_map(|&(role, primitive)| (role == node).then_some(primitive))
        .expect("node belongs to its own template's node list");
    assert_eq!(primitive, Primitive::Torch, "this frontend only realizes Torch nodes into NOR gates");

    let mut node_inputs = incoming.get(&node).cloned().unwrap_or_default();
    for &(from, to) in &template.internal_edges {
        if to == node {
            node_inputs.push(resolve_node(from, template, incoming, built, builder));
        }
    }

    let output = builder.nor(&node_inputs);
    built.insert(node, output.clone());
    output
}

/// Convert Yosys's JSON (as produced by `write_json` after `abc -genlib
/// redstone_nor.genlib`, see `synth.py`) into a [`Netlist`] for
/// `top_module`.
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
        library: Library::default_library(),
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
                    return Err(unsupported(format!("output port `{port_name}` is tied to a constant -- the redstone NOR library has no realizable driver for that")));
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

    let netlist = Netlist { inputs, outputs, gates: ctx.builder.gates };
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

    /// A `BUF` cell should realize as exactly `topology::GateKind::Buf`'s own
    /// template says it does: two chained 1-input NOR gates, the first
    /// reading the real input, the second reading the first's output and
    /// driving the declared output -- the same shape `synthesize_buffer`
    /// used to build by hand, now walked generically out of the library.
    #[test]
    fn a_buf_cell_realizes_as_two_chained_nor1_gates() {
        let json = json!({
            "modules": {
                "top": {
                    "ports": {
                        "a": { "direction": "input", "bits": [2] },
                        "y": { "direction": "output", "bits": [3] }
                    },
                    "cells": {
                        "buf0": {
                            "type": "BUF",
                            "connections": { "A": [2], "Y": [3] }
                        }
                    }
                }
            }
        });

        let (netlist, port_map) = netlist_from_json(&json, "top").expect("a lone BUF cell must synthesize");

        assert_eq!(netlist.inputs, vec!["a".to_string()]);
        assert_eq!(netlist.gates.len(), 2, "BUF is two real NOR gates, never a gate of its own kind");
        assert_eq!(netlist.gates[0].inputs, vec!["a".to_string()], "the first torch reads the real input");
        assert_eq!(
            netlist.gates[1].inputs,
            vec![netlist.gates[0].output.clone()],
            "the second torch reads the first torch's own output"
        );
        assert_eq!(netlist.outputs, vec![netlist.gates[1].output.clone()]);
        assert_eq!(port_map["y"], netlist.gates[1].output);
    }

    /// Constant-0 folding can leave a `NOR3` cell with fewer real inputs than
    /// its declared arity -- `build_nor` has to re-derive the smaller
    /// `GateKind` from what survives folding rather than realizing
    /// `GateKind::Nor(3)`'s (3-input) template with a padded input list.
    #[test]
    fn a_nor3_cell_with_a_constant_zero_pin_folds_to_a_real_2_input_nor() {
        let json = json!({
            "modules": {
                "top": {
                    "ports": {
                        "a": { "direction": "input", "bits": [2] },
                        "b": { "direction": "input", "bits": [3] },
                        "y": { "direction": "output", "bits": [4] }
                    },
                    "cells": {
                        "nor0": {
                            "type": "NOR3",
                            "connections": { "A": [2], "B": [3], "C": ["0"], "Y": [4] }
                        }
                    }
                }
            }
        });

        let (netlist, port_map) = netlist_from_json(&json, "top").expect("NOR3 with one constant-0 pin must synthesize");

        assert_eq!(netlist.gates.len(), 1, "folding must not synthesize an extra gate");
        assert_eq!(netlist.gates[0].inputs, vec!["a".to_string(), "b".to_string()], "the folded C pin must not appear");
        assert_eq!(port_map["y"], netlist.gates[0].output);
    }

    /// A cell type `redstone_nor.genlib` never produces is a hard, loud
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

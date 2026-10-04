//! Source-owned shared-inout semantics for the native physical compiler.
use super::ast::*;
use super::elaborate::{self, Design, ElabAssign, SignalInfo};
use super::rtl::{RtlBinaryOp, RtlExpr, RtlKind};
use super::source::FileId;
use super::{logic, netlist, parser, PortBinding, SourceInput};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone)]
struct Drive {
    value: Expr,
    present: Expr,
}
#[derive(Debug)]
pub(crate) struct DriveContribution {
    pub(crate) value: String,
    pub(crate) present: String,
    pub(crate) injection_value: String,
    #[cfg(test)]
    pub(crate) origin: super::ElabNodeId,
}
#[derive(Debug)]
pub(crate) struct InoutBoundary {
    pub(crate) original: PortBinding,
    pub(crate) external_value: String,
    pub(crate) external_present: String,
    pub(crate) resolved: String,
    pub(crate) conflict: String,
    pub(crate) floating: String,
    pub(crate) internal_contributions: Vec<DriveContribution>,
    pub(crate) external_injection: String,
    pub(crate) read_input: Option<String>,
    pub(crate) external_read_input: Option<String>,
    pub(crate) external_observed: Option<String>,
    pub(crate) expected_value: Option<String>,
}
pub(crate) struct SemanticArtifact {
    pub(crate) netlist: crate::compile::Netlist,
    pub(crate) boundaries: Vec<InoutBoundary>,
    pub(crate) source_outputs: BTreeMap<String, String>,
    #[cfg(test)]
    pub(crate) debug: super::DebugDatabase,
}
#[derive(Debug, PartialEq, Eq)]
pub enum InvalidBus {
    Floating,
    Contention,
    ProtocolViolation,
}
impl InoutBoundary {
    #[cfg(test)]
    pub(crate) fn observe(&self, values: &BTreeMap<String, bool>) -> Result<bool, InvalidBus> {
        if let Some(observed) = &self.external_observed {
            if values[observed] != values[&self.external_injection] {
                return Err(InvalidBus::ProtocolViolation);
            }
        }
        if let Some(expected) = &self.expected_value {
            if values[expected] != values[&self.resolved] {
                return Err(InvalidBus::ProtocolViolation);
            }
        }
        if values[&self.conflict] {
            return Err(InvalidBus::Contention);
        }
        if values[&self.floating] {
            return Err(InvalidBus::Floating);
        }
        Ok(values[&self.resolved])
    }
}
fn expr(template: &Expr, kind: ExprKind) -> Expr {
    Expr {
        kind,
        ..template.clone()
    }
}
fn contains_z(e: &Expr, z: &HashSet<SourceNodeId>) -> bool {
    z.contains(&e.id)
        || match &e.kind {
            ExprKind::Unary(_, a) | ExprKind::BitSelect(a, _) | ExprKind::PartSelect(a, _, _) => {
                contains_z(a, z)
            }
            ExprKind::Binary(_, a, b) => contains_z(a, z) || contains_z(b, z),
            ExprKind::Ternary(a, b, c) => contains_z(a, z) || contains_z(b, z) || contains_z(c, z),
            ExprKind::Concat(parts) => parts.iter().any(|a| contains_z(a, z)),
            _ => false,
        }
}
fn consume_z(e: &Expr, z: &HashSet<SourceNodeId>, seen: &mut HashSet<SourceNodeId>) {
    if z.contains(&e.id) {
        seen.insert(e.id);
    }
    match &e.kind {
        ExprKind::Unary(_, a) | ExprKind::BitSelect(a, _) | ExprKind::PartSelect(a, _, _) => {
            consume_z(a, z, seen)
        }
        ExprKind::Binary(_, a, b) => {
            consume_z(a, z, seen);
            consume_z(b, z, seen);
        }
        ExprKind::Ternary(a, b, c) => {
            consume_z(a, z, seen);
            consume_z(b, z, seen);
            consume_z(c, z, seen);
        }
        ExprKind::Concat(parts) => {
            for a in parts {
                consume_z(a, z, seen)
            }
        }
        _ => {}
    }
}
fn drive(e: &Expr, z: &HashSet<SourceNodeId>) -> Result<Drive, String> {
    if z.contains(&e.id) {
        return Ok(Drive {
            value: e.clone(),
            present: expr(
                e,
                ExprKind::Literal {
                    signed: false,
                    width: match &e.kind {
                        ExprKind::Literal { width, .. } => *width,
                        _ => unreachable!(),
                    },
                    bits: match &e.kind {
                        ExprKind::Literal { bits, .. } => vec![false; bits.len()],
                        _ => unreachable!(),
                    },
                },
            ),
        });
    }
    match &e.kind {
        ExprKind::Ternary(select, a, b) => {
            if contains_z(select, z) {
                return Err("Z in drive selector is unsupported".into());
            }
            let a = drive(a, z)?;
            let b = drive(b, z)?;
            let absent = |mask: &Expr| matches!(&mask.kind,ExprKind::Literal {bits,..} if bits.iter().all(|b|!*b));
            // Value has no semantics when absent. A typed XOR-zero retains
            // conditional width/sign coercion without adding enable twice.
            let value = if absent(&b.present) {
                expr(
                    e,
                    ExprKind::Binary(BinaryOp::Xor, Box::new(a.value), Box::new(b.value)),
                )
            } else if absent(&a.present) {
                expr(
                    e,
                    ExprKind::Binary(BinaryOp::Xor, Box::new(b.value), Box::new(a.value)),
                )
            } else {
                expr(
                    e,
                    ExprKind::Ternary(select.clone(), Box::new(a.value), Box::new(b.value)),
                )
            };
            Ok(Drive {
                value,
                present: expr(
                    e,
                    ExprKind::Ternary(select.clone(), Box::new(a.present), Box::new(b.present)),
                ),
            })
        }
        ExprKind::Concat(parts) => {
            let pairs = parts
                .iter()
                .map(|a| drive(a, z))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Drive {
                value: expr(
                    e,
                    ExprKind::Concat(pairs.iter().map(|p| p.value.clone()).collect()),
                ),
                present: expr(
                    e,
                    ExprKind::Concat(pairs.iter().map(|p| p.present.clone()).collect()),
                ),
            })
        }
        _ => {
            if contains_z(e, z) {
                return Err("Z is allowed only in continuous drive branches".into());
            }
            // Width-preserving all-ones mask; the existing elaborator determines expression width.
            let zero = expr(
                e,
                ExprKind::Binary(BinaryOp::Xor, Box::new(e.clone()), Box::new(e.clone())),
            );
            Ok(Drive {
                value: e.clone(),
                present: expr(e, ExprKind::Unary(UnaryOp::Not, Box::new(zero))),
            })
        }
    }
}
fn signal_expr(template: &Expr, name: String) -> Expr {
    expr(template, ExprKind::Ident(name))
}
fn rsignal(design: &Design, index: usize, bit: u32) -> RtlExpr {
    RtlExpr::new(
        RtlKind::Signal {
            signal: index,
            lsb: bit,
        },
        1,
        design.signals[index].elab,
    )
}
fn generated(mut e: RtlExpr) -> RtlExpr {
    e.synthetic = Some(super::debug::SyntheticOrigin::Passthrough);
    e
}
fn binary(a: RtlExpr, b: RtlExpr, op: RtlBinaryOp) -> RtlExpr {
    let id = a.elab;
    generated(RtlExpr::new(
        RtlKind::Binary(op, Box::new(a), Box::new(b)),
        1,
        id,
    ))
}
fn invert(a: RtlExpr) -> RtlExpr {
    let id = a.elab;
    generated(RtlExpr::new(RtlKind::Not(Box::new(a)), 1, id))
}
fn compile_projected(
    sources: &[SourceInput<'_>],
    top: &str,
    physical_reads: bool,
) -> Result<SemanticArtifact, String> {
    let files = sources
        .iter()
        .enumerate()
        .map(|(i, s)| (FileId(i as u32), s.text))
        .collect::<Vec<_>>();
    let (mut set, z) = parser::parse_sources_drives(&files, true).map_err(|e| e.message)?;
    let mut original_ports = Vec::new();
    let mut z_drives = HashSet::new();
    let mut consumed_z = HashSet::new();
    let mut companion_names = HashSet::new();
    for module in &mut set.modules {
        // Prototype identifiers are reserved explicitly; silently shadowing source is prohibited.
        if module.ports.iter().any(|p|p.name.name.starts_with("__reda_bus_")) || module.items.iter().any(|item| matches!(item,Item::Declaration(d) if d.name.name.starts_with("__reda_bus_"))) {return Err("reserved resolved-bus prototype identifier".into());}
        if module.name.name == top {
            let mut external = Vec::new();
            for port in &mut module.ports {
                if port.direction == PortDirection::Inout {
                    original_ports.push(port.clone());
                    // Private elaboration transport only: original identity retained separately.
                    port.direction = PortDirection::Output;
                    for role in ["value", "present"] {
                        let mut p = port.clone();
                        p.direction = PortDirection::Input;
                        p.name.name = format!("__reda_bus_ext_{role}_{}", port.name.name);
                        external.push(p);
                    }
                }
            }
            module.ports.extend(external);
        }
        let mut items = Vec::new();
        for (n, item) in std::mem::take(&mut module.items).into_iter().enumerate() {
            match item {
                Item::Assign(mut a) => {
                    // Ordinary assignments keep the native elaborator's complete
                    // lvalue and declaration-order support. Their contributions
                    // use the existing all-present default below.
                    if !contains_z(&a.rhs, &z) {
                        items.push(Item::Assign(a));
                        continue;
                    }
                    let ExprKind::Ident(target) = &a.lhs.kind else {
                        return Err("prototype drive target must be a complete declared net".into());
                    };
                    let range = module
                        .ports
                        .iter()
                        .find(|p| p.name.name == *target)
                        .map(|p| p.range.clone())
                        .or_else(|| {
                            items.iter().find_map(|i| match i {
                                Item::Declaration(d) if d.name.name == *target => {
                                    Some(d.range.clone())
                                }
                                _ => None,
                            })
                        })
                        .ok_or("drive target declaration not found")?;
                    if contains_z(&a.rhs, &z) {
                        z_drives.insert(a.id);
                    }
                    consume_z(&a.rhs, &z, &mut consumed_z);
                    let d = drive(&a.rhs, &z)?;
                    let name = format!("__reda_bus_presence_{n}");
                    companion_names.insert(name.clone());
                    items.push(Item::Declaration(NetDecl {
                        signed: false,
                        id: a.id,
                        span: a.span,
                        range,
                        name: Ident {
                            name: name.clone(),
                            span: a.span,
                        },
                    }));
                    items.push(Item::Assign(Assign {
                        lhs: signal_expr(&a.lhs, name),
                        rhs: d.present,
                        ..a.clone()
                    }));
                    a.rhs = d.value;
                    items.push(Item::Assign(a));
                }
                other => items.push(other),
            }
        }
        module.items = items;
    }
    if consumed_z != z {
        return Err("Z outside continuous drive context".into());
    }
    let mut design = elaborate::elaborate(&set, top).map_err(|e| format!("{e:?}"))?;
    if !design.comb_blocks.is_empty() || design.assigns.iter().any(|a| a.clock.is_some()) {
        return Err("resolved initial proof excludes procedural/clocked drivers".into());
    }
    let aliases = logic::AliasClasses::new(&design);
    let mut presence = BTreeMap::new();
    let mut contributions =
        BTreeMap::<(usize, u32), Vec<(RtlExpr, RtlExpr, super::ElabNodeId)>>::new();
    let assigns = std::mem::take(&mut design.assigns);
    for a in &assigns {
        let info = &design.signals[a.signal];
        let short = info.name.rsplit('.').next().unwrap();
        if companion_names.contains(short) {
            presence.insert(
                (
                    design.nodes[a.elab.0 as usize].instance,
                    design.nodes[a.elab.0 as usize].source,
                ),
                a.value.clone(),
            );
        }
    }
    // Companion and source drive share source assignment ID but have distinct elaborated expression records.
    for a in assigns {
        let short = design.signals[a.signal].name.rsplit('.').next().unwrap();
        if companion_names.contains(short) {
            design.assigns.push(a);
            continue;
        }
        let source = design.nodes[a.elab.0 as usize].source;
        let instance = design.nodes[a.elab.0 as usize].instance;
        let p = presence
            .get(&(instance, source))
            .cloned()
            .unwrap_or_else(|| {
                RtlExpr::new(
                    RtlKind::Const(vec![true; a.width as usize]),
                    a.width,
                    a.elab,
                )
            });
        for bit in 0..a.width {
            let key = aliases.canonical(a.signal, a.lsb + bit);
            let v = RtlExpr::new(RtlKind::Slice(Box::new(a.value.clone()), bit), 1, a.elab);
            let p = RtlExpr::new(RtlKind::Slice(Box::new(p.clone()), bit), 1, a.elab);
            contributions.entry(key).or_default().push((v, p, a.elab));
        }
    }
    let mut boundaries = Vec::new();
    for port in original_ports {
        let bus = design
            .signals
            .iter()
            .position(|s| s.name == port.name.name)
            .ok_or("top bus disappeared")?;
        for bit in 0..design.signals[bus].width {
            let value = format!("__reda_bus_ext_value_{}", port.name.name);
            let present = format!("__reda_bus_ext_present_{}", port.name.name);
            let vi = design.signals.iter().position(|s| s.name == value).unwrap();
            let pi = design
                .signals
                .iter()
                .position(|s| s.name == present)
                .unwrap();
            let key = aliases.canonical(bus, bit);
            contributions.entry(key).or_default().push((
                rsignal(&design, vi, bit),
                rsignal(&design, pi, bit),
                design.signals[bus].elab,
            ));
            let name = elaborate::bit_name(&port.name.name, design.signals[bus].width, bit);
            boundaries.push((
                key,
                InoutBoundary {
                    original: PortBinding {
                        name,
                        direction: PortDirection::Inout,
                        signal: port.name.name.clone(),
                        elab: design.signals[bus].elab,
                        bit,
                    },
                    external_value: elaborate::bit_name(&value, design.signals[bus].width, bit),
                    external_present: elaborate::bit_name(&present, design.signals[bus].width, bit),
                    resolved: String::new(),
                    conflict: String::new(),
                    floating: String::new(),
                    internal_contributions: Vec::new(),
                    external_injection: String::new(),
                    read_input: None,
                    external_read_input: None,
                    external_observed: None,
                    expected_value: None,
                },
            ));
        }
    }
    let allowed = design
        .signals
        .iter()
        .enumerate()
        .filter(|(_, s)| s.direction == Some(PortDirection::Inout))
        .flat_map(|(i, s)| {
            (0..s.width)
                .map(|b| aliases.canonical(i, b))
                .collect::<Vec<_>>()
        })
        .chain(boundaries.iter().map(|(k, _)| *k))
        .collect::<HashSet<_>>();
    for ((signal, bit), drivers) in contributions {
        if drivers.len() > 1 && !allowed.contains(&(signal, bit)) {
            return Err("multiple writers on an ordinary unresolved net".into());
        }
        if !allowed.contains(&(signal, bit))
            && drivers
                .iter()
                .any(|(_, _, id)| z_drives.contains(&design.nodes[id.0 as usize].source))
        {
            return Err("absent drive requires a resolved net".into());
        }
        let template = design.signals[signal].clone();
        let mut high = RtlExpr::new(RtlKind::Const(vec![false]), 1, template.elab);
        let mut low = high.clone();
        for (v, p, _) in &drivers {
            high = binary(
                high,
                binary(p.clone(), v.clone(), RtlBinaryOp::And),
                RtlBinaryOp::Or,
            );
            low = binary(
                low,
                binary(p.clone(), invert(v.clone()), RtlBinaryOp::And),
                RtlBinaryOp::Or,
            );
        }
        let conflict = binary(high.clone(), low.clone(), RtlBinaryOp::And);
        let floating = invert(binary(high.clone(), low, RtlBinaryOp::Or));
        design.assigns.push(ElabAssign {
            elab: template.elab,
            span: template.span,
            signal,
            lsb: bit,
            width: 1,
            value: high,
            clock: None,
        });
        if let Some((_, boundary)) = boundaries.iter_mut().find(|(k, _)| *k == (signal, bit)) {
            for (number, (value, present, origin)) in drivers.iter().enumerate() {
                let mut names = Vec::new();
                for (role, value) in [
                    ("value", value.clone()),
                    ("present", present.clone()),
                    (
                        "injection",
                        binary(value.clone(), present.clone(), RtlBinaryOp::And),
                    ),
                ] {
                    let name = format!(
                        "__reda_bus_driver_{number}_{role}_{}",
                        boundary.original.name.replace(['[', ']'], "_")
                    );
                    let index = design.signals.len();
                    design.signals.push(SignalInfo {
                        name: name.clone(),
                        width: 1,
                        direction: Some(PortDirection::Output),
                        elab: *origin,
                        ..template.clone()
                    });
                    design.assigns.push(ElabAssign {
                        elab: *origin,
                        span: template.span,
                        signal: index,
                        lsb: 0,
                        width: 1,
                        value,
                        clock: None,
                    });
                    names.push(name);
                }
                if number + 1 == drivers.len() {
                    boundary.external_injection = names[2].clone();
                } else {
                    boundary.internal_contributions.push(DriveContribution {
                        value: names[0].clone(),
                        present: names[1].clone(),
                        injection_value: names[2].clone(),
                        #[cfg(test)]
                        origin: *origin,
                    });
                }
            }
            for (role, value) in [("conflict", conflict), ("floating", floating)] {
                let name = format!(
                    "__reda_bus_{role}_{}",
                    boundary.original.name.replace(['[', ']'], "_")
                );
                let index = design.signals.len();
                design.signals.push(SignalInfo {
                    name: name.clone(),
                    width: 1,
                    direction: Some(PortDirection::Output),
                    ..template.clone()
                });
                design.assigns.push(ElabAssign {
                    elab: template.elab,
                    span: template.span,
                    signal: index,
                    lsb: 0,
                    width: 1,
                    value,
                    clock: None,
                });
                if role == "conflict" {
                    boundary.conflict = name;
                } else {
                    boundary.floating = name;
                }
            }
        }
    }
    if physical_reads {
        for (key, b) in &mut boundaries {
            let original = design
                .assigns
                .iter()
                .position(|a| (a.signal, a.lsb) == *key)
                .ok_or("missing canonical bus assignment")?;
            let expected = design.assigns[original].value.clone();
            let template = design.signals[key.0].clone();
            let read = format!(
                "__reda_bus_readback_{}",
                b.original.name.replace(['[', ']'], "_")
            );
            let input = design.signals.len();
            design.signals.push(SignalInfo {
                name: read.clone(),
                width: 1,
                direction: Some(PortDirection::Input),
                ..template.clone()
            });
            design.assigns[original].value = rsignal(&design, input, 0);
            b.read_input = Some(read);
            let external_read = format!(
                "__reda_bus_external_sense_{}",
                b.original.name.replace(['[', ']'], "_")
            );
            let external_input = design.signals.len();
            design.signals.push(SignalInfo {
                name: external_read.clone(),
                width: 1,
                direction: Some(PortDirection::Input),
                ..template.clone()
            });
            b.external_read_input = Some(external_read);
            let observed = format!(
                "__reda_bus_external_observed_{}",
                b.original.name.replace(['[', ']'], "_")
            );
            let external_output = design.signals.len();
            design.signals.push(SignalInfo {
                name: observed.clone(),
                width: 1,
                direction: Some(PortDirection::Output),
                ..template.clone()
            });
            design.assigns.push(ElabAssign {
                elab: template.elab,
                span: template.span,
                signal: external_output,
                lsb: 0,
                width: 1,
                value: rsignal(&design, external_input, 0),
                clock: None,
            });
            b.external_observed = Some(observed);

            let expected_name = format!(
                "__reda_bus_expected_{}",
                b.original.name.replace(['[', ']'], "_")
            );
            let output = design.signals.len();
            design.signals.push(SignalInfo {
                name: expected_name.clone(),
                width: 1,
                direction: Some(PortDirection::Output),
                ..template.clone()
            });
            design.assigns.push(ElabAssign {
                elab: template.elab,
                span: template.span,
                signal: output,
                lsb: 0,
                width: 1,
                value: expected,
                clock: None,
            });
            b.expected_value = Some(expected_name);
        }
    }
    let blasted = logic::build(&design).map_err(|e| format!("{e:?}"))?;
    let emitted = netlist::emit(&design, &blasted).map_err(|e| format!("{e:?}"))?;
    for (_, b) in &mut boundaries {
        b.resolved = emitted
            .ports
            .iter()
            .find(|p| p.name == b.original.name)
            .ok_or("missing resolved readback")?
            .signal
            .clone();
        b.original.signal = b.resolved.clone();
        let bind = |name: &str| {
            emitted
                .ports
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.signal.clone())
                .ok_or("missing resolved drive binding")
        };
        for d in &mut b.internal_contributions {
            d.value = bind(&d.value)?;
            d.present = bind(&d.present)?;
            d.injection_value = bind(&d.injection_value)?;
        }
        b.external_injection = bind(&b.external_injection)?;
        if let Some(name) = &b.external_observed {
            b.external_observed = Some(bind(name)?);
        }
        if let Some(name) = &b.expected_value {
            b.expected_value = Some(bind(name)?);
        }
        b.conflict = emitted
            .ports
            .iter()
            .find(|p| p.name == b.conflict)
            .ok_or("missing contention observer")?
            .signal
            .clone();
        b.floating = emitted
            .ports
            .iter()
            .find(|p| p.name == b.floating)
            .ok_or("missing floating observer")?
            .signal
            .clone();
    }
    super::evaluate::validate(&emitted.netlist).map_err(|e| e.to_string())?;
    let source_outputs = emitted
        .ports
        .iter()
        .filter(|p| p.direction == PortDirection::Output && !p.name.starts_with("__reda_bus_"))
        .map(|p| (p.name.clone(), p.signal.clone()))
        .collect();
    #[cfg(test)]
    let mut transformations = blasted.graph.transformations;
    #[cfg(test)]
    transformations.extend(emitted.transformations);
    #[cfg(test)]
    let debug = super::DebugDatabase {
        files: sources
            .iter()
            .map(|s| super::SourceFileInfo {
                name: s.name.into(),
                len: s.text.len() as u32,
            })
            .collect(),
        source_nodes: set.nodes,
        elab_nodes: design.nodes,
        gates: emitted.gates,
        logic_origins: blasted.graph.origins,
        extra_origins: blasted.graph.extra_origins,
        realisations: emitted.realisations,
        transformations,
        signals: emitted.signals,
        netlist_fingerprint: super::debug::netlist_fingerprint(&emitted.netlist),
    };
    Ok(SemanticArtifact {
        #[cfg(test)]
        debug,
        netlist: emitted.netlist,
        boundaries: boundaries.into_iter().map(|(_, b)| b).collect(),
        source_outputs,
    })
}

pub(crate) fn compile_semantics(
    sources: &[SourceInput<'_>],
    top: &str,
) -> Result<SemanticArtifact, String> {
    compile_projected(sources, top, false)
}
/// Validate the original source's dependency graph before replacing source bus
/// readback by a typed physical leaf. This prevents projection masking a loop.
pub(crate) fn compile_for_materialization(
    sources: &[SourceInput<'_>],
    top: &str,
) -> Result<SemanticArtifact, String> {
    compile_semantics(sources, top)?;
    compile_projected(sources, top, true)
}

#[cfg(test)]
mod tests {
    use super::super::evaluate::Evaluator;
    use super::*;
    #[test]
    fn ordinary_selected_and_forward_declared_assignments_preserve_bus_semantics() {
        let fixture = include_str!("../../tests/fixtures/resolved-inout.sv");
        for assignments in [
            "assign y[0]=bus; assign y[1]=~bus;",
            "assign y[1:0]={~bus,bus};",
            "assign {y[1],y[0]}={~bus,bus};",
            "assign late=bus; wire late; assign y={~late,late};",
        ] {
            let source = fixture.replace("output wire y", "output wire [1:0] y")
                .replace("assign y = bus;", assignments);
            let artifact = compile_semantics(&[SourceInput { name: "selected.sv", text: &source }], "top").unwrap();
            assert_eq!(artifact.boundaries.len(), 1);
            let bus = &artifact.boundaries[0];
            assert_eq!(bus.original.name, "bus");
            assert_eq!(bus.original.direction, PortDirection::Inout);
            assert_eq!(bus.internal_contributions.len(), 2);
            let mut evaluator = Evaluator::new(&artifact.netlist).unwrap();
            for state in 0usize..64 {
                let inputs = [("e0",0),("d0",1),("e1",2),("d1",3),
                              (bus.external_present.as_str(),4),(bus.external_value.as_str(),5)]
                    .into_iter().map(|(name,bit)| (name.to_owned(),state & (1<<bit)!=0)).collect();
                let values = evaluator.step(&inputs).unwrap();
                let active: Vec<_> = [(0,1),(2,3),(4,5)].into_iter()
                    .filter(|(en,_)| state & (1<<en)!=0)
                    .map(|(_,data)| state & (1<<data)!=0).collect();
                let expected = if active.is_empty() { Err(InvalidBus::Floating) }
                    else if active.iter().any(|value| *value != active[0]) { Err(InvalidBus::Contention) }
                    else { Ok(active[0]) };
                assert_eq!(bus.observe(&values), expected, "{assignments} state={state}");
                if let Ok(value) = expected {
                    assert_eq!(values[&artifact.source_outputs["y[0]"]], value);
                    assert_eq!(values[&artifact.source_outputs["y[1]"]], !value);
                }
            }
        }
    }

    #[test]
    fn selected_z_driver_keeps_explicit_full_net_refusal() {
        let source = "module top(input en,data,inout wire [1:0] bus); assign bus[0]=en?data:1'bz; endmodule";
        let error = compile_semantics(&[SourceInput { name: "selected-z.sv", text: source }], "top").err().unwrap();
        assert_eq!(error, "prototype drive target must be a complete declared net");
    }

    #[test]
    fn hierarchy_driver_set_matches_frozen_four_state_oracle() {
        let text = include_str!("../../tests/fixtures/resolved-inout.sv");
        let a = compile_semantics(
            &[SourceInput {
                name: "fixture.sv",
                text,
            }],
            "top",
        )
        .unwrap();
        assert_eq!(a.boundaries.len(), 1);
        let b = &a.boundaries[0];
        assert_eq!(b.original.direction, PortDirection::Inout);
        assert!(a.netlist.outputs.contains(&b.original.signal));
        assert_eq!(b.internal_contributions.len(), 2);
        assert_eq!(
            a.debug.source_nodes[a.debug.elab_nodes[b.original.elab.0 as usize].source.0 as usize]
                .kind,
            SourceKind::Port
        );
        for driver in &b.internal_contributions {
            let id = &driver.origin;
            assert_eq!(
                a.debug.source_nodes[a.debug.elab_nodes[id.0 as usize].source.0 as usize].kind,
                SourceKind::Assign
            );
        }
        assert!(a
            .debug
            .logic_origins
            .iter()
            .any(|o| o.synthetic == Some(super::super::debug::SyntheticOrigin::Passthrough)));
        let mut eval = Evaluator::new(&a.netlist).unwrap();
        let raw = include_str!("../../tests/fixtures/resolved-inout-standard-oracle.csv");
        let mut checked = 0;
        for line in raw.lines() {
            let fields = line.split(',').collect::<Vec<_>>();
            if fields.len() != 4 {
                continue;
            }
            let before = fields[0].parse::<usize>().unwrap();
            let after = fields[1].parse::<usize>().unwrap();
            let phase = fields[2].parse::<usize>().unwrap();
            let s = if phase == 0 { before } else { after };
            let inputs = [
                ("e0", 0),
                ("d0", 1),
                ("e1", 2),
                ("d1", 3),
                (b.external_present.as_str(), 4),
                (b.external_value.as_str(), 5),
            ]
            .into_iter()
            .map(|(n, bit)| (n.into(), s & (1 << bit) != 0))
            .collect();
            let out = eval.step(&inputs).unwrap();
            for d in &b.internal_contributions {
                assert_eq!(out[&d.injection_value], out[&d.value] && out[&d.present]);
            }
            assert_eq!(
                out[&b.external_injection],
                inputs[&b.external_value] && inputs[&b.external_present]
            );
            let conflict = out[&b.conflict];
            let floating = out[&b.floating];
            let actual = if conflict {
                "X"
            } else if floating {
                "Z"
            } else if out[&b.resolved] {
                "1"
            } else {
                "0"
            };
            assert_eq!(
                actual, fields[3],
                "state={s},from={before},to={after},phase={phase}"
            );
            match fields[3] {
                "0" | "1" => {
                    let v = fields[3] == "1";
                    assert_eq!(b.observe(&out), Ok(v));
                    assert_eq!(out[&a.source_outputs["y"]], v);
                }
                "X" => assert_eq!(b.observe(&out), Err(InvalidBus::Contention)),
                "Z" => assert_eq!(b.observe(&out), Err(InvalidBus::Floating)),
                _ => unreachable!(),
            }
            checked += 1;
        }
        assert_eq!(checked, 8192);
        assert!(
            super::super::compile_systemverilog(
                &[SourceInput {
                    name: "fixture.sv",
                    text
                }],
                &super::super::CompileOptions::new("top")
            )
            .is_err(),
            "public physical selection must stay closed"
        );
    }
    #[test]
    fn vector_alias_concat_and_multiple_readers_preserve_source_bits() {
        let text="module writer(input e,input [1:0] d,inout wire [1:0] b); assign b=e ? d : 2'bzz; endmodule module reader(inout wire [1:0] b,output [1:0] y); assign y=b; endmodule module top(input e,input [1:0] d,inout wire [1:0] bus,output [1:0] y,z); writer w(.e(e),.d(d),.b({bus[0],bus[1]})); reader a(.b(bus),.y(y)); reader c(.b({bus[0],bus[1]}),.y(z)); endmodule";
        let a = compile_semantics(
            &[SourceInput {
                name: "vector.sv",
                text,
            }],
            "top",
        )
        .unwrap();
        assert_eq!(a.boundaries.len(), 2);
        let mut eval = Evaluator::new(&a.netlist).unwrap();
        for state in 0..32 {
            let mut inputs = BTreeMap::from([
                ("e".into(), state & 1 != 0),
                ("d[0]".into(), state & 2 != 0),
                ("d[1]".into(), state & 4 != 0),
            ]);
            for b in &a.boundaries {
                inputs.insert(b.external_present.clone(), state & 8 != 0);
                inputs.insert(b.external_value.clone(), state & 16 != 0);
            }
            let out = eval.step(&inputs).unwrap();
            for b in &a.boundaries {
                let bit = b.original.bit as usize;
                let internal = state & (1 << (2 - bit)) != 0;
                let active = [
                    (state & 1 != 0, internal),
                    (state & 8 != 0, state & 16 != 0),
                ];
                let vals = active
                    .iter()
                    .filter(|p| p.0)
                    .map(|p| p.1)
                    .collect::<Vec<_>>();
                let expected = if vals.is_empty() {
                    Err(InvalidBus::Floating)
                } else if vals.iter().any(|v| *v != vals[0]) {
                    Err(InvalidBus::Contention)
                } else {
                    Ok(vals[0])
                };
                assert_eq!(b.observe(&out), expected, "state={state},bit={bit}");
                if let Ok(v) = expected {
                    assert_eq!(out[&a.source_outputs[&format!("y[{bit}]")]], v);
                    assert_eq!(out[&a.source_outputs[&format!("z[{}]", 1 - bit)]], v);
                }
            }
        }
    }
    #[test]
    fn contextual_z_rejects_arithmetic_and_bus_read_feedback() {
        for text in ["module top(input e,inout wire bus,output y); assign bus=1'bz & e; assign y=bus; endmodule","module top(output y); assign y=1'bz; endmodule","module top(input a,b,output y); assign y=a; assign y=b; endmodule","module reader(input a,output y); assign y=a; endmodule module top(output y); reader r(.a(1'bz),.y(y)); endmodule","module top(input e,inout wire bus,output y); assign bus=e ? ~bus : 1'bz; assign y=bus; endmodule"] {
   assert!(compile_semantics(&[SourceInput {name:"bad.sv",text}],"top").is_err());
  }
    }
}

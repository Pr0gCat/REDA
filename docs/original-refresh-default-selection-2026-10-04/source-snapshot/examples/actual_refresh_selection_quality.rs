//! Frozen whole-default versus candidate retained-history audit. No adoption.
use flate2::{write::GzEncoder, Compression};
use reda::compile::{
    compile,
    fragment_synth::{
        benchmark::canonical_world_fingerprint,
        identity::{
            InstanceId, ObservationId, ObservationSite, PortId, PrimitiveId, TopologyNodeId,
        },
    },
    geometry::Anchor,
    lowering::lower_optimised,
    CompiledCircuit, Netlist,
};
use reda::frontend::{compile_systemverilog, evaluate::Evaluator, CompileOptions, SourceInput};
use reda::redstone::{
    simulator::Simulator,
    world::{block::BlockKind, storage::World},
};
use serde::Serialize;
use std::{collections::BTreeMap, fs::File, io::Write, path::Path};

const SOURCE: &str = "module m(input a,b,c,d,e,f, output u,z,o,v,w,x,y,q,r); assign u=a|b; assign z=1'b0; assign o=1'b1; assign v=c&d; assign w=c&d; assign x=c; assign y=e^f; assign q=~(e|f); assign r=f; endmodule";
const KINDS: [BlockKind; 20] = [
    BlockKind::Solid,
    BlockKind::Glass,
    BlockKind::Slab,
    BlockKind::RedstoneWire,
    BlockKind::Repeater,
    BlockKind::Comparator,
    BlockKind::Torch,
    BlockKind::WallTorch,
    BlockKind::Lever,
    BlockKind::RedstoneBlock,
    BlockKind::Lamp,
    BlockKind::Piston,
    BlockKind::Button,
    BlockKind::PressurePlate,
    BlockKind::WeightedPressurePlate,
    BlockKind::Observer,
    BlockKind::Target,
    BlockKind::DaylightDetector,
    BlockKind::Other,
    BlockKind::Air,
];
type At = (i32, i32, i32);

fn write_json(path: &Path, value: &impl Serialize) {
    serde_json::to_writer_pretty(File::create(path).unwrap(), value).unwrap();
}
fn signal(world: &World, at: At) -> bool {
    let state = world.get(at.0, at.1, at.2);
    if state.kind == BlockKind::RedstoneWire {
        state.power > 0
    } else {
        state.lit
    }
}
fn occupied(world: &World) -> Vec<At> {
    let mut cells: Vec<_> = KINDS
        .iter()
        .filter(|&&kind| kind != BlockKind::Air)
        .flat_map(|&kind| world.positions_of(kind))
        .map(|flat| world.decode(flat))
        .collect();
    cells.sort_unstable();
    cells
}
fn freeze(out: &Path, name: &str, circuit: &CompiledCircuit) {
    let cells: Vec<_> = occupied(&circuit.world)
        .into_iter()
        .map(|at| (at, circuit.world.get(at.0, at.1, at.2)))
        .collect();
    let encoder = GzEncoder::new(
        File::create(out.join(format!("{name}-world.json.gz"))).unwrap(),
        Compression::default(),
    );
    serde_json::to_writer(encoder,&serde_json::json!({"size":circuit.world.size(),"fingerprint":canonical_world_fingerprint(&circuit.world),"cells":cells})).unwrap();
}

#[derive(Serialize)]
struct Phase {
    from: u32,
    to: u32,
    phase: usize,
    address: u32,
    start: u64,
    ticks: u64,
    processed_queue_events: u64,
    cell_boolean_events: usize,
    // driver channels followed by lamp channels. Metrics: glitches,
    // single-input glitches, last arrival, total pulse width, worst pulse.
    channels: Vec<[u64; 5]>,
}
#[derive(Serialize)]
struct Audit {
    phases: Vec<Phase>,
    status: String,
}

fn audit(
    out: &Path,
    name: &str,
    source: &Netlist,
    circuit: &CompiledCircuit,
    labels: &[String],
) -> Result<Audit, String> {
    let implementation = circuit.implementation_netlist(source);
    let evaluator = Evaluator::new(implementation).map_err(|e| format!("{e:?}"))?;
    let drivers: Vec<_> = if let Some(rewrite) = circuit.physical_rewrite() {
        rewrite.source_output_driver_positions().to_vec()
    } else {
        source
            .outputs
            .iter()
            .map(|s| circuit.gate_output_positions[s])
            .collect()
    };
    let lamps: Vec<_> = circuit
        .output_occurrences()
        .iter()
        .map(|(_, at)| *at)
        .collect();
    if lamps.len() != labels.len() {
        return Err(format!("actual output occurrence count {}", lamps.len()));
    }
    let mut watches = vec![];
    let site = |id, at: At, label| ObservationSite {
        id,
        at: Anchor {
            x: at.0,
            y: at.1,
            z: at.2,
        },
        logical_owner: None,
        display_label: Some(label),
    };
    for (i, (&driver, &lamp)) in drivers.iter().zip(&lamps).enumerate() {
        watches.push(site(
            ObservationId::JunctionOutput(InstanceId(i as u32)),
            driver,
            format!("driver:{}", labels[i]),
        ));
        watches.push(site(
            ObservationId::DeclaredOutput(PortId(i as u32)),
            lamp,
            format!("lamp:{}", labels[i]),
        ));
    }
    for (i, gate) in implementation.gates.iter().enumerate() {
        watches.push(site(
            ObservationId::InstanceOutput(InstanceId(i as u32)),
            circuit.gate_output_positions[&gate.output],
            format!("gate:{}", gate.output),
        ));
    }
    for (i, at) in occupied(&circuit.world).into_iter().enumerate() {
        watches.push(site(
            ObservationId::PrimitiveOutput(PrimitiveId {
                instance: InstanceId(i as u32),
                node: TopologyNodeId(0),
            }),
            at,
            format!("cell:{i}"),
        ));
    }
    write_json(&out.join(format!("{name}-sites.json")), &watches);
    let mut sim = Simulator::new(circuit.world.clone());
    sim.run_until_stable(2000)
        .map_err(|e| format!("startup:{e:?}"))?;
    sim.attach_typed_observer(watches);
    let mut raw = GzEncoder::new(
        File::create(out.join(format!("{name}-events.jsonl.gz"))).unwrap(),
        Compression::default(),
    );
    let mut phases = vec![];
    let mut previous = 0u32;
    for from in 0..(1u32 << source.inputs.len()) {
        for to in 0..(1u32 << source.inputs.len()) {
            for (phase, address) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                let start = sim.current_tick();
                let work = sim.work_done();
                let inputs: BTreeMap<_, _> = source
                    .inputs
                    .iter()
                    .enumerate()
                    .map(|(bit, s)| (s.clone(), address & (1 << bit) != 0))
                    .collect();
                for (s, value) in &inputs {
                    let at = circuit.input_positions[s];
                    let mut state = sim.world().get(at.0, at.1, at.2).clone();
                    state.lit = *value;
                    sim.world_mut().set(at.0, at.1, at.2, state);
                }
                let ticks = sim
                    .run_until_stable_bounded(2000, 1_000_000)
                    .map_err(|e| format!("{name} {from}/{to}/{phase}: {e:?}"))?;
                let implemented_inputs = circuit.physical_rewrite().map_or_else(
                    || inputs.clone(),
                    |rewrite| {
                        rewrite
                            .rewrite_certificate()
                            .rewritten_inputs_in_source_order()
                            .iter()
                            .zip(source.inputs.iter())
                            .map(|(rewritten, s)| (rewritten.clone(), inputs[s]))
                            .collect()
                    },
                );
                let values = evaluator
                    .settle(&implemented_inputs)
                    .map_err(|e| format!("{e:?}"))?;
                for gate in &implementation.gates {
                    let at = circuit.gate_output_positions[&gate.output];
                    if signal(sim.world(), at) != values[&gate.output] {
                        return Err(format!(
                            "{name} {from}/{to}/{phase}: gate {} mismatch",
                            gate.output
                        ));
                    }
                }
                let a = *inputs.get("a").unwrap_or(&false);
                let b = *inputs.get("b").unwrap_or(&false);
                let c = *inputs.get("c").unwrap_or(&false);
                let d = *inputs.get("d").unwrap_or(&false);
                let e = *inputs.get("e").unwrap_or(&false);
                let f = *inputs.get("f").unwrap_or(&false);
                let truth = BTreeMap::from([
                    ("u", a | b),
                    ("z", false),
                    ("o", true),
                    ("v", c & d),
                    ("w", c & d),
                    ("x", c),
                    ("y", e ^ f),
                    ("q", !(e | f)),
                    ("r", f),
                ]);
                let mut channels = vec![];
                for is_lamp in [false, true] {
                    for (index, label) in labels.iter().enumerate() {
                        let at = if is_lamp {
                            lamps[index]
                        } else {
                            drivers[index]
                        };
                        let target = truth[label.as_str()];
                        if signal(sim.world(), at) != target {
                            return Err(format!(
                                "{name} {from}/{to}/{phase}: {}:{label} truth mismatch",
                                if is_lamp { "lamp" } else { "driver" }
                            ));
                        }
                        let id = if is_lamp {
                            ObservationId::DeclaredOutput(PortId(index as u32))
                        } else {
                            ObservationId::JunctionOutput(InstanceId(index as u32))
                        };
                        let events: Vec<_> = sim
                            .typed_observations()
                            .iter()
                            .filter(|e| e.id == id)
                            .collect();
                        let mut metric = [
                            u64::from(events.len() > 1),
                            u64::from(events.len() > 1 && (previous ^ address).count_ones() == 1),
                            events.last().map_or(0, |e| e.tick - start),
                            0,
                            0,
                        ];
                        for pair in events.windows(2) {
                            if pair[0].value != target {
                                let width = pair[1].tick - pair[0].tick;
                                metric[3] += width;
                                metric[4] = metric[4].max(width);
                            }
                        }
                        channels.push(metric);
                    }
                }
                let events: Vec<_> = sim
                    .typed_observations()
                    .iter()
                    .map(|e| {
                        serde_json::json!([
                            e.tick - start,
                            e.id,
                            (e.position.x, e.position.y, e.position.z),
                            e.display_label,
                            e.value
                        ])
                    })
                    .collect();
                serde_json::to_writer(
                    &mut raw,
                    &serde_json::json!({"from":from,"to":to,"phase":phase,"events":events}),
                )
                .unwrap();
                raw.write_all(b"\n").unwrap();
                phases.push(Phase {
                    from,
                    to,
                    phase,
                    address,
                    start,
                    ticks,
                    processed_queue_events: sim.work_done() - work,
                    cell_boolean_events: sim
                        .typed_observations()
                        .iter()
                        .filter(|e| matches!(e.id, ObservationId::PrimitiveOutput(_)))
                        .count(),
                    channels,
                });
                previous = address;
                if phases.len() % 512 == 0 {
                    eprintln!("{name}: {} / 8192 retained phases", phases.len());
                }
            }
        }
    }
    raw.finish().unwrap();
    Ok(Audit {
        phases,
        status: "complete".into(),
    })
}

fn main() {
    let directory = std::env::args().nth(1).expect("output directory");
    let out = Path::new(&directory);
    std::fs::create_dir_all(out).unwrap();
    let selected: Option<String> = None;
    std::fs::write(out.join("source.sv"), SOURCE).unwrap();
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "source.sv",
            text: SOURCE,
        }],
        &CompileOptions::new("m"),
    )
    .unwrap();
    let mut source = lower_optimised(&artifact.netlist).unwrap();
    let mut labels: Vec<_> = artifact
        .ordered_output_ports
        .iter()
        .map(|p| p.name.clone())
        .collect();
    write_json(
        &out.join("source-netlist.json"),
        &serde_json::json!({"inputs":source.inputs,"outputs":source.outputs,"gates":source.gates.iter().map(|g|serde_json::json!({"name":g.name,"kind":format!("{:?}",g.kind),"inputs":g.inputs,"output":g.output})).collect::<Vec<_>>() }),
    );
    write_json(
        &out.join("protocol.json"),
        &serde_json::json!({"baseline_entry": if selected.is_some() { "compile_without_rewrite(exact_root_cone_source)" } else { "reda::compile::compile(lower_optimised(native(source)))" },"candidate_entry": if selected.is_some() { "compile_original_leaf(exact_root_cone_source)" } else { "actual production default compile; generic budget256 local refresh selection" },"labels":labels,"states":(1u64<<source.inputs.len()),"retained_phases":(2u64<<(2*source.inputs.len())),"tick_cap":2000,"queue_event_cap":1_000_000,"metrics":"per-phase settle/processed queue/cell boolean events plus all driver/lamp channels' glitches, single-input glitches, arrival, total/worst pulse; every metric must not regress","observer_policy":"wire power > 0; other cells lit; typed identities preserve shared-site public occurrences"}),
    );
    eprintln!("original/default: compiling the frozen six-input/nine-occurrence source");
    let original = reda::compile::compile_without_rewrite(&source);
    let baseline = match original {
        Ok(c) => c,
        Err(error) => {
            write_json(
                &out.join("baseline-failure.json"),
                &serde_json::json!({"error":format!("{error:?}"),"accepted":false}),
            );
            eprintln!("actual default refused: {error:?}");
            return;
        }
    };
    freeze(out, "baseline", &baseline);
    eprintln!("original/default: frozen; candidate: generating");
    let candidate = compile(&source).unwrap();
    freeze(out, "candidate", &candidate);
    let baseline_audit = audit(out, "baseline", &source, &baseline, &labels);
    let candidate_audit = audit(out, "candidate", &source, &candidate, &labels);
    match (baseline_audit, candidate_audit) {
        (Ok(b), Ok(c)) => {
            let dominates = c.phases.iter().zip(&b.phases).all(|(c, b)| {
                c.ticks <= b.ticks
                    && c.processed_queue_events <= b.processed_queue_events
                    && c.cell_boolean_events <= b.cell_boolean_events
                    && c.channels
                        .iter()
                        .zip(&b.channels)
                        .all(|(c, b)| c.iter().zip(b).all(|(c, b)| c <= b))
            });
            write_json(&out.join("baseline-phases.json"), &b);
            write_json(&out.join("candidate-phases.json"), &c);
            write_json(
                &out.join("result.json"),
                &serde_json::json!({"accepted":dominates,"baseline_phases":b.phases.len(),"candidate_phases":c.phases.len(),"scope":"this frozen whole-default six-input/nine-port comparison only"}),
            );
            eprintln!("whole-default non-regression: {dominates}");
        }
        (b, c) => {
            write_json(
                &out.join("audit-failure.json"),
                &serde_json::json!({"baseline":b.err(),"candidate":c.err(),"accepted":false}),
            );
        }
    }
}

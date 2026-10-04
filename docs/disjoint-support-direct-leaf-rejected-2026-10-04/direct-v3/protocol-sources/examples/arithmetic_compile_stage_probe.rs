//! Diagnostic stage timing only; this is not counter trace qualification.
use reda::compile::{compile, lowering::lower_optimised};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use std::time::Instant;

fn main() {
    let started = Instant::now();
    let text = include_str!("../tests/fixtures/arithmetic_counter_add.sv")
        .replace("posedge", "negedge");
    let artifact = compile_systemverilog(
        &[SourceInput { name: "arithmetic_counter_add", text: &text }],
        &CompileOptions::new("arithmetic_counter_add"),
    ).expect("native source compiles");
    let lowered = lower_optimised(&artifact.netlist).expect("lowering");
    eprintln!("ARITHMETIC_STAGE compile_begin seconds={:.3} netlist={lowered:?}", started.elapsed().as_secs_f64());
    let physical = compile(&lowered).expect("physical compilation");
    eprintln!("ARITHMETIC_STAGE compile_done seconds={:.3} kind={:?} size={:?}", started.elapsed().as_secs_f64(), physical.planner_kind(), physical.world.size());
    let mut simulator = Simulator::new(physical.world);
    eprintln!("ARITHMETIC_STAGE power_on_begin seconds={:.3}", started.elapsed().as_secs_f64());
    simulator.run_until_stable(2000).expect("original power-on ceiling");
    eprintln!("ARITHMETIC_STAGE power_on_done seconds={:.3} ticks={}", started.elapsed().as_secs_f64(), simulator.current_tick());
}

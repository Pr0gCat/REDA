use reda::frontend::{compile_systemverilog_physical, CompileOptions, PortDirection, SourceInput};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};

#[test]
fn selected_and_forward_declared_readers_observe_the_same_physical_bus() {
    let fixture = include_str!("fixtures/resolved-inout.sv");
    let source = fixture.replace("output wire y", "output wire [3:0] y").replace(
        "assign y = bus;",
        "assign y[0]=bus; assign y[2:1]={~bus,bus}; assign {y[3]}=~late; assign late=bus; wire late;",
    );
    let circuit = compile_systemverilog_physical(
        &[SourceInput { name: "selected-readers.sv", text: &source }], &CompileOptions::new("top"),
    ).unwrap();
    assert_eq!(circuit.pinout.inouts.len(), 1);
    let bus = &circuit.pinout.inouts[0];
    assert_eq!(bus.source.name, "bus");
    assert_eq!(bus.source.direction, PortDirection::Inout);
    assert_eq!(bus.channel.len(), 11);
    assert_eq!(circuit.pinout.inputs.len(), 4);
    assert_eq!(circuit.pinout.outputs.len(), 4);
    let mut simulator = Simulator::new(circuit.physical.world.clone());
    for value in [false, true, false] {
        for (name, position) in &circuit.pinout.inputs {
            let mut lever = simulator.world().get(position[0], position[1], position[2]).clone();
            assert_eq!(lever.kind, BlockKind::Lever);
            lever.lit = match name.as_str() { "e0" => true, "d0" => value, "e1" | "d1" => false, _ => panic!("unexpected source input") };
            simulator.world_mut().set(position[0], position[1], position[2], lever);
        }
        simulator.run_until_stable(2000).unwrap();
        assert_eq!(bus.observe(simulator.world()), Ok(value));
        for (name, expected) in [("y[0]",value),("y[1]",value),("y[2]",!value),("y[3]",!value)] {
            let position = circuit.pinout.outputs[name];
            let lamp = simulator.world().get(position[0], position[1], position[2]);
            assert_eq!(lamp.kind, BlockKind::Lamp);
            assert_eq!(lamp.lit, expected, "{name} bus={value}");
        }
    }
}

use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
fn native(text: &str, top: &str) -> reda::frontend::CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "radix.sv",
            text,
        }],
        &CompileOptions::new(top),
    )
    .unwrap()
}
#[test]
fn radix_spellings_keep_the_qualified_wide_rom_source_graph() {
    let hex = include_str!("../docs/wide-collector-experiment-2026-10-03/wide_rom.sv");
    let binary = include_str!("../docs/wide-collector-experiment-2026-10-03/wide_rom_binary.sv");
    let a = native(hex, "wide_rom");
    let b = native(binary, "wide_rom");
    assert_eq!(a.netlist, b.netlist);
    assert_eq!(a.output_map(), b.output_map());
    let oracle = Evaluator::new(&a.netlist).unwrap();
    let words: [u16; 16] = [
        0x9653, 0x2ad4, 0xc138, 0x73e1, 0x4b29, 0xe592, 0x187f, 0xab64, 0x6d83, 0xf04a, 0x35bc,
        0x82d7, 0x59e8, 0xd126, 0x07fb, 0xbe45,
    ];
    for address in 0..16 {
        let inputs = (0..4)
            .map(|bit| {
                (
                    a.port(&format!("a[{bit}]")).unwrap().signal.clone(),
                    address & (1 << bit) != 0,
                )
            })
            .collect();
        let values = oracle.evaluate(&inputs).unwrap();
        for bit in 0..16 {
            assert_eq!(
                values[&a.output_map()[&format!("q[{bit}]")]],
                words[address] & (1 << bit) != 0
            );
        }
    }
}
#[test]
fn octal_uppercase_and_partial_digits_preserve_two_state_values() {
    for spelling in ["5'o25", "5'O025", "5'h15", "5'H0_15"] {
        let a = native(
            &format!("module m(output [4:0] q); assign q={spelling}; endmodule"),
            "m",
        );
        let fixed = native(
            "module m(output [4:0] q); assign q=5'b10101; endmodule",
            "m",
        );
        assert_eq!(a.netlist, fixed.netlist);
    }
    for spelling in ["5'h35", "5'o65", "8'hxF", "8'oz7", "8'o8"] {
        assert!(compile_systemverilog(
            &[SourceInput {
                name: "bad.sv",
                text: &format!("module m(output [7:0] q);assign q={spelling};endmodule")
            }],
            &CompileOptions::new("m")
        )
        .is_err());
    }
}

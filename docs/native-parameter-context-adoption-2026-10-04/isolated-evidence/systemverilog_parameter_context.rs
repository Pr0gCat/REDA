use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};

fn word_width(source: &str, width: u32) -> u64 {
    let artifact = compile_systemverilog(&[SourceInput { name: "parameter_context.sv", text: source }], &CompileOptions::new("m"))
        .unwrap_or_else(|errors| panic!("{errors:?}"));
    let values = Evaluator::new(&artifact.netlist).unwrap().evaluate(&Default::default()).unwrap();
    (0..width).fold(0, |value, bit| value | ((values[&artifact.output_map()[&format!("y[{bit}]")]] as u64) << bit))
}

#[test]
fn declaration_context_reaches_arithmetic_before_truncation() {
    for (expression, expected) in [
        ("4'hf+4'h1", 16), ("8'hf+8'h1", 16),
        ("(4'hf+4'h1)*4'h2", 32), ("4'h0-4'h1", 255),
        ("(4'shf+4'sh1)+8'h1", 17), ("-(4'sh8)+8'h1", 249),
    ] {
        let source = format!("module m #(parameter logic [7:0] P={expression})(output [7:0] y); assign y=P; endmodule");
        assert_eq!(word(&source), expected, "{source}");
    }
}

#[test]
fn dependent_names_keep_their_own_declaration_context() {
    for (declaration, expected) in [("logic [7:0]",17), ("",1)] {
        let source = format!("module m #(parameter {declaration} P=4'hf+4'h1, parameter logic [7:0] Q=P+4'h1)(output [7:0] y); assign y=Q; endmodule");
        assert_eq!(word(&source), expected, "{source}");
    }
}

#[test]
fn parent_overrides_are_self_determined_and_defaults_use_overridden_width() {
    for (parent, override_text, expected) in [
        ("", ".W(8),.P(4'hf+4'h1)", 0),
        ("", "8,4'hf+4'h1", 0),
        ("", ".W(8)", 16),
        ("#(parameter logic [7:0] V=4'hf+4'h1)", ".W(8),.P(V)", 16),
    ] {
        let source = format!("module child #(parameter W=4, parameter logic [W-1:0] P=4'hf+4'h1)(output [7:0] y); assign y=P; endmodule module m {parent}(output [7:0] y); child #({override_text}) u(.y(y)); endmodule");
        assert_eq!(word(&source), expected, "{source}");
    }
}

fn word(source: &str) -> u64 { word_width(source, 8) }

#[test]
fn minimum_literal_matches_signed_and_unsigned_wide_contexts() {
    for (literal, expected) in [("64'h0",0xffffffff80000000), ("64'sh0",0xffffffff80000000)] {
        let source = format!("module m #(parameter logic [63:0] P=(-2147483648)+{literal})(output [63:0] y); assign y=P; endmodule");
        assert_eq!(word_width(&source,64), expected, "{source}");
    }
}

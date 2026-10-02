use reda::frontend::debug::SyntheticOrigin;
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use std::collections::BTreeMap;

fn compile(text: &str) -> reda::frontend::CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "reset.sv",
            text,
        }],
        &CompileOptions::new("m"),
    )
    .unwrap_or_else(|errors| panic!("{errors:?}"))
}

#[test]
fn conditional_reset_priority_and_hold_follow_literal_edges() {
    for edge in ["posedge", "negedge"] {
        for body in [
            "if (reset) q <= 1'b0; else if (en) q <= d;",
            "if (reset) begin q <= 1'b0; end else begin if (en) begin q <= d; end end",
            "if (reset) q <= 1'b0; else if (en) q <= d; else q <= q;",
        ] {
            for legacy in [false, true] {
                let keyword = if legacy { "always" } else { "always_ff" };
                let text = format!("module m(input clk,reset,en,d,output reg q); {keyword} @({edge} clk) {body} endmodule");
                let artifact = compile(&text);
                let repeat = compile(&text);
                assert_eq!(artifact.netlist, repeat.netlist);
                assert_eq!(artifact.debug.to_json(), repeat.debug.to_json());
                assert!(artifact.debug.matches(&artifact.netlist));
                assert!(artifact
                    .debug
                    .logic_origins
                    .iter()
                    .any(|origin| origin.synthetic == Some(SyntheticOrigin::BranchJoin)));
                let q_signal = artifact.output_map()["q"].clone();
                let mut evaluator = Evaluator::new(&artifact.netlist).unwrap();
                let (mut q, mut previous) = (false, false);
                for cycle in 0..64 {
                    for (control, clock) in [
                        (cycle % 8, false),
                        (cycle % 8, true),
                        ((cycle % 8) ^ 7, true),
                        ((cycle % 8) ^ 7, false),
                    ] {
                        let (reset, en, d) = (control & 1 != 0, control & 2 != 0, control & 4 != 0);
                        if (edge == "posedge" && !previous && clock)
                            || (edge == "negedge" && previous && !clock)
                        {
                            if reset {
                                q = false;
                            } else if en {
                                q = d;
                            }
                        }
                        previous = clock;
                        let inputs = BTreeMap::from([
                            ("clk".into(), clock),
                            ("reset".into(), reset),
                            ("en".into(), en),
                            ("d".into(), d),
                        ]);
                        assert_eq!(
                            evaluator.step(&inputs).unwrap()[&q_signal],
                            q,
                            "{text}, cycle={cycle}, control={control}, clock={clock}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn dangling_else_binds_to_nearest_if_and_missing_outer_branch_holds() {
    let artifact = compile("module m(input clk,a,b,d,output reg q); always @(posedge clk) if(a) if(b) q<=d; else q<=1'b0; endmodule");
    let mut evaluator = Evaluator::new(&artifact.netlist).unwrap();
    let q_signal = artifact.output_map()["q"].clone();
    let mut q = false;
    for control in [7, 0, 5, 7, 6, 3] {
        let (a, b, d) = (control & 1 != 0, control & 2 != 0, control & 4 != 0);
        let mut inputs = BTreeMap::from([
            ("clk".into(), false),
            ("a".into(), a),
            ("b".into(), b),
            ("d".into(), d),
        ]);
        evaluator.step(&inputs).unwrap();
        inputs.insert("clk".into(), true);
        if a {
            q = b && d;
        }
        assert_eq!(evaluator.step(&inputs).unwrap()[&q_signal], q);
    }
}

#[test]
fn conditional_branches_validate_targets_widths_and_nonblocking_writes() {
    for (body, expected) in [
        ("if(reset) q<=2'b00; else r<=2'b11;", "same register slice"),
        (
            "if(reset) q<=2'b00; else q[0]<=1'b1;",
            "same register slice",
        ),
        ("if(reset) q<=2'b00; else q<=1'b1;", "width mismatch"),
        ("if(reset) q<=2'b00; else q=2'b11;", "nonblocking"),
        ("if(reset) begin q<=2'b00; q<=2'b11; end", "expected"),
    ] {
        let text=format!("module m(input clk,reset,output reg [1:0] q,r); always @(posedge clk) {body} endmodule");
        let errors = compile_systemverilog(
            &[SourceInput {
                name: "reset.sv",
                text: &text,
            }],
            &CompileOptions::new("m"),
        )
        .expect_err("invalid tree accepted");
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "{errors:?}"
        );
    }
}

#[test]
fn statement_reset_reuses_existing_next_state_netlists() {
    for (statement, expression, top) in [
        (
            include_str!("fixtures/reset_counter.sv"),
            include_str!("fixtures/feedback_counter.sv"),
            "counter",
        ),
        (
            include_str!("fixtures/reset_fsm.sv"),
            include_str!("fixtures/feedback_fsm.sv"),
            "fsm",
        ),
        (
            include_str!("fixtures/conditional_state_enable.sv"),
            include_str!("fixtures/feedback_state_enable.sv"),
            "state_enable",
        ),
    ] {
        let statement = if top == "state_enable" {
            statement.replace("module conditional_state_enable", "module m")
        } else {
            statement.replace(&format!("module reset_{top}"), "module m")
        };
        let expression = expression.replace(&format!("module feedback_{top}"), "module m");
        assert_eq!(
            compile(&statement).netlist,
            compile(&expression).netlist,
            "{top}"
        );
    }
}

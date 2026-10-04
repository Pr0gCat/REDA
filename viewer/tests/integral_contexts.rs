use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, DebugDatabase, SourceInput};

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn signed_context_truth_and_sidecars_survive_native_and_wasm_consumers() {
    for (expression, signed_result) in [("a", true), ("~a", true), ("~a & 8'hff", false)] {
        let text = format!(
            "module m(input signed [3:0] a,output [7:0] y); assign y={expression}; endmodule"
        );
        let compile = || {
            compile_systemverilog(
                &[SourceInput {
                    name: "context.sv",
                    text: &text,
                }],
                &CompileOptions::new("m"),
            )
            .unwrap()
        };
        let artifact = compile();
        let encoded = artifact.debug.to_json();
        // New context fixtures are frozen from the independently checked native
        // truth/sidecar control; existing historical fixture hashes stay untouched.
        let expected_hash = match expression {
            "a" => "5a2e231e73c9b58b85996afa1e6f277e079066dc558c079a3f9adcc64e4c63d3",
            "~a" => "fd430c8e86e00912daedfbff474c97c57c33a3996c41a17e52d055c8ef9d6fb0",
            "~a & 8'hff" => "983214667c8ebd38bcb6a4aa93ece39d8b5b985d3b5c377dcd8da5b2b60c3361",
            _ => unreachable!(),
        };
        assert_eq!(
            reda::compile::metrics::canonical_fingerprint(encoded.as_bytes()).as_str(),
            expected_hash
        );
        let restored = DebugDatabase::from_json(&encoded).unwrap();
        assert!(restored.matches(&artifact.netlist));
        assert_eq!(restored.to_json(), encoded);
        assert_eq!(compile().debug.to_json(), encoded);
        for origin in restored
            .logic_origins
            .iter()
            .chain(restored.extra_origins.iter().map(|(_, origin)| origin))
        {
            assert!(origin.bit < restored.elab_nodes[origin.elab.0 as usize].width);
        }
        let evaluator = Evaluator::new(&artifact.netlist).unwrap();
        for word in 0usize..16 {
            let inputs = (0..4)
                .map(|bit| (format!("a[{bit}]"), word & (1 << bit) != 0))
                .collect();
            let values = evaluator.evaluate(&inputs).unwrap();
            let widened = if signed_result && word & 8 != 0 {
                word | 240
            } else {
                word
            };
            let expected = if expression == "a" {
                widened
            } else {
                (!widened) & 255
            };
            assert_eq!(artifact.output_map().len(), 8);
            for bit in 0..8 {
                assert_eq!(
                    values[&artifact.output_map()[&format!("y[{bit}]")]],
                    expected & (1 << bit) != 0,
                    "{expression}, word {word}, bit {bit}"
                );
            }
        }
    }
}

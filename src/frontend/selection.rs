//! Native-first host selection. Placement and verification run after selection
//! and cannot trigger fallback.
use super::{
    compile_systemverilog, synthesize_verilog, CompileOptions, DiagnosticKind, SourceInput,
};
use crate::compile::Netlist;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontendMode {
    Auto,
    Native,
    Yosys,
}
impl FrontendMode {
    pub fn take_argument(args: &mut Vec<String>) -> Result<Self, String> {
        let Some(index) = args.iter().position(|arg| arg == "--frontend") else {
            return Ok(Self::Auto);
        };
        args.remove(index);
        if index >= args.len() {
            return Err("--frontend requires auto, native, or yosys".into());
        }
        let value = args.remove(index);
        let mode = match value.as_str() {
            "auto" => Self::Auto,
            "native" => Self::Native,
            "yosys" => Self::Yosys,
            _ => {
                return Err(format!(
                    "unknown frontend `{value}`; use auto, native, or yosys"
                ))
            }
        };
        if args.iter().any(|arg| arg == "--frontend") {
            return Err("--frontend may only be supplied once".into());
        }
        Ok(mode)
    }
}

pub struct SelectedFrontend {
    pub netlist: Netlist,
    pub outputs: HashMap<String, String>,
    /// Native source-order public occurrences; legacy Yosys labels stay sorted.
    pub output_bindings: Vec<(String, String)>,
    pub frontend: FrontendMode,
    pub fallback_reason: Option<String>,
}

pub fn synthesize_selected(
    source: SourceInput<'_>,
    top: &str,
    mode: FrontendMode,
) -> Result<SelectedFrontend, String> {
    select_with(source, top, mode, synthesize_verilog)
}

fn select_with(
    source: SourceInput<'_>,
    top: &str,
    mode: FrontendMode,
    yosys: impl FnOnce(&str, &str) -> Result<(Netlist, HashMap<String, String>), super::FrontendError>,
) -> Result<SelectedFrontend, String> {
    let mut fallback_reason = None;
    if mode != FrontendMode::Yosys {
        match compile_systemverilog(std::slice::from_ref(&source), &CompileOptions::new(top)) {
            Ok(artifact) => {
                let outputs = artifact.output_map();
                let output_bindings = artifact.ordered_output_ports.iter()
                    .map(|port| (port.name.clone(),port.signal.clone())).collect();
                return Ok(SelectedFrontend {
                    netlist: artifact.netlist,
                    outputs,
                    output_bindings,
                    frontend: FrontendMode::Native,
                    fallback_reason,
                });
            }
            Err(diagnostics) => {
                let reason = diagnostics
                    .iter()
                    .map(|d| d.render(std::slice::from_ref(&source)))
                    .collect::<Vec<_>>()
                    .join("\n");
                if mode != FrontendMode::Auto || !allows_fallback(&diagnostics) {
                    return Err(reason);
                }
                fallback_reason = Some(reason);
            }
        }
    }
    let (netlist, outputs) = yosys(source.text, top).map_err(|error| match &fallback_reason {
        Some(reason) => {
            format!("native frontend unsupported: {reason}\nYosys fallback failed: {error}")
        }
        None => error.to_string(),
    })?;
    let mut output_bindings: Vec<_> = outputs.iter()
        .map(|(label,signal)| (label.clone(),signal.clone())).collect();
    output_bindings.sort();
    Ok(SelectedFrontend {
        netlist,
        outputs,
        output_bindings,
        frontend: FrontendMode::Yosys,
        fallback_reason,
    })
}

fn allows_fallback(diagnostics: &[super::Diagnostic]) -> bool {
    !diagnostics.is_empty()
        && diagnostics
            .iter()
            .all(|d| d.kind == DiagnosticKind::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn yosys_selection_keeps_legacy_sorted_labels() {
        let selected=select_with(SourceInput{name:"unused",text:"unused"},"m",FrontendMode::Yosys,
            |_,_| Ok((Netlist{inputs:vec![],outputs:vec!["g0".into(),"g0".into()],gates:vec![]},
                HashMap::from([("z".into(),"g0".into()),("a".into(),"g0".into())])))).unwrap();
        assert_eq!(selected.output_bindings,vec![("a".into(),"g0".into()),("z".into(),"g0".into())]);
        assert_eq!(selected.netlist.outputs,vec!["g0","g0"]);
    }
    #[test]
    fn errors_and_verification_failures_never_allow_fallback() {
        for kind in [DiagnosticKind::Source, DiagnosticKind::Internal] {
            let diagnostic = super::super::Diagnostic {
                kind,
                severity: super::super::Severity::Error,
                message: "unsupported: misleading message, including verification failure".into(),
                span: super::super::source::Span::new(super::super::source::FileId(0), 0, 0),
            };
            assert!(!allows_fallback(&[diagnostic.clone()]));
            let unsupported = super::super::Diagnostic {
                kind: DiagnosticKind::Unsupported,
                ..diagnostic.clone()
            };
            assert!(!allows_fallback(&[unsupported, diagnostic]));
        }
        assert!(!allows_fallback(&[]));
    }
    #[test]
    fn native_success_and_source_errors_do_not_invoke_yosys() {
        for (text, ok) in [
            ("module m(input a, output y); assign y = a; endmodule", true),
            ("module m(input a, output y); assign y = ; endmodule", false),
            (
                "module m(input a, output y); assign y = missing; endmodule",
                false,
            ),
        ] {
            let result = select_with(
                SourceInput {
                    name: "test.sv",
                    text,
                },
                "m",
                FrontendMode::Auto,
                |_, _| panic!("Yosys must not run"),
            );
            assert_eq!(result.is_ok(), ok);
        }
    }
}

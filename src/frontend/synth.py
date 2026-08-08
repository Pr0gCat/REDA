#!/usr/bin/env python3
"""Synthesis driver for REDA's Verilog frontend.

Invoked as a subprocess by `src/frontend/mod.rs`:

    python synth.py <verilog-file> <top-module> <genlib-file> <output-json>

Runs the Verilog source through Yosys (via `yowasp-yosys`, a WASM build of
Yosys with ABC built in) and technology-maps every bit of combinational logic
onto exactly the NOR1/NOR2/NOR3/BUF cells described in `redstone_nor.genlib`
in this same directory -- see that file for where its area/delay numbers
come from. `write_json` then dumps the mapped netlist for the Rust side to
read (`src/frontend/yosys_json.rs`).

Requires the `yowasp-yosys` package (`pip install yowasp-yosys`, or see the
project's `requirements.txt`). If it is missing, this prints a message that
says so explicitly and exits non-zero, rather than leaking a bare
`ModuleNotFoundError` traceback at whoever is running the frontend.

`proc -norom` (rather than plain `proc`) matters here: Yosys's default `proc`
script includes `proc_rom`, which recognizes a `case` statement that just
maps an index to constants (exactly what a segment decoder's truth table
looks like) and turns it into a `$mem` primitive instead of logic. `abc`
cannot map a `$mem` cell against any genlib, so without `-norom` a decoder
written the natural way (a `case` per digit) fails to synthesize at all.
"""
import os
import sys


def main() -> int:
    if len(sys.argv) != 5:
        print("usage: synth.py <verilog-file> <top-module> <genlib-file> <output-json>", file=sys.stderr)
        return 2

    # Normalise every path to forward slashes up front. Yosys's own argument
    # parser (for `-l`) and its script language (for `read_verilog` et al.)
    # both run inside a WASI sandbox that maps Windows drive letters as
    # POSIX-style mounts (see yowasp_runtime's `preopen_dir` calls) -- a
    # literal backslash is not a path separator there, so a raw Windows path
    # silently fails to open rather than erroring clearly.
    verilog_file, top_module, genlib_file, output_json = (arg.replace("\\", "/") for arg in sys.argv[1:5])

    try:
        from yowasp_yosys import run_yosys
    except ImportError:
        print(
            "reda: the Verilog frontend needs the `yowasp-yosys` Python package "
            "(pip install yowasp-yosys, or `pip install -r requirements.txt`), "
            "which is not installed in this Python environment.",
            file=sys.stderr,
        )
        return 3

    def quoted(path: str) -> str:
        # Yosys script commands accept a double-quoted filename. Normalising
        # to forward slashes sidesteps any ambiguity around backslashes in
        # the script language's own escaping rules on Windows paths.
        return '"' + path.replace("\\", "/") + '"'

    script = f"""
read_verilog {quoted(verilog_file)}
hierarchy -check -top {top_module}
proc -norom
opt
techmap
opt
abc -genlib {quoted(genlib_file)}
opt_clean -purge
write_json {quoted(output_json)}
"""

    log_file = output_json + ".yosys.log"

    # `run_yosys` does NOT raise on a Yosys-level error (a missing top
    # module, a Verilog syntax error, ABC failing to map the design) -- it
    # prints "ERROR: ..." to the log and returns normally either way. The
    # only reliable success signal is whether the output file actually
    # showed up, so that -- not the return value -- is what this checks.
    try:
        run_yosys(["-q", "-l", log_file, "-p", script])
    except SystemExit as exc:
        code = exc.code if isinstance(exc.code, int) else 1
        if code != 0:
            _report_failure(verilog_file, log_file)
            return code
    except Exception as exc:  # pragma: no cover - defensive
        print(f"reda: yosys raised an unexpected error: {exc}", file=sys.stderr)
        _report_failure(verilog_file, log_file)
        return 1

    if not os.path.exists(output_json):
        _report_failure(verilog_file, log_file)
        return 1

    return 0


def _report_failure(verilog_file: str, log_file: str) -> None:
    print(f"reda: synthesizing `{verilog_file}` failed; yosys did not produce an output JSON.", file=sys.stderr)
    try:
        with open(log_file, encoding="utf-8", errors="replace") as f:
            lines = f.readlines()
    except OSError:
        print(f"reda: (could not read yosys's own log at `{log_file}`)", file=sys.stderr)
        return

    error_lines = [line for line in lines if "ERROR" in line]
    relevant = error_lines if error_lines else lines[-20:]
    print("reda: relevant yosys log output:", file=sys.stderr)
    for line in relevant:
        print("  " + line.rstrip("\n"), file=sys.stderr)


if __name__ == "__main__":
    sys.exit(main())

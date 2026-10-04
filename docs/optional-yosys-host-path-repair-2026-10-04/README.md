# Optional Yosys host temporary paths

The original forced-Yosys control fails even with the installed repository Python environment: YoWASP maps its private scratch directory to `/tmp`, hiding REDA host input/output paths under `/tmp`. The retained failure reports inability to open the output log. Installed runtime preopens the host working directory and its ancestors for relative paths.

The synthesis driver now makes input and output paths relative before constructing the Yosys script and log path. Source processing, mapping, output validation and failure reporting remain unchanged. Absolute paths on different Windows drives remain available when relative conversion is impossible; the drive mounts do not conflict with `/tmp`. No environment mounts or access policy are widened.

The original native/auto/forced-Yosys truth control now passes. Five existing selection controls pass together, covering explicit unsupported-source fallback, real CLI forced/fallback execution, missing-interpreter native CLI behavior and failure propagation. The long legacy native physical CLI control is running independently; it is not claimed passed by the five-control result. No tests or assertions change. Python syntax also checks successfully. Logs are retained losslessly as gzip. Qualification uses the shared integration worktree; user arithmetic remains unstaged.

This repairs optional independent comparison. It does not establish every required HDL feature without Yosys or whole-project convergence.

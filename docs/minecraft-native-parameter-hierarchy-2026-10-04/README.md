# Native typed parameter hierarchy in Minecraft26.2

The clean source-bound native compiler generates the retained candidate.dump directly from source.sv. The child width override W=8 changes default initializer `4'hf+4'h1` from narrow zero to16, and the source function is y=a XOR16. All4 source inputs,8 public occurrences (including three separate constant-low outputs) and285 actual implemented gates are preserved in the dump. The clean producer is b6d737a plus the precisely owned parameter patch; clean-source-manifest.json and producer-terminal.json bind source/test qualification, executable and dump. This is that exact clean candidate, not a fresh build of later current HEAD.

Official Minecraft Java26.2 on the existing server completes16 input vectors with128 literal-source public truth checks and4560 implementation-gate checks. Root's independent audit reconstructs every reported gate and public value from62854 raw real RCON command/reply pairs, then compares them against the literal source function and the typed actual-gate graph. FROZEN-SHA256SUMS verifies16 pre-game input/protocol files. No mocked RCON, simulator substitute, physical-rule change or settling-budget increase is used. The unchanged existing harness runs at its default settling floors/ceiling. This is stable-state conformance, not intra-tick waveform quality.

Preflight checks all30 region chunks unforced. The original preflight failed only because the copied protocol lacked its existing harness import; that setup failure and initial manifest remain. The unmodified missing dependency is then copied, the complete protocol is frozen, and qualified preflight succeeds. The game succeeds on the existing server without restarting or changing its tick/physics settings. Private credentials remain only in the server's properties file.

Cleanup is independently checked against all3319 original occupied positions and8 boundary samples as Air. All30 region chunks are queried before and after the audit's own temporary loading and are unforced; the game's actual release acknowledgement is retained. This verifies original occupied positions and boundary samples, not every voxel of the full bounding box.

Recheck retained evidence:

```sh
python3 audit-game.py
```

To repeat the game, use a separate copy and a separate reviewed origin; preserve these artifacts. Run the frozen run-game.py with candidate.dump, the existing properties file and origin210000,151,180000, then audit-cleanup.py and audit-game.py. The wrapper invokes the actual existing RCON client and logs its replies verbatim.

Broader combinational/sequential game coverage, standard bidirectional inout and all eight full requirements remain open. This game result does not replace original-controlled continuous waveform qualification.

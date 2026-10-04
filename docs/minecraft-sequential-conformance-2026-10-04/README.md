# Sequential Minecraft conformance: prepared, not executed

The mixed-clock reset/feedback driver is prepared for the qualified 264-phase protocol. It observes all 15 gate outputs, both public lamps, and all 10 physical DFF cell sites, including actual repeater locking. Input changes occur in a frozen game tick. Native datapack snapshots cover each sampled game-tick boundary atomically.

Offline qualification: three producer tests and fifteen Python tests pass, with no skipped tests. The Python previous-state oracle matches every frozen Rust reference phase, internal gate, cell value, cell lock, and source state. Nonsequential and4 dump output remains byte-identical; its SHA-256 is `a370a647904e39c2de6c992e552ae7c8239b4817f1167e359f7ccdae035386a1`. Sources, the final linked producer executable, immutable library, and dump are bound in `offline/oracle-parity.json`. The offline verification script is repeatable from the repository root while its recorded `/tmp` artifacts remain available.

No real Minecraft sequential execution has occurred. Automatic approval review rejected the experiment before process creation because exact server-owner authorization was not established. See `approval-rejection.json`. No credential read, RCON connection, datapack installation, or world mutation resulted from that denied command. A new exact authorization must be supplied before retrying.

The stock server does not expose intra-tick scheduled events or an empty simulator event heap. The proposed settlement criterion is full observation agreement within the existing 2000-game-tick cap, followed by the existing 16-game-tick dwell and final agreement. This is an explicit game-observation criterion, not a claim of simulator settlement equivalence. Full tick snapshots, changes, command responses, first failure, and cleanup errors will be retained when execution is authorized.

The generated `offline/capture.mcfunction` is an offline protocol artifact only; it has not been installed in the server world. Setup refuses existing output/datapack directories and cleanup removes only directories created by this invocation.

The dedicated mixed-clock literal oracle now verifies the qualified lowered gate graph and public output bindings before use. Mutants retaining the same input, DFF, and public names but changing data logic or swapping public state bindings are rejected. This guard is confined to the dedicated oracle; the generic dump parser still accepts other valid circuits.

## Independent root qualification

The precise producer/parser patches apply to the independently qualified committed-source legacy tree without dirty user code. All three producer tests pass and generate the real Rust fixture dumps. With those fixtures supplied, all 17 conformance Python tests pass with zero skips, including the existing combinational rewrite consumers. The dedicated oracle additionally rejects same-interface altered DFF logic and swapped public state bindings. All 264 offline state/gate/cell/locking phases match the retained literal reference. These are offline tool contracts only; automatic approval denial remains effective for real sequential game execution.

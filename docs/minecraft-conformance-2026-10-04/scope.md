# Actual Minecraft 26.2 combinational conformance

The owner explicitly accepted the Minecraft EULA and authorized the local server. Official Minecraft26.2 jar SHA1 is independently rechecked against the installed Mojang metadata. The server binds game and RCON endpoints to localhost. Empty-server pausing initially stopped world ticks despite normal RCON responses; the server was cleanly stopped, pause-when-empty-seconds changed from60 to0, and restarted at its unchanged20TPS rate. Initial and later server logs retain this observation. No simulator or physical verification rules changed.

A clean aef7047 library compiled the retained mc_dump producer. Native-default full seven segment is generated with an explicitly nonexistent REDA_PYTHON. Two independently cleared/rebuilt real-game runs execute every16-input vector and read all206 implemented gates and seven public lamps until the existing quiescence condition. Both pass with zero mismatches; their complete vector, expected-gate and actual-gate/output reports are identical. Original-default and4 also passes all16 vectors and every implemented gate. These are actual server/RCON observations, not simulator or mock results.

The unchanged32 primitive probes report30 matches, two disagreements and zero errors. Both disagreements concern wire climbing on a glass step; the conducts_glass probe also measures climbing and does not establish glass electrical conduction. These negative results are preserved rather than changing expected values or physics to declare a pass.

This is stable-state conformance with existing ceilings/polls, not complete per-game-tick waveform or full sequential/larger-ROM coverage. Gap7 remains open. Exact original protocol sources, input dumps, results and logs are retained here, along with source/library/executable/jar hashes and nonsecret server configuration. RCON credentials and the server world/properties file are excluded.

Reproduce native dump and actual-game checks:

```sh
REDA_PYTHON=/nonexistent/reda-python mc_dump --frontend native verilog:seven_segment > seven_segment.dump
python3 conformance/circuit_conformance.py --dump seven_segment.dump --properties minecraft-server-26.2/server/server.properties --out seven-segment.json --label 26.2-native-default
python3 conformance/run.py --properties minecraft-server-26.2/server/server.properties --out primitive-rules.json --label 26.2
```

The independent second rebuild uses the same dump and normal clear/release lifecycle. Primitive and circuit runs have separate physical regions; and4 completed before the primitive harness released its historical global forceload. Future parallel primitive cleanup is being scoped to its owned region; no primitive/circuit test overlaps that global-release operation in these accepted records.

# Task 4: official `mc_dump` conformance

## Integration under test

- Integration commit: `976898406b25c4329642caa3c1d3e5f4a57c5377`
  (`fix(mc_dump): optimise Verilog dump lowering`)
- Lowering path: catalog `verilog:...` and file-form `mc_dump verilog <file.v>
  <top>` use `lower_optimised`; hand-written catalog circuits retain ordinary
  `lower` compatibility lowering.
- No fan-in-packing code is present or introduced by this integration.

## Official dump

Exact command, from the integration commit:

```powershell
cargo run --release --bin mc_dump -- verilog:seven_segment > C:\Users\LTY\AppData\Local\Temp\reda-task4-9768984-verilog-seven-segment.txt
```

Result:

| record | count |
|---|---:|
| `GATE` | 47 |
| `GATEOUT` | 47 |
| `BLOCK` | 10,088 |

SHA-256 of that dump:

```text
CA50ED0E7D13169AD0EA0264F4AB23F89088352C31D3A37EF2C4834E269DBB2D
```

The black-box regression is `tests/mc_dump.rs`:
`official_verilog_seven_segment_dump_uses_optimised_lowering`. Replacing the
Verilog path with ordinary `lower` makes it observe the former 56 `GATE` /
12,348 `BLOCK` dump instead.

## Fresh Minecraft 1.20.1 conformance

There was no RCON listener before the run. A new local 1.20.1 server process
was started from `C:\Users\LTY\Desktop\REDA\minecraft-server`; its startup log
said `Starting minecraft server version 1.20.1`. Its `server.jar` SHA-1 was
`84194A2F286EF7C14ED7CE0090DBA59902951553`.

Exact conformance command (the omitted `--vectors` means all 16 combinations
of the dump's four inputs):

```powershell
python conformance/circuit_conformance.py --dump "C:\Users\LTY\AppData\Local\Temp\reda-task4-9768984-verilog-seven-segment.txt" --properties "C:\Users\LTY\Desktop\REDA\minecraft-server\server\server.properties" --out "conformance/results/verilog_decoder_task4_9768984.json" --label "task4-mc-dump-9768984-20260810" --origin "170000,151,140000"
```

Result: the generated
`conformance/results/verilog_decoder_task4_9768984.json` records 10,088
non-air blocks, 6,246 placement commands, all **16/16** vectors matched, and
**0** mismatches. The harness used its default pre-build clear and omitted
`--keep`, so its `finally` block cleared the region and released forceload.
After the run, RCON returned:

```text
forceload query 170000 140000
Chunk at [10625, 8750] in minecraft:overworld is not marked for force loading

stop
Stopping the server
```

Port 25575 was confirmed closed after the normal RCON shutdown.

## Automated scope

```text
cargo test --release --test mc_dump official_verilog_seven_segment_dump_uses_optimised_lowering -- --exact --nocapture
1 passed; 0 failed

cargo test --release --test verilog_frontend -- --nocapture
4 passed; 0 failed

bash ./check.sh
root: 391 passed, 0 failed; viewer: 20 passed, 0 failed; clippy and wasm build passed
```

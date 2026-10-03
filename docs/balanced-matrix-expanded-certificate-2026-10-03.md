# Strictly certified balanced four-input matrices

The compiler now has an explicit `compile_balanced_torch_matrix` entry owned by the existing torch-row synthesis layer. It derives the full truth table from the validated source graph, shares positive and negative address rails, programs only nonzero rows, binds actual literal/row/collector refresh stages, and submits the implementation to the unchanged source-equivalence and expanded physical certifiers. The default selector does not use this entry yet.

## Binding problems and their resolution

The physical premise was not a certified candidate. Three concrete ownership problems had to be resolved:

1. Input levers shared supports with their inverters. The producer uses the existing separate-input-floor normalization, already independently checked in the preceding experiment.
2. Some restored row tails have real driven dust but no logical consumer. The existing world adapter now assigns a source-owned, sink-free load route to these actual conductors. It does not invent a logical gate or sink. Existing shape, emission, and electrical checks still apply to the route. The source-to-route identity map is reused.
3. A row-pair observation could coincide with a torch's immediate dust landing, and a final repeater directly against a lamp reused its input landing as the delivery terminal. The pair observation moves within its actual connected carrier; final repeaters receive a separate dust output contact. No delay component or waiting phase is added. The collector also removes wire and repeater prefixes before the first real programmed term rather than binding them to a fictitious constant driver.

All repeated output collection uses the existing expanded merge topology, including four- and five-input distributed collectors and single-contributor carrier observations. No legacy library admission limit or verifier rule changes. Each emitted world is required to have exactly the constructor's canonical fingerprint; unowned physical parts cannot silently disappear during emission.

## Evidence

Full native seven segment and ROM16x7 each pass source rewrite equivalence, implementation equivalence, strict physical certification, export/reload identity, and the original continuous 512-phase protocol at the unchanged 2000-tick cap. Three fresh certified worlds per case have identical canonical fingerprints, headers excluding temporary artifact paths, complete phase records, and result records.

The probe observes every primitive, every implemented gate, intermediate carriers, public drivers, and lamps. The independent Python audit evaluates the recorded implementation graph, checks its complete truth table against the frozen source reference, checks every implementation binding, and replays every event. Missing bindings, changed implementation logic, wrong samples, omitted events, absent certificate provenance claims, and truncated protocols are rejected by the runnable audit test. Offline replay alone does not certify a physical artifact: the manifest separately binds the unchanged Rust certifier execution to frozen SDK sources, library/executable hashes, exported worlds, and certificate logs.

| Case | Frozen reference worst target settlement | Certified matrix | Reference lamp glitch transitions | Certified matrix |
|---|---:|---:|---:|---:|
| Native full seven segment | 44 game ticks | 22 game ticks | 83/256 | 0/256 |
| Native ROM16x7 | 82 game ticks | 26 game ticks | 157/256 | 0/256 |

All retained setup/target quality measures pass, with no public phase/channel arrival regression, no settlement phase regression, and no hidden extra transition. Seven segment has 206 implemented gates and 237 observed sites; ROM has 310 implemented gates and 353 observed sites. These counts describe coverage, not acceptance criteria. Every recorded event and sample at the preceding premise's original observed sites remains exactly identical after filtering the newly added implementation observations.

Three Rust integration tests pass: full-circuit certification with renamed signals and reversed gate order; invalid/stateful/source-budget and current-boundary rejection; and complete continuous public truth for 3/7/11/15 programmed rows with seven repeated output aliases, including final collector repeaters. Six existing physical rewrite consumer/view tests also pass. No tests, historical controls, thresholds, simulator rules, or verification rules are removed or repinned.

## Scope and remaining work

The explicit entry currently accepts exactly four combinational inputs and one through seven outputs with no constant-low column. Unsupported columns fail closed; that refusal is not completed constant support. The two full native cases are certified and independently quality-checked. The additional collector-boundary tests establish truth and terminal correctness, not broader quality qualification.

Default integration, constant-low drivers, wider ROM/Mux boundaries, general composition/search, broader frontend and sequential coverage, Yosys-absence acceptance, real Minecraft conformance, and the previously recorded full-library failures remain unfinished. The SDK snapshot preserves the pre-existing unqualified worktree foundation as provenance; this commit does not adopt those unrelated changes. No project-wide or release completion claim is made.

## Reproduction

Run the narrow committed checks with the repository environment:

```sh
source tools/env.sh
ulimit -s 262144
CARGO_BUILD_JOBS=1 RUST_MIN_STACK=268435456 cargo test --release --test balanced_torch_matrix
CARGO_BUILD_JOBS=1 RUST_MIN_STACK=268435456 cargo test --release --test physical_rewrite_views
python3 tools/test_audit_balanced_matrix_wave.py
python3 tools/test_audit_shared_matrix_premise.py
```

The recorded run uses direct `rustc` with the exact arguments and dependency artifacts in the adjacent manifest, a unique isolated library output, and unique test/probe executables. The wave probe uses the explicit compiler entry, then exports and reloads the certified world before simulation. Its conservative `certified: false` waveform fields retain the premise-auditor protocol; the separate certificate logs and bound Rust execution establish expanded certification. `release_qualified` remains false.

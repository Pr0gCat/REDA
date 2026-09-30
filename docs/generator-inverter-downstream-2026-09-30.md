# Downstream timing qualification and inverter bypass correction

Reference production commit: e3ef731. Extending physical qualification found a
regression in a three-input NOR fed by a three-inverter chain and two direct
primary inputs. The previous immediate-consumer guard admitted the bypass.
Independent positive lowering has worst output-driver change 8 ticks, longest
output-driver pulse 6 ticks, and worst world settling 12 ticks. The candidate
has 12, 10, and 16 ticks respectively. Fewer glitches (5 to 3) does not excuse
these three individual regressions. No acceptance metric uses size.

The correction traverses the complete downstream cone of both the removed
signal and its replacement, including sibling consumers. Any multi-input,
merge, or sequential receiver excludes the bypass. This is a connectivity
rule independent of names. It preserves the path for every newly tested
multi-input receiver, rather than asserting that Boolean equality preserves
timing. Unary output chains and forks remain eligible; multi-input sources
feeding an otherwise unary downstream cone remain eligible.

The physical helper now drives arbitrary input vectors and checks independently
computed outputs plus every actual internal driver, retaining simulator history.
Single-input glitches are counted only when exactly one input changes. Each
new family has 12 fixtures (two/three inputs, chain lengths 3 through 8), 480
target and 480 setup phases per arm. Together they cover 3840 physical phases
plus initial settling. Both arms are compiled twice for world determinism.
All nine metrics must remain within both the current independent positive
lowering control and separately frozen controls. The new frozen JSON records
full traces, fingerprints, and metrics; prior 25 fixture controls remain untouched.

The twelve downstream-receiver candidates now equal their references exactly.
All twelve multi-input-source/unary-output candidates improve worst output
change time without any individual metric regression. The original chains and
forks retain their existing caps. This correction limits an unsafe optimization;
it is not a claim of universal timing improvement or Minecraft certification.

The user-proposed side-torch rail and outputs below already has an explicit
compile_torch_row implementation and tests/torch_row.rs physical probes.
Automatic selection remains disabled: the unresolved stock mixed-support
backfeed defects still prevent qualifying the complete stock/row comparison.
This correction does not resolve those defects or finish the active goal.

Validation: all five inverter qualification tests, three torch-row physical
probes, all 19 lowering unit tests, and frontend/generator quality controls
(1312 target transitions), output isolation, four constant-driver tests, and
native/fresh-Yosys DFF physical test pass. All original 25 full fixture records
match the frozen JSON exactly. git diff --check passes.

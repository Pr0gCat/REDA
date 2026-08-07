#!/usr/bin/env python
"""End-to-end conformance harness: place a compiled `and4` circuit into a
real Minecraft 1.20.1 server, drive its four levers through every input
vector, read the lamp, and compare against the pure-boolean NOR truth table.

This is the "does the whole compiled circuit actually work in the real game"
test described in docs/superpowers/specs/2026-08-07-minecraft-conformance.md.
It is a different, larger thing than conformance/probes.py: probes.py asks
narrow yes/no questions about individual redstone rules in isolation; this
module builds one real, complete, 611-block circuit and asks whether it
computes the right answer.

Usage:
    cargo run --release --bin mc_dump -- and4 > /tmp/and4.txt
    python and4_conformance.py --dump /tmp/and4.txt \
        --properties ../minecraft-server/server/server.properties \
        --out results/and4_head.json --label head

The server must already be running (docs/minecraft-server.md) with RCON
enabled. `mc_dump` (src/bin/mc_dump.rs) is a separate step, run once per
commit under test, so this script never has to embed a Rust toolchain
invocation -- it only ever reads the plain-text dump `mc_dump` produces.

# Placement order -- why blocks go in in this order, not dump order

`mc_dump` emits blocks in a flat Y/Z/X scan, which is not a safe build
order: it interleaves gate bodies, routing dust, and repeaters in whatever
order they happen to sit in space, not in the order a working circuit needs
to come alive. Two things from harness.py's hard-won rules constrain the
real order:

1. **Repeaters must not be born already looking at a powered input.**
   harness.py: "Repeaters ... need their input to genuinely change (built
   pointing at air, then a source is placed ... as a second step) rather
   than being placed already in their 'final' state. Skip either of those
   and the block sits there inert forever." Every repeater in this circuit
   has redstone dust as its input (see compile/mod.rs's own comment: "a
   route always ends in a repeater facing the next gate's support block" --
   dust always mediates). So every repeater here is placed *before* any
   dust exists anywhere in the build: at that moment its input is literally
   air, satisfying the validated-safe recipe by construction, not by luck.

2. **Torches are placed before any dust exists too, and that is not a
   guess -- it is what makes their hard-coded `lit=true` state correct.**
   `compile()` bakes every wall torch's blockstate as `lit=true` regardless
   of the gate's real logical value (see the comment on `wall_torch` in
   compile/mod.rs -- it is a construction placeholder, not a simulated
   result). A torch is lit exactly when the block it is attached to is
   *not* powered. If torches go in before any dust exists to power
   anything, their attachment block is unpowered by simple absence of any
   other block, and `lit=true` is the actual physical truth at that
   instant for every single torch, unconditionally -- no gate-logic
   reasoning required.

That gives the order: solid/floor blocks -> lamps (inert sinks, order
doesn't matter) -> wall torches -> repeaters -> redstone dust, all placed
while every lever is absent (or, if present, off). Levers go in dead last,
each placement/toggle acting as the one genuine triggering event described
in harness.py rule 2 ("a fresh placement ... of an active source correctly
notifies whatever is directly touching it"), which then rides the
dust-mediated propagation chain (rule 2's "reaches one hop past itself")
all the way through the circuit -- a real player builds a redstone
computer exactly this way: wire everything up inert, then flip the switch.

This ordering is a hypothesis grounded in the documented rules, not a
retest of them -- it is checked empirically below by confirming a handful
of vectors (particularly all-off and all-on) settle to the expected lamp
state before trusting a full 16-vector sweep, and by reading every gate's
own output torch on every vector so a disagreement can be localised to the
first gate that is wrong, not just reported as "the lamp was wrong".

# Toggling vs. rebuilding

The spec suggested rebuilding the whole circuit per input vector as the
"safe" default. This harness instead builds the static structure exactly
once and only ever re-sets the four lever blocks between vectors -- i.e.
toggling in place. That is explicitly the *other* option the spec allows
("If you find toggling works reliably for our geometry, that is better and
faster -- but prove it, do not assume it."), and harness.py rule 2 already
established that toggling an existing lever is exactly as reliable as a
fresh placement provided dust mediates the far end, which every lever
socket here does by construction. The empirical checks below (matching
every one of 16 vectors, including several immediately-adjacent pairs that
only flip one bit) are what stands in for that proof; see the run report's
"vectors" list.

# Robustness against a racing world

A run against a genuinely fresh region (never forceloaded before in this
world) once reported two mismatched vectors (`0011`, `0111`, both first
disagreeing at gate `g3`); every run since -- including one after a full
server restart -- passed 16/16 with no code changes. That shape (wrong
exactly once, on brand-new terrain, self-healing once the chunks existed on
disk) pointed at a race between this harness and world generation/loading
rather than a real circuit bug, and one link in that chain is confirmed,
not assumed: `/forceload add` is not synchronous. Measured directly against
a live server, `execute if loaded <pos>` still answers "Test failed"
immediately after a `forceload add` covering that position returns, and
only starts answering "Test passed" about one game tick (~50-100ms) later.
This harness used to issue `/fill`/`/setblock` commands right after
forceload's command returned, trusting that return to mean the region was
already loaded and ticking -- which the measurement above shows is false.
(The original two-vector mismatch itself could not be reproduced on demand
across repeated fresh-world attempts while diagnosing this -- it is a
narrow race, not a deterministic one -- but the false assumption it
implicates was reproduced directly and is real regardless.)

Three defenses now stand between forceload and the first `setblock`:

1. `wait_for_region_loaded` polls `execute if loaded` for every chunk the
   build will occupy until each genuinely reports loaded, instead of
   trusting forceload's return.
2. `verify_region_is_air` samples the cleared region with `execute if
   block ... minecraft:air` and retries until it actually reads back clean,
   instead of a fixed sleep after `clear_region`.
3. `assert_quiescent` checks the lamp and every gate's output torch against
   the all-zero quiescent state right after the initial build settles, and
   raises `RegionNotReady` (a distinct exit code, 2, from a real mismatch's
   exit code, 1) if the circuit is not already in the state the simulator
   predicts -- so a corrupted build is reported as "the build did not
   settle as expected" instead of being silently swept across all 16
   vectors and reported as circuit-level mismatches.
"""

from __future__ import annotations

import argparse
import itertools
import json
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from rcon import RconClient

# Where the circuit is placed. Deliberately far from both world spawn and
# the probes.py region (100000, *, 100000..~101300) so the two suites can
# never collide if run back to back. Same Y as harness.py's probe region
# (151) -- already confirmed to sit well clear of a superflat world's
# generated layers.
DEFAULT_ORIGIN = (150000, 151, 100000)

FILL_LIMIT = 32000  # stay under vanilla's 32768-block /fill cap with margin


@dataclass
class Block:
    x: int
    y: int
    z: int
    kind: str
    facing: str
    face: str
    lit: bool
    delay: int
    name: str


@dataclass
class Dump:
    size: tuple[int, int, int]
    blocks: list[Block]
    inputs: dict[str, tuple[int, int, int]]
    outputs: dict[str, tuple[int, int, int]]
    gate_outputs: dict[str, tuple[int, int, int]]
    gates: list[tuple[str, list[str]]]  # (output_name, input_names) in topological order


def parse_dump(path: Path) -> Dump:
    size = None
    blocks: list[Block] = []
    inputs: dict[str, tuple[int, int, int]] = {}
    outputs: dict[str, tuple[int, int, int]] = {}
    gate_outputs: dict[str, tuple[int, int, int]] = {}
    gates: list[tuple[str, list[str]]] = []

    for line in path.read_text(encoding="utf-8").splitlines():
        parts = line.split()
        if not parts:
            continue
        tag = parts[0]
        if tag == "SIZE":
            size = (int(parts[1]), int(parts[2]), int(parts[3]))
        elif tag == "BLOCK":
            blocks.append(
                Block(
                    x=int(parts[1]),
                    y=int(parts[2]),
                    z=int(parts[3]),
                    kind=parts[4],
                    facing=parts[5],
                    face=parts[6],
                    lit=(parts[7] == "true"),
                    delay=int(parts[8]),
                    name=parts[9],
                )
            )
        elif tag == "INPUT":
            inputs[parts[1]] = (int(parts[2]), int(parts[3]), int(parts[4]))
        elif tag == "OUTPUT":
            outputs[parts[1]] = (int(parts[2]), int(parts[3]), int(parts[4]))
        elif tag == "GATEOUT":
            gate_outputs[parts[1]] = (int(parts[2]), int(parts[3]), int(parts[4]))
        elif tag == "GATE":
            name = parts[1]
            ins = parts[2].split(",") if len(parts) > 2 else []
            gates.append((name, ins))
        else:
            raise ValueError(f"unrecognised dump line tag {tag!r}: {line!r}")

    if size is None:
        raise ValueError("dump had no SIZE line")
    return Dump(size, blocks, inputs, outputs, gate_outputs, gates)


def evaluate_expected(gates: list[tuple[str, list[str]]], primary: dict[str, int]) -> dict[str, int]:
    """Pure-boolean NOR evaluation: every gate is NOR of its inputs. Gates
    are already in topological order (mc_dump preserves Netlist::gates
    order), so a single forward pass is enough -- no fixpoint iteration
    needed, unlike the physical simulator."""
    values = dict(primary)
    for name, ins in gates:
        values[name] = 0 if any(values[i] for i in ins) else 1
    return values


def block_state_string(b: Block) -> str:
    if b.kind == "Solid":
        return b.name
    if b.kind == "Lamp":
        return f"{b.name}[lit=false]"
    if b.kind in ("WallTorch", "Torch"):
        # See module docstring point 2: lit=true is the physically correct
        # value at the moment this is placed (no dust exists yet anywhere,
        # so nothing can be powering this torch's attachment block).
        return f"{b.name}[facing={b.facing},lit=true]"
    if b.kind == "Repeater":
        # powered=false unconditionally, regardless of what compile() baked
        # in (always a placeholder there). See module docstring point 1:
        # placed before any dust exists, so this is also physically true.
        return f"{b.name}[facing={b.facing},delay={b.delay},locked=false,powered=false]"
    if b.kind == "RedstoneWire":
        # Bare, no power/shape properties -- the server computes both from
        # context as soon as anything nearby changes. See probes.py's DUST
        # convention, which this deliberately matches.
        return b.name
    raise ValueError(f"and4_conformance does not know how to place block kind {b.kind!r} ({b.name})")


def lever_state_string(b: Block, on: bool) -> str:
    return f"{b.name}[face={b.face},facing={b.facing},powered={'true' if on else 'false'}]"


CATEGORY_ORDER = ["Solid", "Lamp", "WallTorch", "Torch", "Repeater", "RedstoneWire"]


def build_phase_commands(blocks: list[Block], origin: tuple[int, int, int]) -> list[str]:
    """Emit setblock/fill commands for every non-lever block, grouped by
    category in CATEGORY_ORDER, collapsing contiguous same-state runs along
    X into a single /fill (see module docstring for why category order
    matters; run-collapsing is purely a speed optimisation on top of it)."""
    ox, oy, oz = origin
    commands: list[str] = []
    for category in CATEGORY_ORDER:
        run = [b for b in blocks if b.kind == category]
        i = 0
        n = len(run)
        while i < n:
            b = run[i]
            state = block_state_string(b)
            j = i + 1
            while (
                j < n
                and run[j].y == b.y
                and run[j].z == b.z
                and run[j].x == run[j - 1].x + 1
                and block_state_string(run[j]) == state
            ):
                j += 1
            span = run[i:j]
            if len(span) >= 2:
                x1, y1, z1 = ox + span[0].x, oy + span[0].y, oz + span[0].z
                x2, y2, z2 = ox + span[-1].x, oy + span[-1].y, oz + span[-1].z
                commands.append(f"fill {x1} {y1} {z1} {x2} {y2} {z2} {state} replace")
            else:
                x, y, z = ox + b.x, oy + b.y, oz + b.z
                commands.append(f"setblock {x} {y} {z} {state} replace")
            i = j
    return commands


def clear_region(client: RconClient, origin: tuple[int, int, int], size: tuple[int, int, int]) -> None:
    ox, oy, oz = origin
    sx, sy, sz = size
    margin = 3
    lo = (ox - margin, oy - margin, oz - margin)
    hi = (ox + sx + margin, oy + sy + margin, oz + sz + margin)
    volume_per_x_slice = (hi[1] - lo[1]) * (hi[2] - lo[2])
    step = max(1, FILL_LIMIT // max(1, volume_per_x_slice))
    x = lo[0]
    while x <= hi[0]:
        x2 = min(x + step - 1, hi[0])
        client.command(f"fill {x} {lo[1]} {lo[2]} {x2} {hi[1]} {hi[2]} minecraft:air")
        x = x2 + 1


def forceload(client: RconClient, origin: tuple[int, int, int], size: tuple[int, int, int]) -> None:
    ox, _, oz = origin
    sx, _, sz = size
    margin = 8
    client.command(f"forceload add {ox - margin} {oz - margin} {ox + sx + margin} {oz + sz + margin}")


def forceload_release(client: RconClient, origin: tuple[int, int, int], size: tuple[int, int, int]) -> None:
    ox, _, oz = origin
    sx, _, sz = size
    margin = 8
    client.command(f"forceload remove {ox - margin} {oz - margin} {ox + sx + margin} {oz + sz + margin}")


class RegionNotReady(RuntimeError):
    """Raised when the world does not confirm the state this harness is
    about to depend on -- a loaded region, a genuinely blank canvas, or a
    settled circuit -- within a generous timeout. A test that proceeds
    anyway is exactly the false-failure risk this class exists to remove:
    see the module docstring's "Robustness against a racing world" section."""


def _xz_chunk_grid(origin: tuple[int, int, int], size: tuple[int, int, int], step: int = 16) -> list[tuple[int, int]]:
    """One (x, z) sample point per ~step x step cell of the build's
    footprint, so a chunk-granular check (is this loaded? is this air?)
    touches every chunk the build will actually occupy, not just the
    region's outer corners -- a slow-to-load chunk in the middle of a wide
    circuit would be invisible to a corners-only check."""
    ox, _, oz = origin
    sx, _, sz = size
    xs = sorted(set(range(ox, ox + sx, step)) | {ox + sx - 1})
    zs = sorted(set(range(oz, oz + sz, step)) | {oz + sz - 1})
    return [(x, z) for x in xs for z in zs]


def wait_for_region_loaded(
    client: RconClient,
    origin: tuple[int, int, int],
    size: tuple[int, int, int],
    timeout: float = 15.0,
    poll_interval: float = 0.1,
) -> None:
    """`/forceload add` is not synchronous: confirmed directly against a
    live server, `execute if loaded <pos>` still answers "Test failed"
    immediately after a `forceload add` covering that position returns, and
    only starts answering "Test passed" about one game tick (~50-100ms)
    later. Everything downstream of forceload() in this harness used to
    assume the region was already loaded and ticking the instant the
    command came back -- that assumption is false, and is the leading
    suspect for the one observed false failure (see the module docstring's
    "Robustness against a racing world" section). This polls every chunk
    the build will occupy until each one genuinely reports loaded, rather
    than trusting forceload's return to mean anything."""
    ox, oy, _ = origin
    points = [(x, oy, z) for x, z in _xz_chunk_grid(origin, size)]
    deadline = time.monotonic() + timeout
    while True:
        pending = [
            p for p in points
            if not client.command(f"execute if loaded {p[0]} {p[1]} {p[2]}").strip().startswith("Test passed")
        ]
        if not pending:
            return
        if time.monotonic() >= deadline:
            raise RegionNotReady(
                f"region did not report loaded within {timeout:.1f}s of forceload "
                f"({len(pending)}/{len(points)} sample chunks still unloaded, e.g. {pending[0]}) -- "
                "forceload is not synchronous; see wait_for_region_loaded's docstring."
            )
        time.sleep(poll_interval)


def verify_region_is_air(
    client: RconClient,
    origin: tuple[int, int, int],
    size: tuple[int, int, int],
    timeout: float = 20.0,
    poll_interval: float = 0.2,
) -> None:
    """After clear_region() issues its `/fill ... air` commands, confirm the
    region really reads back as air before trusting it as a blank canvas --
    a sampled `execute if block` check that retries rather than proceeding.
    Guards against exactly the same class of problem as
    wait_for_region_loaded: a command that returned is not proof its effect
    is visible to the very next command sent a moment later."""
    ox, oy, oz = origin
    _, sy, _ = size
    ys = sorted({oy, oy + sy - 1})
    points = [(x, y, z) for x, z in _xz_chunk_grid(origin, size) for y in ys]
    deadline = time.monotonic() + timeout
    while True:
        not_air = [
            p for p in points
            if not client.command(f"execute if block {p[0]} {p[1]} {p[2]} minecraft:air").strip().startswith(
                "Test passed"
            )
        ]
        if not not_air:
            return
        if time.monotonic() >= deadline:
            raise RegionNotReady(
                f"region did not verify as air within {timeout:.1f}s of clearing "
                f"({len(not_air)}/{len(points)} sample points not air, e.g. {not_air[0]}) -- "
                "refusing to build on ground we have not confirmed is blank."
            )
        time.sleep(poll_interval)


def read_lamp(client: RconClient, pos: tuple[int, int, int]) -> int | None:
    x, y, z = pos
    if client.command(f"execute if block {x} {y} {z} minecraft:redstone_lamp[lit=true]").strip().startswith(
        "Test passed"
    ):
        return 1
    if client.command(f"execute if block {x} {y} {z} minecraft:redstone_lamp[lit=false]").strip().startswith(
        "Test passed"
    ):
        return 0
    return None


def read_torch(client: RconClient, pos: tuple[int, int, int]) -> int | None:
    x, y, z = pos
    if client.command(
        f"execute if block {x} {y} {z} minecraft:redstone_wall_torch[lit=true]"
    ).strip().startswith("Test passed"):
        return 1
    if client.command(
        f"execute if block {x} {y} {z} minecraft:redstone_wall_torch[lit=false]"
    ).strip().startswith("Test passed"):
        return 0
    return None


def assert_quiescent(
    client: RconClient,
    dump: Dump,
    origin: tuple[int, int, int],
    output_signal: str,
    output_pos: tuple[int, int, int],
) -> None:
    """After the initial build settles (every lever placed off), check the
    lamp and every gate's output torch against the pure-boolean evaluation
    for all-zero inputs, and fail loudly if any disagree, instead of
    silently sweeping all 16 vectors over a circuit that never finished
    settling. This does not depend on identifying the exact reason the
    build might be wrong -- a race with world generation, a dropped RCON
    command, an unlucky chunk reload -- it just refuses to trust a circuit
    it has not itself confirmed is in the state the simulator predicts."""
    ox, oy, oz = origin
    expected = evaluate_expected(dump.gates, {name: 0 for name in sorted(dump.inputs)})

    mismatches: list[str] = []
    lamp_pos = (ox + output_pos[0], oy + output_pos[1], oz + output_pos[2])
    actual_lamp = read_lamp(client, lamp_pos)
    if actual_lamp != expected[output_signal]:
        mismatches.append(f"output {output_signal}: expected {expected[output_signal]}, read {actual_lamp!r}")

    for gate_name, gate_pos in dump.gate_outputs.items():
        pos = (ox + gate_pos[0], oy + gate_pos[1], oz + gate_pos[2])
        actual = read_torch(client, pos)
        if actual != expected[gate_name]:
            mismatches.append(f"gate {gate_name}: expected {expected[gate_name]}, read {actual!r}")

    if mismatches:
        raise RegionNotReady(
            "the build did not settle as expected -- circuit disagrees with the all-zero "
            "quiescent state before a single vector was even swept: " + "; ".join(mismatches)
        )


def read_properties(path: Path) -> dict:
    props = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        props[key.strip()] = value.strip()
    return props


def run(args: argparse.Namespace) -> int:
    dump = parse_dump(Path(args.dump))
    origin = tuple(int(v) for v in args.origin.split(","))
    assert len(origin) == 3

    input_names = sorted(dump.inputs.keys())  # a, b, c, d
    if len(dump.outputs) != 1:
        print(f"error: expected exactly one output, dump has {list(dump.outputs)}", file=sys.stderr)
        return 1
    (output_signal, output_pos), = dump.outputs.items()

    if args.vectors:
        vectors = [tuple(int(c) for c in v) for v in args.vectors.split(",")]
    else:
        vectors = list(itertools.product([0, 1], repeat=len(input_names)))

    props = read_properties(Path(args.properties))
    port = int(props.get("rcon.port", "25575"))
    password = props.get("rcon.password")
    if not password:
        print("error: rcon.password not found in server.properties", file=sys.stderr)
        return 1

    client = RconClient(args.host, port, password)
    print(f"Connecting to {args.host}:{port} ...")
    client.connect()

    report: dict = {
        "label": args.label,
        "dump": str(args.dump),
        "origin": origin,
        "size": dump.size,
        "non_air_blocks": len(dump.blocks),
        "vectors": [],
    }

    try:
        print("Forceloading region...")
        forceload(client, origin, dump.size)
        wait_for_region_loaded(client, origin, dump.size)
        if not args.no_clear:
            print("Clearing region...")
            clear_region(client, origin, dump.size)
            verify_region_is_air(client, origin, dump.size)

        lever_blocks = [b for b in dump.blocks if b.kind == "Lever"]
        static_blocks = [b for b in dump.blocks if b.kind != "Lever"]
        by_name = {}
        for name, pos in dump.inputs.items():
            match = [b for b in lever_blocks if (b.x, b.y, b.z) == pos]
            assert len(match) == 1, f"expected exactly one lever block at {pos} for input {name}"
            by_name[name] = match[0]

        print(f"Placing {len(static_blocks)} static blocks + {len(lever_blocks)} levers (all off)...")
        t0 = time.monotonic()
        commands = build_phase_commands(static_blocks, origin)
        # Levers go in last, all off -- see module docstring: the static
        # network must exist and be fully inert before any lever exists.
        ox, oy, oz = origin
        for name in input_names:
            b = by_name[name]
            x, y, z = ox + b.x, oy + b.y, oz + b.z
            commands.append(f"setblock {x} {y} {z} {lever_state_string(b, False)} replace")
        for cmd in commands:
            client.command(cmd)
        placement_seconds = time.monotonic() - t0
        print(f"Placement issued {len(commands)} commands for {len(dump.blocks)} blocks in {placement_seconds:.1f}s")
        report["placement_commands"] = len(commands)
        report["placement_seconds"] = placement_seconds

        print(f"Settling {args.settle_build:.1f}s after initial build...")
        time.sleep(args.settle_build)

        print("Checking the build settled to the expected all-zero quiescent state...")
        assert_quiescent(client, dump, origin, output_signal, output_pos)

        current = {name: False for name in input_names}

        for vector in vectors:
            primary = {name: bit for name, bit in zip(input_names, vector)}
            expected = evaluate_expected(dump.gates, primary)
            expected_output = expected[output_signal]

            changed = [name for name in input_names if bool(primary[name]) != current[name]]
            for name in input_names:
                b = by_name[name]
                x, y, z = ox + b.x, oy + b.y, oz + b.z
                client.command(f"setblock {x} {y} {z} {lever_state_string(b, bool(primary[name]))} replace")
            current = {name: bool(primary[name]) for name in input_names}

            time.sleep(args.settle_vector)

            actual_output = read_lamp(client, (ox + output_pos[0], oy + output_pos[1], oz + output_pos[2]))

            gate_readings = {}
            for gate_name, gate_pos in dump.gate_outputs.items():
                pos = (ox + gate_pos[0], oy + gate_pos[1], oz + gate_pos[2])
                gate_readings[gate_name] = read_torch(client, pos)

            first_disagreement = None
            for gate_name, _ins in dump.gates:
                if gate_readings.get(gate_name) is not None and gate_readings[gate_name] != expected[gate_name]:
                    first_disagreement = gate_name
                    break

            vec_str = "".join(str(b) for b in vector)
            match = actual_output == expected_output
            print(
                f"  {vec_str} -> expected={expected_output} actual={actual_output} "
                f"{'OK' if match else 'MISMATCH'}"
                + (f" (first bad gate: {first_disagreement})" if first_disagreement else "")
            )

            report["vectors"].append(
                {
                    "vector": vec_str,
                    "changed_levers": changed,
                    "expected_output": expected_output,
                    "actual_output": actual_output,
                    "match": match,
                    "expected_gates": expected,
                    "actual_gates": gate_readings,
                    "first_disagreeing_gate": first_disagreement,
                }
            )

    finally:
        if not args.keep:
            print("Clearing region and releasing forceload...")
            clear_region(client, origin, dump.size)
            forceload_release(client, origin, dump.size)
        client.close()

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(report, indent=2), encoding="utf-8")

    total = len(report["vectors"])
    mismatches = sum(1 for v in report["vectors"] if not v["match"])
    print(f"\n{total} vectors run, {mismatches} mismatched. Wrote {out_path}")
    return 1 if mismatches else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dump", required=True, help="path to mc_dump's text output for and4")
    ap.add_argument("--properties", required=True, help="path to the target server's server.properties")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--out", required=True, help="path to write the JSON report")
    ap.add_argument("--label", required=True, help="short label for this run, e.g. 'head' or 'a1d50a4'")
    ap.add_argument("--origin", default="{},{},{}".format(*DEFAULT_ORIGIN), help="x,y,z world origin to place the circuit at")
    ap.add_argument("--settle-build", type=float, default=8.0, help="seconds to wait after the initial static build")
    ap.add_argument("--settle-vector", type=float, default=6.0, help="seconds to wait after setting levers for one vector")
    ap.add_argument("--vectors", default=None, help="comma-separated 4-bit vectors to test, e.g. 0000,1111 (default: all 16)")
    ap.add_argument("--no-clear", action="store_true", help="skip the pre-build air clear (region already known clean)")
    ap.add_argument("--keep", action="store_true", help="leave the circuit standing and the region forceloaded after the run")
    args = ap.parse_args()
    try:
        return run(args)
    except RegionNotReady as exc:
        # Deliberately distinct from a mismatched-vector failure (exit 1):
        # this means the world was never in a state worth testing, not that
        # the circuit disagreed with the truth table. run()'s own
        # try/finally has already cleared the region and released the
        # forceload before this is reached.
        print(f"\nERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

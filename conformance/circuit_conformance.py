#!/usr/bin/env python
"""End-to-end conformance harness: place any circuit `mc_dump` can describe
into a real Minecraft 1.20.1 server, drive its levers through every input
vector, read every output, and compare against the pure-boolean NOR truth
table.

This is the "does the whole compiled circuit actually work in the real game"
test described in docs/superpowers/specs/2026-08-07-minecraft-conformance.md.
It is a different, larger thing than conformance/probes.py: probes.py asks
narrow yes/no questions about individual redstone rules in isolation; this
module builds one real, complete circuit -- from `and4`'s 611 blocks and one
output up to the seven-segment decoder's 16,894 blocks and seven -- and asks
whether it computes the right answer.

This script is deliberately circuit-agnostic: it was originally written
against `and4` alone (one lever set, one lamp), then generalised to read
however many `INPUT`/`OUTPUT` lines a dump contains rather than assuming
exactly one output. Nothing here hardcodes a circuit name, an input count, or
an output count -- see `Dump`/`parse_dump` below, which build every one of
those from the dump file itself. The failure-category logic (`RegionNotReady`
vs `CircuitMismatch`, exit 2 vs exit 1) that makes a failing run diagnostic
rather than just "something didn't match" is untouched by that
generalisation -- it operates over "every output" and "every gate" exactly
as it always did over the one output `and4` happens to have.

Usage:
    cargo run --release --bin mc_dump -- and4 > /tmp/and4.txt
    python circuit_conformance.py --dump /tmp/and4.txt \
        --properties ../minecraft-server/server/server.properties \
        --out results/and4_head.json --label head

    cargo run --release --bin mc_dump -- seven_segment > /tmp/decoder.txt
    python circuit_conformance.py --dump /tmp/decoder.txt \
        --properties ../minecraft-server/server/server.properties \
        --out results/decoder_head.json --label head

    cargo run --release --bin mc_dump -- verilog:seven_segment > /tmp/vdecoder.txt
    python circuit_conformance.py --dump /tmp/vdecoder.txt \
        --properties ../minecraft-server/server/server.properties \
        --out results/verilog_decoder_head.json --label head

# Wired-OR merges

The third example above is the Yosys-synthesised decoder, and it is the first
circuit run through here that is not pure NOR: the Verilog frontend maps
Yosys's `OR2`/`OR3` cells onto **wire merges** rather than gates. A merge is a
different animal in both of the ways this harness cares about -- it ORs its
inputs instead of NOR-ing them, and it has no torch at all, just a dust
junction -- so the `GATE` line now carries a kind (`nor`/`merge`) and this
script honours it in `evaluate_expected`, `read_state`, and the placement
check. Getting either half wrong does not merely under-report: NOR-ing a
merge poisons every expected value downstream of it, and looking for a torch
at a junction reports a correctly-built circuit as `RegionNotReady`. Nothing
about a pure-NOR circuit changed -- every hand-written one emits only `nor`.

No settle flags needed above, on either circuit: this harness polls each
circuit until it is actually quiescent (see poll_until_quiescent) rather than
sleeping a fixed guess, so a bigger circuit just takes however long it
actually takes, not however long someone remembered to ask for. --settle-build
and --settle-vector still exist, but as floors (skip pointless early polls
during dead time), not the stopping condition.

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
of vectors (particularly all-off and all-on) settle to the expected output
state(s) before trusting a full sweep, and by reading every gate's own
output torch on every vector so a disagreement can be localised to the
first gate that is wrong, not just reported as "an output was wrong".

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
3. `assert_quiescent` checks every output lamp and every gate's output torch
   against the all-zero quiescent state right after the initial build settles, and
   raises before a single input vector is even swept if anything is wrong.
   It asks three separate questions, in order, because they have different
   causes and different fixes:

   a. **Did placement land?** Is the block we asked to be at each position
      (lamp, every gate's output torch) actually there, by block identity
      alone -- ignoring lit/powered/facing entirely? This is answerable
      without knowing anything about redstone: it is the same kind of
      check `verify_region_is_air` already makes, just pointed at "the
      right non-air block" instead of "air". If this fails, the world
      never got into the state we told it to -- that is `RegionNotReady`,
      exit code 2, an environment/harness problem, not a circuit one.
   b. **Has it settled, or is it still changing?** Poll every signal --
      every output lamp and every gate's output torch -- repeatedly, and
      call it settled only once several consecutive polls in a row agree on
      all of them (see `poll_until_quiescent` and `REQUIRED_STABLE_POLLS` --
      not just two: a freshly-placed circuit glitches almost continuously
      for the first several seconds, with quiet notches between glitches
      almost as long as one poll interval, so two reads alone can land in
      the same notch by chance and look settled when they are not). If it
      never holds still that long before a generous,
      dump-derived ceiling (see `compute_settle_ceiling`) runs out, the
      circuit is oscillating, not merely wrong -- a distinct failure shape
      from "wrong but stable", and the message says so explicitly. This
      used to be a fixed sleep (`--settle-vector`, formerly a flat 6.0s)
      followed by a single pair of reads -- which is what let the
      seven-segment decoder's real 124-tick (6.2s) settle time read as a
      circuit bug (specific vectors, specific segments, a named "first bad
      gate") when it was really the harness giving up 0.2s early. Polling
      to an actual fixed point removes the guess entirely; see this
      function's own docstring and `poll_until_quiescent`'s for the detail.
   c. **Settled to the right values?** With placement confirmed and no
      motion detected, any remaining disagreement with the simulator is
      the compiled circuit's logic being wrong. That is `CircuitMismatch`,
      exit code 1 -- the same category a mismatch found during the vector
      sweep gets -- and the message names every disagreeing gate.

   Only (a) is a `RegionNotReady`. (b) and (c) are both `CircuitMismatch`:
   the world did what we asked; the circuit computed the wrong thing. That
   distinction is the whole point -- see "What to fix" in this harness's
   change history: a dead circuit must never be reported as "go look at
   the server".
"""

from __future__ import annotations

import argparse
import itertools
import json
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from harness import GAME_TICK_S
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
class Gate:
    """One netlist gate, straight off a `GATE` line.

    `kind` is `"nor"` or `"merge"`. A merge is a **wired OR**, not a NOR --
    see mc_dump.rs's module docstring. It matters twice over, and neither
    place is optional:

    * `evaluate_expected` must OR a merge's inputs instead of NOR-ing them.
      Getting this wrong is not a local error: every gate downstream of the
      merge inherits the wrong value too, so a single mis-evaluated merge
      turns into a report full of "first bad gate" names that are all
      correct and one that is not.
    * A merge has no torch at all -- it is a bare dust junction where its
      inputs' routes are allowed to touch -- so the block sitting at its
      `GATEOUT` position is `minecraft:redstone_wire`, and its logical value
      is "this dust has non-zero power" rather than "this torch is lit". See
      `read_merge_junction` and `expected_block_at`.

    Every hand-written reference circuit is pure NOR, so nothing about it
    changed; the Verilog frontend maps Yosys `OR2`/`OR3` cells onto merges,
    which is what made this distinction load-bearing.
    """

    name: str
    inputs: list[str]
    kind: str

    @property
    def is_merge(self) -> bool:
        return self.kind == "merge"


@dataclass
class Dump:
    size: tuple[int, int, int]
    blocks: list[Block]
    inputs: dict[str, tuple[int, int, int]]
    outputs: dict[str, tuple[int, int, int]]  # keyed by internal signal name, e.g. "g45"
    output_labels: dict[str, str]  # internal signal name -> human label, e.g. "g45" -> "a"
    gate_outputs: dict[str, tuple[int, int, int]]
    gates: list[Gate]  # in topological order

    def output_display_name(self, internal_name: str) -> str:
        """Human label for an output signal if `mc_dump` supplied one via
        OUTLABEL (every reference circuit does), else the internal name
        itself -- so a dump from an older `mc_dump` binary that predates
        OUTLABEL still works, just less legibly."""
        return self.output_labels.get(internal_name, internal_name)

    def sorted_output_names(self) -> list[str]:
        """Internal output names, ordered by their human label ("a" before
        "g") when labels are available, falling back to the dict's own
        (BTreeMap, i.e. alphabetical-by-internal-name) order otherwise."""
        return sorted(self.outputs, key=lambda n: (self.output_display_name(n), n))


def parse_dump(path: Path) -> Dump:
    size = None
    blocks: list[Block] = []
    inputs: dict[str, tuple[int, int, int]] = {}
    outputs: dict[str, tuple[int, int, int]] = {}
    output_labels: dict[str, str] = {}
    gate_outputs: dict[str, tuple[int, int, int]] = {}
    gates: list[Gate] = []

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
        elif tag == "OUTLABEL":
            # "OUTLABEL label internal_name" -- see mc_dump.rs's module
            # docstring. Stored inverted (internal -> label) because every
            # lookup this script does starts from the internal name (the key
            # `outputs`, `evaluate_expected`'s result dict, and `GATE` lines
            # all share).
            output_labels[parts[2]] = parts[1]
        elif tag == "GATEOUT":
            gate_outputs[parts[1]] = (int(parts[2]), int(parts[3]), int(parts[4]))
        elif tag == "GATE":
            # "GATE output_name input1,input2,... kind". Inputs are "-" when
            # a gate has none, so that `kind` is always the same field; kind
            # itself defaults to "nor" so a dump from an mc_dump predating
            # wired-OR merges still parses (every gate in one of those really
            # was a NOR).
            name = parts[1]
            raw_inputs = parts[2] if len(parts) > 2 else "-"
            ins = [] if raw_inputs == "-" else raw_inputs.split(",")
            kind = parts[3] if len(parts) > 3 else "nor"
            if kind not in ("nor", "merge"):
                raise ValueError(f"unrecognised gate kind {kind!r}: {line!r}")
            gates.append(Gate(name, ins, kind))
        else:
            raise ValueError(f"unrecognised dump line tag {tag!r}: {line!r}")

    if size is None:
        raise ValueError("dump had no SIZE line")
    return Dump(size, blocks, inputs, outputs, output_labels, gate_outputs, gates)


def evaluate_expected(gates: list[Gate], primary: dict[str, int]) -> dict[str, int]:
    """Pure-boolean evaluation: a `nor` gate is the NOR of its inputs, a
    `merge` gate the OR of them (see `Gate`). Gates are already in
    topological order (mc_dump preserves Netlist::gates order), so a single
    forward pass is enough -- no fixpoint iteration needed, unlike the
    physical simulator."""
    values = dict(primary)
    for gate in gates:
        high = any(values[i] for i in gate.inputs)
        values[gate.name] = int(high) if gate.is_merge else int(not high)
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
    raise ValueError(f"circuit_conformance does not know how to place block kind {b.kind!r} ({b.name})")


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
    """Fill the circuit's bounding box (plus margin) with air, sliced along X
    so no single `/fill` exceeds vanilla's 32768-block cap.

    Found the hard way, on the seven-segment decoder: the slice-volume
    calculation here used to compute `(hi[1] - lo[1]) * (hi[2] - lo[2])` --
    missing the `+1` every one of these bounds needs because `/fill`'s
    coordinates are inclusive on both ends. That silently under-counted the
    true per-slice volume (e.g. and4's own region: undercounted as 891,
    really 1,092; the decoder's: undercounted enough that `step` came out at
    10 when it needed to be closer to 9), which computed a `step` one notch
    too generous. The resulting `/fill` commands then asked for slightly
    *more* than 32768 blocks each -- rejected outright by the server with
    "Too many blocks in the specified area" -- and because nothing here ever
    inspected `client.command()`'s return string, every single one of those
    rejections was swallowed in silence. `clear_region` would report success
    (there is nothing to report -- it never raised) while leaving the entire
    previous build standing untouched underneath whatever got placed next.
    This never surfaced on `and4` in isolation because a region visited for
    the first time is already blank, so a no-op "clear" of already-air
    ground is indistinguishable from a working one -- it only became visible
    once a *second*, different, larger circuit was built at the same origin
    right after an `and4` run, and `verify_region_is_air`'s necessarily
    coarse sample grid happened not to land on any of the leftover blocks.
    Fixed by counting inclusively and, belt-and-braces, by making a rejected
    fill loud instead of silent -- see the response check below."""
    ox, oy, oz = origin
    sx, sy, sz = size
    margin = 3
    lo = (ox - margin, oy - margin, oz - margin)
    hi = (ox + sx + margin, oy + sy + margin, oz + sz + margin)
    dy = hi[1] - lo[1] + 1
    dz = hi[2] - lo[2] + 1
    volume_per_x_slice = dy * dz
    step = max(1, FILL_LIMIT // max(1, volume_per_x_slice))
    x = lo[0]
    while x <= hi[0]:
        x2 = min(x + step - 1, hi[0])
        response = client.command(f"fill {x} {lo[1]} {lo[2]} {x2} {hi[1]} {hi[2]} minecraft:air")
        if "Too many blocks" in response:
            raise RegionNotReady(
                f"clear_region's fill slice x={x}..{x2} was rejected by the server: {response!r} -- "
                "this slice's volume exceeds vanilla's /fill cap; clear_region's slicing math is wrong "
                "again, or FILL_LIMIT needs to come down further. Refusing to proceed silently onto "
                "ground that was never actually confirmed cleared."
            )
        x = x2 + 1


# Measured directly against this project's own live 1.20.1 server (not taken
# on faith from any wiki): a single `/forceload add` call rejects outright
# with "Too many chunks in the specified area (maximum 256, specified 441)"
# once its rectangle exceeds 256 chunks -- confirmed by requesting a 21x21
# chunk area (441) and getting that exact refusal. This is a *per-command*
# cap, not a per-dimension total: two separate calls of 225 chunks each (well
# under 256 individually) were both accepted and `/forceload query` then
# reported all 450 chunks force-loaded simultaneously. So the fix is not to
# shrink the forceloaded area -- it is to split one oversized request into
# several requests that each stay under the cap.
#
# and4's whole build (66x75 blocks, +8 margin -> ~6x6 chunks = 36) is nowhere
# near 256 and would fit in one call regardless. The seven-segment decoder
# (232x280 blocks, +8 margin -> roughly 16x19 = 304 chunks) is the circuit
# that actually exceeds the cap in this project -- see the module docstring
# for that finding reported plainly, since a decoder-sized build not fitting
# in a single vanilla command is a genuine, worth-recording property of our
# output's size, not a harness bug to quietly paper over.
FORCELOAD_CHUNK_LIMIT = 256
# 16x16 chunks = 256, exactly at the cap, and a whole-number divisor of any
# chunk-aligned rectangle -- simplest possible tiling that always stays
# legal regardless of how large a future circuit's footprint grows to.
FORCELOAD_TILE_CHUNKS = 16


def _forceload_tiles(x_lo: int, z_lo: int, x_hi: int, z_hi: int) -> list[tuple[int, int, int, int]]:
    """Split the block-coordinate rectangle [x_lo, x_hi] x [z_lo, z_hi] into
    tiles of at most FORCELOAD_TILE_CHUNKS x FORCELOAD_TILE_CHUNKS chunks
    each (<= FORCELOAD_CHUNK_LIMIT chunks per tile), returned as
    (bx0, bz0, bx1, bz1) block-coordinate rectangles ready for
    `forceload add`/`forceload remove`. A rectangle that already fits under
    the cap comes back as a single tile, unchanged from the pre-tiling
    behaviour of this function's callers."""
    cx0, cx1 = x_lo // 16, x_hi // 16  # inclusive chunk coordinates
    cz0, cz1 = z_lo // 16, z_hi // 16
    tiles = []
    cx = cx0
    while cx <= cx1:
        cx2 = min(cx + FORCELOAD_TILE_CHUNKS - 1, cx1)
        cz = cz0
        while cz <= cz1:
            cz2 = min(cz + FORCELOAD_TILE_CHUNKS - 1, cz1)
            bx0 = max(x_lo, cx * 16)
            bx1 = min(x_hi, cx2 * 16 + 15)
            bz0 = max(z_lo, cz * 16)
            bz1 = min(z_hi, cz2 * 16 + 15)
            tiles.append((bx0, bz0, bx1, bz1))
            cz = cz2 + 1
        cx = cx2 + 1
    return tiles


def forceload(client: RconClient, origin: tuple[int, int, int], size: tuple[int, int, int]) -> None:
    ox, _, oz = origin
    sx, _, sz = size
    margin = 8
    x_lo, x_hi = ox - margin, ox + sx + margin
    z_lo, z_hi = oz - margin, oz + sz + margin
    for bx0, bz0, bx1, bz1 in _forceload_tiles(x_lo, z_lo, x_hi, z_hi):
        client.command(f"forceload add {bx0} {bz0} {bx1} {bz1}")


def forceload_release(client: RconClient, origin: tuple[int, int, int], size: tuple[int, int, int]) -> None:
    ox, _, oz = origin
    sx, _, sz = size
    margin = 8
    x_lo, x_hi = ox - margin, ox + sx + margin
    z_lo, z_hi = oz - margin, oz + sz + margin
    for bx0, bz0, bx1, bz1 in _forceload_tiles(x_lo, z_lo, x_hi, z_hi):
        client.command(f"forceload remove {bx0} {bz0} {bx1} {bz1}")


class RegionNotReady(RuntimeError):
    """Raised when the world does not confirm the *environment* state this
    harness depends on -- a loaded region, a genuinely blank canvas, or
    (see assert_quiescent) the blocks we placed actually being physically
    present at the positions we placed them -- within a generous timeout.
    Deliberately narrow: this class is about the world failing to do what
    we told it, never about a circuit computing the wrong answer. A test
    that proceeds on an unconfirmed world is exactly the false-failure risk
    this class exists to remove: see the module docstring's "Robustness
    against a racing world" section. Its counterpart for a circuit that IS
    physically present but logically wrong is CircuitMismatch, below."""


class CircuitMismatch(RuntimeError):
    """Raised when the world did exactly what we asked -- every block we
    placed is confirmed present at its position by identity, and the
    signals we're reading are no longer changing -- but the circuit
    disagrees with the pure-boolean simulator anyway, or never stopped
    changing at all (oscillation). Exit code 1, the same category a
    mismatch discovered mid-sweep gets: unlike RegionNotReady, this means
    look at the compiled circuit, not at the server."""


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


def block_id_present(client: RconClient, pos: tuple[int, int, int], block_id: str) -> bool:
    """Pure block-identity check: is `block_id` sitting at `pos`, regardless
    of any blockstate property (lit, powered, facing, delay, ...)? Vanilla's
    `execute if block` with no bracketed state treats every property as a
    wildcard, so this answers "did placement land here" without needing to
    know anything about how the block's redstone state ought to look --
    that is a separate question, answered by read_lamp/read_torch below."""
    x, y, z = pos
    return client.command(f"execute if block {x} {y} {z} {block_id}").strip().startswith("Test passed")


def verify_positions_placed(
    client: RconClient, checks: list[tuple[tuple[int, int, int], str]]
) -> list[str]:
    """Given [(position, expected_block_id), ...], return a description of
    every position that does not actually hold the expected block. This is
    the same kind of check verify_region_is_air already makes (a bare
    block-id match over RCON), just pointed at "the right non-air block"
    instead of "air" -- it confirms placement landed, nothing more, and in
    particular says nothing about whether the circuit's logic is right."""
    missing = []
    for pos, block_id in checks:
        if not block_id_present(client, pos, block_id):
            missing.append(f"{pos}: expected block id {block_id!r}, not present")
    return missing


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


def read_merge_junction(client: RconClient, pos: tuple[int, int, int]) -> int | None:
    """Read a wired-OR merge's value off its dust junction.

    A merge has no torch to read (see `Gate`): its `GATEOUT` position is the
    bare dust cell where its inputs' routes join, and the value it carries is
    whether that dust is powered at all. `power=0` is the one state that
    means "low"; every other power level (1..15) means high, so this asks the
    `power=0` question and treats a present-but-not-zero wire as 1 rather
    than enumerating fifteen power levels over fifteen RCON round trips.

    "Non-zero power at the junction" is not this harness's own invention of
    what a merge means -- it is the same predicate the compiler itself
    verifies for a merge that drives a declared output (see
    src/compile/mod.rs's `delivers` check: "does `pin` ... ever receive a
    non-zero signal"), read back out of the real world instead of the
    router's model of it.
    """
    x, y, z = pos
    if client.command(f"execute if block {x} {y} {z} minecraft:redstone_wire[power=0]").strip().startswith(
        "Test passed"
    ):
        return 0
    if client.command(f"execute if block {x} {y} {z} minecraft:redstone_wire").strip().startswith("Test passed"):
        return 1
    return None


def expected_block_at(gate: Gate) -> str:
    """The block id that must physically be at `gate`'s GATEOUT position: a
    wall torch for a NOR, plain dust for a merge's junction."""
    return "minecraft:redstone_wire" if gate.is_merge else "minecraft:redstone_wall_torch"


def read_state(
    client: RconClient,
    dump: Dump,
    origin: tuple[int, int, int],
) -> tuple[dict[str, int | None], dict[str, int | None]]:
    """Read every output lamp and every gate's output torch once, as a
    snapshot. Generalised from a single `output_pos` to `dump.outputs` (any
    number of lamps) -- and4 has one, the seven-segment decoder has seven;
    nothing below needs to know which."""
    ox, oy, oz = origin
    lamp_vals = {}
    for output_name, output_pos in dump.outputs.items():
        pos = (ox + output_pos[0], oy + output_pos[1], oz + output_pos[2])
        lamp_vals[output_name] = read_lamp(client, pos)
    gate_vals = {}
    kind_of = {gate.name: gate for gate in dump.gates}
    for gate_name, gate_pos in dump.gate_outputs.items():
        pos = (ox + gate_pos[0], oy + gate_pos[1], oz + gate_pos[2])
        gate = kind_of.get(gate_name)
        if gate is not None and gate.is_merge:
            gate_vals[gate_name] = read_merge_junction(client, pos)
        else:
            gate_vals[gate_name] = read_torch(client, pos)
    return lamp_vals, gate_vals


def gate_logic_depth(gates: list[Gate]) -> int:
    """Longest dependency chain among the netlist's gates, one level per NOR
    gate -- the same "logic-depth lower bound" concept src/timing/mod.rs
    computes for the compiler itself (see that module's docstring),
    reimplemented here from the dump's own GATE lines because this script
    has no access to the Rust Netlist type, only its text dump. Primary
    inputs (levers) contribute depth 0; each gate's depth is one more than
    its deepest dependency. `gates` is already topologically ordered (mc_dump
    preserves that), so a single forward pass is enough. Used only to size
    compute_settle_ceiling's margin below -- not itself a timing prediction."""
    depth: dict[str, int] = {}
    for gate in gates:
        depth[gate.name] = 1 + max((depth.get(i, 0) for i in gate.inputs), default=0)
    return max(depth.values(), default=0)


# Real vanilla constants this project has already independently measured
# against a live 1.20.1 server and encoded in the simulator (see
# src/redstone/simulator/component.rs and conformance/results/1.20.1.json's
# repeater_delay_is_two_to_eight_game_ticks / lamp entries) -- reused here
# rather than re-guessed, so compute_settle_ceiling below is grounded in
# measured redstone behaviour, not picked to make today's decoder pass.
TORCH_DELAY_GAME_TICKS = 2
LAMP_TURN_OFF_DELAY_GAME_TICKS = 4
# Doubles the raw physical estimate below to cover two things that are NOT
# circuit physics: RCON round-trip/poll-interval slop, and "average repeaters
# per gate" being, by construction, an underestimate for whichever gate sits
# above the average.
SETTLE_CEILING_SAFETY_FACTOR = 2.0
# A dump with few or no repeaters (a trivial circuit) still deserves a
# non-instant ceiling: a floor of real seconds for RCON latency and tick
# jitter, not zero.
MIN_SETTLE_CEILING_SECONDS = 2.0


def compute_settle_ceiling(dump: Dump) -> float:
    """A generous, dump-derived upper bound on how long this specific
    circuit could plausibly take to reach a fixed point -- used as
    poll_until_quiescent's ceiling: the point past which "still changing"
    stops meaning "slow" and starts meaning "oscillating" (CircuitMismatch).

    Deliberately not a fixed constant (that is the bug this module exists to
    fix -- a flat 6.0s --settle-vector was shorter than the seven-segment
    decoder's own real 124-tick/6.2s settle time) and deliberately not a
    number a human tunes per circuit on the command line either (that just
    moves the same problem one layer up, onto whoever picks
    --settle-build/--settle-vector next). Instead it is computed from the
    dump's own structure:

    - `depth`, the longest gate dependency chain (gate_logic_depth) -- the
      number of NOR inversions any one signal could have to cross serially.
    - `avg_repeaters_per_gate` and `avg_repeater_delay_ticks`, both read
      directly from this dump's own repeater population (every repeater
      mc_dump emits carries its own `delay`; see Block.delay) rather than
      assumed. This uses the *average*, not the sum of every repeater in the
      whole circuit: most repeaters sit on parallel branches, not the one
      critical path, so summing all of them overstates a single path's cost
      by roughly an order of magnitude. Measured directly on the decoder:
      785 repeaters totalling 1570 game ticks of delay, against a real
      measured critical-path settle of 124 game ticks -- using the raw sum
      would make a genuinely oscillating decoder take a minute and a half to
      be declared failed, for no benefit.

    ceiling_ticks = depth * avg_repeaters_per_gate * avg_repeater_delay_ticks
                    + depth * TORCH_DELAY_GAME_TICKS
                    + LAMP_TURN_OFF_DELAY_GAME_TICKS
    then doubled (SETTLE_CEILING_SAFETY_FACTOR) and floored at
    MIN_SETTLE_CEILING_SECONDS.

    Measured against the two circuits this project currently has: and4 (7
    gates, 29 repeaters, depth 4) comes out to a few seconds; the
    seven-segment decoder (84 gates, 785 repeaters, depth 9) comes out to
    ~19s -- about 3x its own measured real settle time (124 ticks = 6.2s).
    That is the shape a ceiling should have: comfortable margin over any real
    circuit's actual settle time, without being so large that a genuinely
    broken one takes minutes to be reported."""
    depth = gate_logic_depth(dump.gates)
    repeaters = [b for b in dump.blocks if b.kind == "Repeater"]
    if repeaters:
        avg_repeaters_per_gate = len(repeaters) / max(1, len(dump.gates))
        avg_repeater_delay_ticks = sum(b.delay for b in repeaters) / len(repeaters) * 2  # redstone ticks -> game ticks
    else:
        avg_repeaters_per_gate = 0.0
        avg_repeater_delay_ticks = 0.0
    ceiling_ticks = (
        depth * avg_repeaters_per_gate * avg_repeater_delay_ticks
        + depth * TORCH_DELAY_GAME_TICKS
        + LAMP_TURN_OFF_DELAY_GAME_TICKS
    )
    ceiling_seconds = ceiling_ticks * GAME_TICK_S * SETTLE_CEILING_SAFETY_FACTOR
    return max(ceiling_seconds, MIN_SETTLE_CEILING_SECONDS)


# How many consecutive polls must agree, all the way down, before the
# circuit is declared settled. NOT 2: measured directly on the decoder, a
# freshly-placed build produces real digital glitches (a net taking a wrong
# value before its final one, exactly what src/timing/mod.rs calls "glitch
# count") on almost every single poll for its first ~5.5 real seconds, with
# quiet notches *between* those glitches as long as ~0.36s -- comparable to
# a single default poll_interval (0.25s). Two reads landing in the same
# notch by chance therefore looks identical to "settled" but is not: a live
# run on a genuinely fresh world hit exactly this, declaring the build
# quiescent (with a wrong value, correctly caught by the check that follows
# quiescence -- but for the wrong reason) after only ~1s, while an
# instrumented replay of the identical circuit kept glitching for another
# 4.5s before reaching its real, and correct, fixed point. Requiring several
# consecutive agreeing polls -- i.e. a continuous quiet stretch several
# times longer than the longest observed mid-transient notch -- makes that
# kind of aliasing vanishingly unlikely without meaningfully slowing down a
# circuit that has genuinely finished (the cost is (REQUIRED_STABLE_POLLS-1)
# * poll_interval tacked onto the tail of every settle, not multiplied into
# it). Expressed as a poll count rather than a fixed duration so it stays
# proportionate however poll_interval is tuned.
REQUIRED_STABLE_POLLS = 5


def poll_until_quiescent(
    client: RconClient,
    dump: Dump,
    origin: tuple[int, int, int],
    min_wait: float,
    ceiling: float,
    poll_interval: float,
    context: str,
) -> tuple[dict[str, int | None], dict[str, int | None]]:
    """Wait until every output lamp and every gate's output torch reads
    identically across REQUIRED_STABLE_POLLS consecutive polls, instead of
    sleeping a fixed guess and hoping. This is the actual fix for the bug
    described at the top of this module: a hand-tuned sleep's correctness
    depends on nobody ever building a circuit slower than the number that
    was tuned against, and when that happens the failure it produces
    (specific vectors, specific segments, a named "first bad gate") is
    indistinguishable from a real compiled-circuit fault.

    `min_wait` is honoured as a floor before the very first poll: a circuit
    cannot possibly have finished reacting to a lever flip before even one
    redstone tick has passed, so polling from t=0 would just spend RCON round
    trips confirming dead time. Those round trips are individually cheap --
    measured directly against this project's live server, a full 91-position
    read (7 output lamps + 84 gate torches, the seven-segment decoder's
    complete signal set) costs ~11ms -- but there is no reason to spend even
    that on time nothing could have changed yet, so the floor stays.

    `ceiling` bounds the *total* elapsed time (including `min_wait`). Past
    it, the required stable-poll streak has still never been reached -- this
    project treats that as an oscillating circuit, a real fault, and raises
    CircuitMismatch (never a timeout/environment error; see RegionNotReady
    vs CircuitMismatch in the module docstring). Exit code and category are
    unaffected by why the mismatch was found -- mid-sweep or here.

    Reads every gate's output torch on every poll, not just the output
    lamps: the same ~11ms measurement above showed no meaningful cost
    difference between reading 7 positions and 91, so there is no real
    trade-off to make here. Watching only the outputs would risk calling the
    circuit settled while an interior gate is still one hop from finishing
    propagation -- exactly the kind of gap a lamp-only probe cannot see but
    a later vector's mismatch would eventually expose anyway, just later and
    less legibly."""
    start = time.monotonic()
    if min_wait > 0:
        time.sleep(min_wait)
    prev = read_state(client, dump, origin)
    stable_streak = 1  # the read we just took counts as the first of the streak
    while True:
        if stable_streak >= REQUIRED_STABLE_POLLS:
            return prev
        elapsed = time.monotonic() - start
        if elapsed >= ceiling:
            raise CircuitMismatch(
                f"{context}: the circuit never stopped changing within {ceiling:.1f}s "
                f"(after an initial {min_wait:.1f}s floor), polling every {poll_interval:.2f}s and "
                f"requiring {REQUIRED_STABLE_POLLS} consecutive agreeing reads -- it never held still "
                "that long, which means the circuit is oscillating rather than merely slow to settle. "
                "This is a real circuit fault, not an environment or timing problem: a redstone "
                "circuit that never reaches a fixed point cannot be made to pass by waiting longer."
            )
        time.sleep(poll_interval)
        cur = read_state(client, dump, origin)
        if cur == prev:
            stable_streak += 1
        else:
            stable_streak = 1
        prev = cur


def assert_quiescent(
    client: RconClient,
    dump: Dump,
    origin: tuple[int, int, int],
    min_wait: float,
    ceiling: float,
    poll_interval: float,
) -> None:
    """After the initial build settles (every lever placed off), confirm
    the circuit is trustworthy before a single input vector is swept, by
    asking three separate questions in order -- see the module docstring's
    "Three defenses" section point 3 for the reasoning:

    1. Did placement land? (RegionNotReady if not -- environment problem.)
    2. Has it actually stopped changing, or is it oscillating?
    3. Having stopped changing, did it stop at the *right* values?

    (2) and (3) both raise CircuitMismatch: by that point the world is
    confirmed to hold exactly the blocks we asked for, so any remaining
    disagreement is the compiled circuit's logic, not the harness's.

    Generalised from a single output lamp to every entry in `dump.outputs`
    -- every check below already iterated "every gate"; the only and4-
    specific thing was ever the one hardcoded lamp position, which is now
    just one more dict to loop over."""
    ox, oy, oz = origin
    expected = evaluate_expected(dump.gates, {name: 0 for name in sorted(dump.inputs)})

    # 1. Placement landed? Pure block-identity check, independent of any
    # redstone state -- see block_id_present's docstring. Deliberately
    # checked before we interpret lit/powered at all, so a missing or
    # wrong block here can never be misread as a circuit-logic failure.
    checks: list[tuple[tuple[int, int, int], str]] = []
    for output_pos in dump.outputs.values():
        pos = (ox + output_pos[0], oy + output_pos[1], oz + output_pos[2])
        checks.append((pos, "minecraft:redstone_lamp"))
    gate_by_name = {gate.name: gate for gate in dump.gates}
    for gate_name, gate_pos in dump.gate_outputs.items():
        pos = (ox + gate_pos[0], oy + gate_pos[1], oz + gate_pos[2])
        gate = gate_by_name.get(gate_name)
        # A merge's junction is dust, not a torch -- checking for a torch
        # there would report a perfectly-placed circuit as RegionNotReady.
        checks.append((pos, expected_block_at(gate) if gate else "minecraft:redstone_wall_torch"))
    missing = verify_positions_placed(client, checks)
    if missing:
        raise RegionNotReady(
            "placement did not land as instructed -- these positions do not hold the "
            "block we placed there, checked by block identity alone and independent of "
            "any redstone state, so this is an environment/harness problem, not a "
            "circuit failure: " + "; ".join(missing)
        )

    # 2. Settled, or still moving? Poll until REQUIRED_STABLE_POLLS
    # consecutive reads agree on every signal (see poll_until_quiescent);
    # past `ceiling`, that function itself raises CircuitMismatch for us --
    # a circuit that never reaches a fixed point is oscillating, a
    # different failure shape from "settled but wrong".
    lamps, gates = poll_until_quiescent(
        client, dump, origin, min_wait, ceiling, poll_interval,
        context="the initial all-zero build",
    )

    # 3. Settled -- but to the values the simulator predicts?
    mismatches: list[str] = []
    for output_name, actual in lamps.items():
        if actual != expected[output_name]:
            mismatches.append(
                f"output {dump.output_display_name(output_name)}: expected {expected[output_name]}, read {actual!r}"
            )
    for gate_name, actual in gates.items():
        if actual != expected[gate_name]:
            kind = gate_by_name[gate_name].kind if gate_name in gate_by_name else "?"
            mismatches.append(f"gate {gate_name} ({kind}): expected {expected[gate_name]}, read {actual!r}")
    if mismatches:
        raise CircuitMismatch(
            "the compiled circuit disagrees with the simulator at rest -- placement is confirmed "
            "and every signal is stable, but the build settled to the wrong values before a single "
            "input vector was even swept: " + "; ".join(mismatches)
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

    input_names = sorted(dump.inputs.keys())  # a, b, c, d -- or d3, d2, d1, d0
    if not dump.outputs:
        print("error: dump has no OUTPUT lines", file=sys.stderr)
        return 1
    # Internal signal names, ordered by human label (a..g) when mc_dump
    # supplied OUTLABEL lines. Everything below is written for "however many
    # outputs the dump has" -- and4 has one, the seven-segment decoder has
    # seven -- rather than assuming exactly one.
    output_names = dump.sorted_output_names()

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
        wait_for_region_loaded(client, origin, dump.size, timeout=args.load_timeout)
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

        ceiling = args.settle_ceiling if args.settle_ceiling is not None else compute_settle_ceiling(dump)
        print(
            f"Settle ceiling for this circuit: {ceiling:.1f}s "
            f"(floors: {args.settle_build:.1f}s build / {args.settle_vector:.1f}s vector, "
            f"polling every {args.poll_interval:.2f}s)"
        )
        report["settle_ceiling_seconds"] = ceiling

        print("Checking placement landed and polling until the build settles to the expected all-zero state...")
        assert_quiescent(
            client, dump, origin,
            min_wait=args.settle_build, ceiling=ceiling, poll_interval=args.poll_interval,
        )

        current = {name: False for name in input_names}

        for vector in vectors:
            primary = {name: bit for name, bit in zip(input_names, vector)}
            expected = evaluate_expected(dump.gates, primary)
            expected_outputs = {name: expected[name] for name in output_names}

            changed = [name for name in input_names if bool(primary[name]) != current[name]]
            for name in input_names:
                b = by_name[name]
                x, y, z = ox + b.x, oy + b.y, oz + b.z
                client.command(f"setblock {x} {y} {z} {lever_state_string(b, bool(primary[name]))} replace")
            current = {name: bool(primary[name]) for name in input_names}

            vec_str = "".join(str(b) for b in vector)
            lamps, gate_readings = poll_until_quiescent(
                client, dump, origin, args.settle_vector, ceiling, args.poll_interval,
                context=f"vector {vec_str}",
            )
            actual_outputs = {name: lamps[name] for name in output_names}

            first_disagreement = None
            for gate in dump.gates:
                if (
                    gate_readings.get(gate.name) is not None
                    and gate_readings[gate.name] != expected[gate.name]
                ):
                    first_disagreement = f"{gate.name} ({gate.kind})"
                    break

            # A vector matches only if every single output does -- one wrong
            # segment out of seven is still a failing vector, not a partial
            # pass. mismatched_outputs names exactly which ones disagreed, by
            # their human label, so a decoder failure can say "segments c, e"
            # rather than just "something was wrong".
            per_output_match = {name: actual_outputs[name] == expected_outputs[name] for name in output_names}
            match = all(per_output_match.values())
            mismatched_outputs = [dump.output_display_name(n) for n in output_names if not per_output_match[n]]

            expected_by_label = {dump.output_display_name(n): expected_outputs[n] for n in output_names}
            actual_by_label = {dump.output_display_name(n): actual_outputs[n] for n in output_names}
            if len(output_names) == 1:
                summary = f"expected={next(iter(expected_by_label.values()))} actual={next(iter(actual_by_label.values()))}"
            else:
                summary = " ".join(f"{label}={actual_by_label[label]}/{expected_by_label[label]}" for label in expected_by_label)
                summary = f"[{summary}] (actual/expected)"
            print(
                f"  {vec_str} -> {summary} "
                f"{'OK' if match else 'MISMATCH'}"
                + (f" (bad outputs: {','.join(mismatched_outputs)})" if mismatched_outputs else "")
                + (f" (first bad gate: {first_disagreement})" if first_disagreement else "")
            )

            report["vectors"].append(
                {
                    "vector": vec_str,
                    "changed_levers": changed,
                    "expected_outputs": expected_by_label,
                    "actual_outputs": actual_by_label,
                    "match": match,
                    "mismatched_outputs": mismatched_outputs,
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
    ap.add_argument("--dump", required=True, help="path to mc_dump's text output for the circuit under test (any circuit mc_dump knows)")
    ap.add_argument("--properties", required=True, help="path to the target server's server.properties")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--out", required=True, help="path to write the JSON report")
    ap.add_argument("--label", required=True, help="short label for this run, e.g. 'head' or 'a1d50a4'")
    ap.add_argument("--origin", default="{},{},{}".format(*DEFAULT_ORIGIN), help="x,y,z world origin to place the circuit at")
    ap.add_argument(
        "--settle-build", type=float, default=0.5,
        help="minimum seconds to wait after the initial static build before the first quiescence "
        "poll (a floor to skip pointless early polls during dead time -- NOT the actual settle time; "
        "see poll_until_quiescent). The real stopping condition is REQUIRED_STABLE_POLLS consecutive "
        "polls of every gate and output agreeing.",
    )
    ap.add_argument(
        "--settle-vector", type=float, default=0.25,
        help="minimum seconds to wait after setting levers for one vector before the first "
        "quiescence poll (a floor, not the actual settle time -- see --settle-build)",
    )
    ap.add_argument(
        "--poll-interval", type=float, default=0.25,
        help="seconds between quiescence polls once the floor has elapsed",
    )
    ap.add_argument(
        "--settle-ceiling", type=float, default=None,
        help="override the auto-computed settle ceiling (seconds) past which a circuit that is "
        "still changing is declared oscillating (CircuitMismatch) instead of merely slow. Default: "
        "derived from this dump's own gate depth and repeater population -- see "
        "compute_settle_ceiling. Only override this to deliberately force a shorter ceiling (to "
        "exercise the oscillation failure path quickly) or a longer one (for a circuit far larger "
        "than any this project currently has).",
    )
    ap.add_argument("--vectors", default=None, help="comma-separated input vectors to test, one bit per input in sorted-input-name order, e.g. 0000,1111 (default: all 2^n)")
    ap.add_argument("--no-clear", action="store_true", help="skip the pre-build air clear (region already known clean)")
    ap.add_argument("--keep", action="store_true", help="leave the circuit standing and the region forceloaded after the run")
    ap.add_argument(
        "--load-timeout", type=float, default=15.0,
        help="seconds to wait for forceload to report the region actually loaded before giving up "
        "with RegionNotReady (exit 2). Lower this (e.g. to a fraction of a second) against a "
        "never-visited region to deliberately exercise that failure path.",
    )
    args = ap.parse_args()
    try:
        return run(args)
    except RegionNotReady as exc:
        # Deliberately distinct from a circuit-level failure (exit 1): this
        # means the world was never confirmed to be in the state we asked
        # for -- forceload, a blank canvas, or placement itself -- not that
        # the circuit disagreed with the truth table. run()'s own
        # try/finally has already cleared the region and released the
        # forceload before this is reached.
        print(f"\nERROR: {exc}", file=sys.stderr)
        return 2
    except CircuitMismatch as exc:
        # The world did what we asked; the compiled circuit computed the
        # wrong thing (or never stopped changing). Same exit code as a
        # mismatch found during the vector sweep, because it is the same
        # category of failure -- just caught one step earlier.
        print(f"\nERROR: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

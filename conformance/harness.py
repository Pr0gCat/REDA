"""Placement/verification helpers and probe-slot bookkeeping shared by every
probe in probes.py.

This module encodes lessons that cost a long, careful diagnostic session to
learn -- each one reproduced multiple times against the live server before
being trusted. They are the difference between a probe suite that answers a
real question and one that silently measures nothing.

1. **Read-back method.** `data get block` only works on block *entities*
   (chests, signs, ...). For everything a redstone rule cares about here --
   dust, repeaters, comparators, torches, levers, lamps -- the only reliable
   read-back is `execute if block <pos> minecraft:<name>[<state>]`, answered
   with the literal string "Test passed." / "Test failed.".

2. **`/forceload` is mandatory, for the whole run.** Chunks outside the
   world's permanent spawn-keep-alive area do not tick at all with zero
   players connected, regardless of `simulation-distance`. Every probe in
   this suite runs inside one `/forceload`ed region held open for the
   entire run and released at the end -- not per-probe -- because repeatedly
   adding and removing forceload around the same coordinates was itself a
   source of flaky results during development.

3. **`/setblock` does not simulate a block update the way real play does.**
   This is the load-bearing discovery of the whole exercise, found by
   spending an afternoon chasing why textbook circuits (lever -> block ->
   torch, a torch under a lamp, a redstone block behind a comparator)
   refused to respond no matter how long the probe waited:

   - A **fresh placement** of an active source (a lever, a redstone block)
     correctly notifies whatever is *directly touching it*: adjacent dust
     recomputes, and a lamp directly touching it lights.
   - A chain of **pure redstone dust** reliably propagates end to end --
     dust has aggressive custom neighbour-notification logic that keeps
     re-triggering its neighbours, confirmed with chains over a dozen
     blocks long decaying exactly one strength per block.
   - **Nothing else reliably crosses a second hop.** A plain block that is
     itself freshly powered by one neighbour does not relay that power to a
     *different* neighbour -- not to a torch, not to a lamp, not even to a
     second piece of dust -- regardless of placement order, staged delays
     (tested up to 10 real seconds), or which specific block sits in the
     middle. This was tested exhaustively with lever, redstone block, torch
     and dust all playing the "source touching the middle block" role, and
     dust, lamp, wall torch and a second dust cell all playing the "reads
     the middle block" role: every single combination failed identically.
   - **Repeaters and comparators need a floor block under them before they
     will do anything at all**, and they need their *input to genuinely
     transition* (built pointing at air, then a source is placed there as a
     second step) rather than being placed already in their "final" state.
     Skip either of those and the block sits there inert forever, no matter
     the wait -- this is not a hop-count issue, it looks like the engine
     never schedules the internal tick that would make the visible
     `powered`/`lit` property real.
   - A **lamp is an unreliable sensor for a torch or a repeater's direct
     output** even at one hop (a lamp touching a lit torch, or touching a
     freshly-transitioned repeater's front, both stayed dark in repeated
     trials) even though the identical arrangement with **dust** as the
     sensor works every time. Consequently this suite reads redstone dust's
     own `power` property wherever the rule under test allows it, and only
     uses a lamp when the source driving it is a lever, a redstone block,
     or dust directly touching it (all three confirmed reliable).

   Every probe in `probes.py` is built to stay inside these confirmed-
   reliable shapes. Where a rule from the "at minimum" list in the task
   could only be tested by crossing a second hop through a plain block --
   whether a lever's declared support survives an incorrect placement,
   whether a fence/hopper/composter's support type is enforced, whether a
   torch inverting can be *observed as a live transition* rather than a
   static snapshot -- it is left out of the executable suite and called out
   explicitly in the run report instead of silently producing a number that
   would not mean what it appears to mean.

4. **`/setblock` never enforces placement-support validity (`canSurvive`).**
   A lever with zero support on any side, placed via `/setblock`, does not
   pop off -- not immediately, not after a neighbouring block changes,
   not after ten seconds. This was tested directly and repeatedly. It means
   this suite cannot answer "which face does a lever's `face`/`facing`
   combination actually attach to" or "does a hopper/fence/composter
   support dust/a repeater/a torch" -- those are placement-validity
   questions that only show up via a real client placement or a structure/
   schematic paste (which is presumably how the project's two prior shipped
   bugs -- the missing lever `face` and the inverted repeater `facing` --
   were actually caught), not via raw `/setblock`.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field

from rcon import RconClient

# Slot geometry. Each probe owns a box SLOT_SPAN blocks wide (X), SLOT_DEPTH
# deep (Z), starting SLOT_SPAN*index blocks east of the region origin. Given
# up front in one block so nothing needs to be regenerated/loaded per probe.
SLOT_SPAN = 24
SLOT_DEPTH = 20
SLOT_HEIGHT = 10
BASE_Y = 151  # arbitrary height, well clear of the flat world's generated layers

# The whole region lives far from world spawn (0,0) -- spawn's own chunk
# keep-alive behaviour interacted unpredictably with explicit /forceload
# during development; a plain, distant, never-before-generated area avoided
# that entirely.
REGION_X = 100000
REGION_Z = 100000


@dataclass
class Pos:
    x: int
    y: int
    z: int

    def off(self, dx: int = 0, dy: int = 0, dz: int = 0) -> "Pos":
        return Pos(self.x + dx, self.y + dy, self.z + dz)

    def coords(self) -> str:
        return f"{self.x} {self.y} {self.z}"


@dataclass
class Slot:
    """One probe's private rectangle of world."""

    index: int
    client: RconClient
    origin: Pos = field(init=False)

    def __post_init__(self) -> None:
        self.origin = Pos(REGION_X + self.index * SLOT_SPAN, BASE_Y, REGION_Z)

    def at(self, dx: int, dy: int, dz: int) -> Pos:
        return self.origin.off(dx, dy, dz)

    def clear(self) -> None:
        lo = self.origin.off(-2, -2, -2)
        hi = self.origin.off(SLOT_SPAN - 3, SLOT_HEIGHT, SLOT_DEPTH - 3)
        self.client.command(f"fill {lo.coords()} {hi.coords()} minecraft:air")

    def set(self, dx: int, dy: int, dz: int, block: str) -> None:
        p = self.at(dx, dy, dz)
        resp = self.client.command(f"setblock {p.coords()} {block} replace")
        if "Could not set" in resp or ("error" in resp.lower() and "Test" not in resp):
            raise RuntimeError(f"setblock failed at {p.coords()} -> {block}: {resp!r}")

    def check(self, dx: int, dy: int, dz: int, block_match: str) -> bool:
        """block_match is the part after the coordinates, e.g.
        'minecraft:redstone_lamp[lit=true]'."""
        p = self.at(dx, dy, dz)
        resp = self.client.command(f"execute if block {p.coords()} {block_match}")
        return resp.strip().startswith("Test passed")

    def floor(self, dx: int, dz: int, dy: int = -1, block: str = "minecraft:stone") -> None:
        """Place a support floor block. Repeaters and comparators in
        particular were found to never activate at all without one."""
        self.set(dx, dy, dz, block)


def region_bounds(num_slots: int) -> tuple[int, int, int, int]:
    lo_x = REGION_X - 5
    lo_z = REGION_Z - 5
    hi_x = REGION_X + num_slots * SLOT_SPAN + 5
    hi_z = REGION_Z + SLOT_DEPTH + 5
    return lo_x, lo_z, hi_x, hi_z


def forceload_region(client: RconClient, num_slots: int) -> None:
    lo_x, lo_z, hi_x, hi_z = region_bounds(num_slots)
    client.command(f"forceload add {lo_x} {lo_z} {hi_x} {hi_z}")


def forceload_release(client: RconClient) -> None:
    client.command("forceload remove all")


def clear_region(client: RconClient, num_slots: int) -> None:
    """/fill has a 32768-block limit, so sweep it in chunks along X."""
    lo_x, lo_z, hi_x, hi_z = region_bounds(num_slots)
    y_lo, y_hi = BASE_Y - 3, BASE_Y + SLOT_HEIGHT + 2
    step = max(1, 30000 // ((hi_z - lo_z) * (y_hi - y_lo)))
    x = lo_x
    while x < hi_x:
        x2 = min(x + step, hi_x)
        client.command(f"fill {x} {y_lo} {lo_z} {x2} {y_hi} {hi_z} minecraft:air")
        x = x2 + 1


def settle(seconds: float) -> None:
    time.sleep(seconds)


def poll_until(check_fn, timeout_s: float, interval_s: float = 0.03) -> float | None:
    """Poll `check_fn()` until it is True or the timeout elapses. Returns the
    elapsed wall-clock time at the moment it first became True, or None.

    Used for the repeater-delay timing probe: Minecraft has no `/tick step`
    before 1.20.3, so measuring "how many ticks after the input changed"
    means polling wall-clock time over RCON and inferring tick counts from
    the ~50ms tick period.
    """
    start = time.monotonic()
    deadline = start + timeout_s
    while time.monotonic() < deadline:
        if check_fn():
            return time.monotonic() - start
        time.sleep(interval_s)
    return None


GAME_TICK_S = 0.05


def ticks(seconds: float | None) -> float | None:
    if seconds is None:
        return None
    return round(seconds / GAME_TICK_S, 1)

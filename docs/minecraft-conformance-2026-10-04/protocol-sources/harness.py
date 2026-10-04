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

3. **`/setblock` does not simulate a block update the way real play does --
   except through redstone dust, which is the one block type that does.**
   This took two rounds to get right. The first round concluded "a signal
   never crosses a second plain block, full stop" -- which is wrong, and
   would have quietly thrown away the ability to test anything with a
   torch, a comparator side, or a lever's real effect in a circuit. The
   corrected, narrower rule, confirmed by directly building the failing
   and succeeding shapes side by side and toggling the one variable at a
   time:

   - A **fresh placement or an in-place property change (a "toggle") of an
     active source** (a lever, a redstone block) correctly notifies
     whatever is *directly touching it*: adjacent dust recomputes, and a
     lamp directly touching it lights. This part was never in question.
   - A chain of **pure redstone dust** reliably propagates end to end, and
     -- this is the part the first round missed -- **when redstone dust's
     own power changes, it also reaches one hop past itself**: a torch, a
     lamp, or another piece of dust that is attached to or touching a
     *plain block that the changed dust is directly touching* updates
     correctly. Confirmed directly: `redstone_block -> dust -> stone ->
     wall_torch -> dust`, with the redstone block placed *last*, correctly
     inverted the torch and dropped the far dust to 0; removing the
     mediating dust cell (`redstone_block -> stone -> wall_torch` directly)
     and repeating the identical experiment left the torch lit every time.
     This matches redstone dust's real, well-documented reputation for
     triggering far more block updates than a naive implementation would
     need to -- that over-notification is exactly what makes this shape
     work. It is *not* a property of levers, redstone blocks, repeaters or
     comparators touching a plain block directly; only dust has it.
   - **Toggling an existing block in place works exactly as well as a
     fresh placement, provided a piece of dust still mediates the plain
     block in between.** Tested directly: a lever already sitting at
     `powered=false`, feeding dust that touches a shared stone a wall torch
     is attached to, correctly inverted the torch when the *same lever*
     was overwritten to `powered=true` -- no different from placing a new
     redstone block there. The only thing toggling breaks is the case that
     was already broken anyway: a lever directly touching the plain block,
     with no dust in between, does not reach a torch on that block's other
     face whether the lever is freshly placed or toggled. So the deciding
     variable is dust mediation, not placement-versus-toggle.
   - **Nothing that isn't dust reliably crosses a second hop.** A lever, a
     redstone block, or a lit repeater/comparator's output, touching a
     plain block directly with no dust in the path, does not get relayed
     to a *different* neighbour of that block -- not a torch, not a lamp,
     not a second piece of dust -- regardless of placement order, staged
     delays (tested up to 10 real seconds), or toggle-versus-placement.
     Getting a genuinely *strong* power source to reach a comparator's side
     input this way remains out of reach for the same reason: the side
     reads whether a neighbouring block is itself powered, and the only
     source strong enough to matter (a lever) does not mediate through a
     plain block the way dust does.
   - **Repeaters and comparators need a floor block under them before they
     will do anything at all**, and they need their *input to genuinely
     change* (built pointing at air, then a source is placed or toggled in
     as a second step) rather than being placed already in their "final"
     state. Skip either of those and the block sits there inert forever,
     no matter the wait -- this is not a hop-count issue, it looks like the
     engine never schedules the internal tick that would make the visible
     `powered`/`lit` property real.
   - A **lamp is an unreliable sensor for a torch or a repeater's direct
     output** even at one hop (a lamp touching a lit torch, or touching a
     freshly-transitioned repeater's front, both stayed dark in repeated
     trials) even though the identical arrangement with **dust** as the
     sensor works every time. Consequently this suite reads redstone dust's
     own `power` property wherever the rule under test allows it, and only
     uses a lamp when the source driving it is a lever, a redstone block,
     or dust directly touching it (all three confirmed reliable).
   - A newly-generated chunk region needs a few real seconds to settle
     after `/forceload add` before it reliably ticks at all -- a basic,
     always-reliable 1-hop test (redstone block directly touching dust)
     was seen to fail immediately after forceloading a brand new area and
     then pass a few seconds later with nothing else changed. This suite
     forceloads its whole region once up front and gives it a settle pause
     before the first probe runs, rather than per-probe.

5. **On 26.2, none of the above dust-mediation or genuine-transition
   techniques were reliable for repeaters, comparators, or a torch
   responding to a dust-mediated support change** -- even though every one
   of them was 100% reproducible across dozens of trials on 1.20.1. A
   repeater built pointing at air, then given a real redstone block at its
   input, never transitioned to `powered=true` on 26.2 in any trial (tested
   up to 8 real seconds, and via a lever toggle, a remove-then-replace, and
   dust mediation -- all failed identically); forcing `powered=true`
   directly, by contrast, *does* produce a real, dust-verifiable output on
   26.2, which is itself the reverse of 1.20.1 (where forcing the state
   directly was the one thing that reliably did *not* work; see point 3).
   The dust-mediated torch probes were flakier still: identical code
   produced a passing result in one run and a failing one in the next,
   across otherwise-identical trials, whereas the same probes never once
   flipped outcome across many 1.20.1 runs. Because of this, every probe in
   `probes.py` that depends on a repeater/comparator genuinely activating,
   or a torch reacting to a dust-mediated support change, is reported as
   **inconclusive on 26.2** rather than a confirmed match or divergence --
   the evidence does not distinguish "the rule changed" from "RCON-driven
   world edits reach 26.2's tick scheduler less reliably than 1.20.1's".
   26.2 does support `/tick freeze` and `/tick step` (confirmed live,
   `/tick query` answers normally) which 1.20.1 does not -- a follow-up
   attempt at these specific probes should almost certainly drive the
   world with single stepped ticks instead of wall-clock sleeps, rather
   than trying to out-wait whatever is making this unreliable.

   Every probe in `probes.py` is built to stay inside these confirmed-
   reliable shapes -- routing any plain-block hop through a dust cell where
   the rule allows it. Where a rule from the "at minimum" list could still
   only be tested by crossing a second hop through a plain block with no
   dust available to mediate it -- whether a lever's declared support
   survives an incorrect placement (a placement-validity question, not a
   power-propagation one; see point 4), whether a fence/hopper/composter's
   support type is enforced, or a comparator's side input from a genuinely
   strong (non-dust) source -- it is left out of the executable suite and
   called out explicitly in the run report instead of silently producing a
   number that would not mean what it appears to mean.

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

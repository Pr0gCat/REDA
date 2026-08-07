"""The rule probes themselves.

Each probe is a function `run(slot) -> ProbeResult`. It clears its own slot,
places a self-contained arrangement of blocks, waits for it to settle, and
reads back state with `execute if block` -- never `data get block` (see
harness.py's module docstring for why, and for the much longer list of
things that turned out not to work the way a "just place it and read it"
probe suite would naively assume).

`assumption` records, in one sentence, what our code (cited by `source`)
currently assumes. `run()` reports what the live server actually says. The
two are compared by run.py, not here -- the point of this file is to keep
"what we assume" and "what is true" as two independently-written things.

Every probe here stays inside the shapes confirmed reliable during
development (see harness.py): dust-to-dust chains, an active source
directly touching dust or a lamp, and -- crossing a second hop through a
plain block -- a dust cell mediating between a fresh or toggled source and
a component (torch, another dust cell) that is itself attached to or
touching that same plain block. What still does not work, even with dust
mediation: a *second, merely adjacent* dust cell reading a plain block's
power (only an attached component like a torch responds), and a
comparator's side input from a genuinely strong source (dust itself is
only ever a weak source, so it cannot stand in for a lever there). Those,
plus placement-validity questions no RCON command can answer at all --
lever `face` attachment, hopper/fence/composter placement-support -- are
left out of this file and called out by name in the run report instead of
silently producing a number that would not mean what it appears to mean.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Callable, Optional

from harness import Slot, settle, poll_until, ticks


@dataclass
class Check:
    question: str
    expected: bool
    actual: bool
    note: str = ""

    @property
    def matches(self) -> bool:
        return self.actual == self.expected


@dataclass
class Measurement:
    question: str
    note: str
    value_ticks: Optional[float]


@dataclass
class ProbeResult:
    checks: list = field(default_factory=list)
    measurements: list = field(default_factory=list)

    def add(self, question: str, expected: bool, actual: bool, note: str = "") -> None:
        self.checks.append(Check(question, expected, actual, note))

    def measure(self, question: str, note: str, value_s) -> None:
        self.measurements.append(Measurement(question, note, ticks(value_s)))

    @property
    def all_match(self) -> bool:
        return all(c.matches for c in self.checks)

    @property
    def all_measurements_null(self) -> bool:
        """True when this probe had measurements and every one of them
        timed out (never observed the expected transition at all) -- a
        strong signal that the underlying mechanism didn't fire, not just
        that the timing differs. A probe with no measurements is not
        flagged by this (it has nothing to time out)."""
        return bool(self.measurements) and all(m.value_ticks is None for m in self.measurements)


@dataclass
class Probe:
    name: str
    category: str
    assumption: str
    source: str
    run: Callable


PROBES: list = []


def probe(name: str, category: str, assumption: str, source: str):
    def wrap(fn):
        PROBES.append(Probe(name, category, assumption, source, fn))
        return fn

    return wrap


LAMP = "minecraft:redstone_lamp"
DUST = "minecraft:redstone_wire"
STONE = "minecraft:stone"


def dust_power(slot: Slot, dx: int, dy: int, dz: int, power: int) -> bool:
    return slot.check(dx, dy, dz, f"{DUST}[power={power}]")


def lamp_lit(slot: Slot, dx: int, dy: int, dz: int, lit: bool) -> bool:
    return slot.check(dx, dy, dz, f"{LAMP}[lit={'true' if lit else 'false'}]")


# --------------------------------------------------------------------------
# Dust connectivity
# --------------------------------------------------------------------------


@probe(
    "dust_same_layer_connects",
    "dust-connectivity",
    "Same-layer dust connects to an adjacent same-layer dust block; a "
    "one-block gap does not carry the signal across.",
    "src/redstone/simulator/connectivity.rs: dust_connections, same-layer branch",
)
def dust_same_layer(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    for dx in range(0, 5):
        slot.floor(dx, 0)
    slot.set(0, 0, 0, "minecraft:redstone_block")
    slot.set(1, 0, 0, DUST)
    slot.set(2, 0, 0, DUST)
    slot.set(3, 0, 0, DUST)
    for dx in [0, 1, 3, 4]:
        slot.floor(dx, 3)
    slot.set(0, 0, 3, "minecraft:redstone_block")
    slot.set(1, 0, 3, DUST)
    # x=2, z=3 left empty -- no floor, no dust: the gap.
    slot.set(3, 0, 3, DUST)
    settle(1.5)
    r.add("adjacent same-layer dust carries the signal (dust 3 blocks out reads power=13)", True, dust_power(slot, 3, 0, 0, 13))
    r.add("a one-block gap in the same layer stops the signal (dust past the gap reads power=0)", True, dust_power(slot, 3, 0, 3, 0))
    return r


@probe(
    "dust_climbs_when_open",
    "dust-connectivity",
    "Dust climbs a step up when the horizontal neighbour is conductive and "
    "nothing conductive sits directly above the source dust.",
    "src/redstone/simulator/connectivity.rs: dust_connections, climb branch",
)
def dust_climb_open(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    slot.floor(-1, 0)
    slot.set(-1, 0, 0, "minecraft:redstone_block")
    slot.floor(0, 0)
    slot.set(0, 0, 0, DUST)  # source dust, power 15
    slot.floor(1, 0)
    slot.set(1, 0, 0, STONE)  # conductive step
    slot.set(1, 1, 0, DUST)  # climbed dust -- (0,1,0) left open above the source
    settle(1.5)
    r.add("source dust powered", True, dust_power(slot, 0, 0, 0, 15))
    r.add("dust climbs a conductive step when nothing blocks above the source (power=14)", True, dust_power(slot, 1, 1, 0, 14))
    return r


@probe(
    "dust_climb_blocked_by_conductive_above_source",
    "dust-connectivity",
    "A conductive block directly above the source dust cuts the climb "
    "connection, even though the step itself is unchanged.",
    "src/redstone/simulator/connectivity.rs: dust_connections, "
    "`!is_conductive(world, from.up())` guard",
)
def dust_climb_blocked_above(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    slot.floor(-1, 0)
    slot.set(-1, 0, 0, "minecraft:redstone_block")
    slot.floor(0, 0)
    slot.set(0, 0, 0, DUST)
    slot.set(0, 1, 0, STONE)  # blocks the climb from directly above the source
    slot.floor(1, 0)
    slot.set(1, 0, 0, STONE)
    slot.set(1, 1, 0, DUST)
    settle(1.5)
    r.add("a conductive block above the source dust cuts the upward connection (power=0)", True, dust_power(slot, 1, 1, 0, 0))
    return r


@probe(
    "dust_climb_blocked_by_nonconductive_step",
    "dust-connectivity",
    "Dust does not climb a non-conductive step (glass): the horizontal "
    "neighbour must itself be conductive for the climb rule to fire.",
    "src/redstone/simulator/connectivity.rs: dust_connections, "
    "`neighbour_conducts` guard on the climb branch",
)
def dust_climb_blocked_glass(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    slot.floor(-1, 0)
    slot.set(-1, 0, 0, "minecraft:redstone_block")
    slot.floor(0, 0)
    slot.set(0, 0, 0, DUST)
    slot.set(1, 0, 0, "minecraft:glass")  # non-conductive step itself
    slot.set(1, 1, 0, DUST)
    settle(1.5)
    r.add("source dust powered", True, dust_power(slot, 0, 0, 0, 15))
    r.add("dust does not climb a non-conductive (glass) step (power=0)", True, dust_power(slot, 1, 1, 0, 0))
    return r


@probe(
    "dust_descends_when_open",
    "dust-connectivity",
    "Dust descends a step down when the horizontal neighbour is NOT "
    "conductive (the opposite polarity from the climb rule).",
    "src/redstone/simulator/connectivity.rs: dust_connections, descend branch",
)
def dust_descend_open(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    slot.set(-1, 1, 0, "minecraft:redstone_block")  # feeds the source dust directly
    slot.floor(0, 0, dy=0)  # floor for the source dust, one level up
    slot.set(0, 1, 0, DUST)  # source dust
    # (1,1,0) left open -- non-conductive neighbour cell, permits descend
    slot.floor(1, 0, dy=-1)  # floor for the lower dust
    slot.set(1, 0, 0, DUST)  # lower dust
    settle(1.5)
    r.add("source dust powered", True, dust_power(slot, 0, 1, 0, 15))
    r.add("dust descends a step when the neighbour cell is open (power=14)", True, dust_power(slot, 1, 0, 0, 14))
    return r


@probe(
    "dust_descend_blocked_by_conductive_neighbour",
    "dust-connectivity",
    "A conductive block in the horizontal neighbour cell blocks the "
    "descend connection.",
    "src/redstone/simulator/connectivity.rs: dust_connections, "
    "`!neighbour_conducts` guard on the descend branch",
)
def dust_descend_blocked(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    slot.set(-1, 1, 0, "minecraft:redstone_block")
    slot.floor(0, 0, dy=0)  # floor for the source dust
    slot.set(0, 1, 0, DUST)
    slot.set(1, 1, 0, STONE)  # conductive neighbour cell -- blocks descend
    slot.floor(1, 0, dy=-1)
    slot.set(1, 0, 0, DUST)
    settle(1.5)
    r.add("source dust powered", True, dust_power(slot, 0, 1, 0, 15))
    r.add("a conductive neighbour cell blocks the descend connection (power=0)", True, dust_power(slot, 1, 0, 0, 0))
    return r


# --------------------------------------------------------------------------
# Dust strength
# --------------------------------------------------------------------------


@probe(
    "dust_strength_starts_at_15",
    "dust-strength",
    "Dust directly adjacent to a power source (here: a redstone block) "
    "reads power=15.",
    "src/redstone/rules/taxonomy.rs: power_emitted_by, RedstoneBlock arm (strength: 15)",
)
def dust_strength_start(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    slot.floor(-1, 0)
    slot.set(-1, 0, 0, "minecraft:redstone_block")
    slot.floor(0, 0)
    slot.set(0, 0, 0, DUST)
    settle(1.0)
    r.add("dust adjacent to a redstone block has power=15", True, dust_power(slot, 0, 0, 0, 15))
    return r


@probe(
    "dust_strength_decays_by_one_per_block",
    "dust-strength",
    "Dust strength decreases by exactly 1 per block of distance from the source.",
    "Standard vanilla dust behaviour; the simulator's BFS propagation walk "
    "assumes it directly (src/redstone/simulator/propagate.rs, "
    "recompute_dust_strengths: `next_strength = strength - 1`)",
)
def dust_strength_decay(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    for dx in range(0, 17):
        slot.floor(dx, 0)
    slot.set(0, 0, 0, "minecraft:redstone_block")
    for dx in range(1, 17):
        slot.set(dx, 0, 0, DUST)
    settle(1.5)
    for k in [1, 5, 10, 15]:
        expected_power = 16 - k
        r.add(f"dust {k} block(s) from the source has power={expected_power}", True, dust_power(slot, k, 0, 0, expected_power))
    return r


@probe(
    "dust_dies_at_zero",
    "dust-strength",
    "The 16th dust block from a source has power=0, while the 15th still has power=1.",
    "src/redstone/rules/taxonomy.rs: power_emitted_by only emits dust when "
    "`state.power > 0`; src/redstone/simulator/propagate.rs's BFS stops at strength<=1",
)
def dust_dies_at_zero(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    for dx in range(0, 17):
        slot.floor(dx, 0)
    slot.set(0, 0, 0, "minecraft:redstone_block")
    for dx in range(1, 17):
        slot.set(dx, 0, 0, DUST)
    settle(1.5)
    r.add("dust 15 blocks from the source has power=1", True, dust_power(slot, 15, 0, 0, 1))
    r.add("dust 16 blocks from the source has power=0 (dead)", True, dust_power(slot, 16, 0, 0, 0))
    return r


# --------------------------------------------------------------------------
# Torch
# --------------------------------------------------------------------------


@probe(
    "torch_drives_adjacent_dust",
    "torch",
    "A lit torch drives directly-touching dust to full strength (15). "
    "(Only the side direction could be probed this way: dust needs a full "
    "support block directly beneath it, and a standing torch does not "
    "provide one, so dust cannot physically rest in the one cell directly "
    "above a torch -- confirmed live, the block silently fails to place "
    "there at all.)",
    "src/redstone/rules/taxonomy.rs: power_emitted_toward, Torch arm",
)
def torch_drives_dust(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    slot.floor(0, 0)
    slot.set(0, 0, 0, "minecraft:redstone_torch[lit=true]")
    slot.floor(1, 0)
    slot.set(1, 0, 0, DUST)  # beside the torch, same layer
    settle(1.5)
    r.add("dust beside a lit torch reads power=15", True, dust_power(slot, 1, 0, 0, 15))
    return r


def _dust_mediated_source(slot: Slot, feed_dx: int, feed_dz: int, target_dx: int, target_dz: int) -> None:
    """Places a dust cell at (feed_dx,0,feed_dz), touching the target
    position, then a redstone block one step further out as the actual
    source -- placed *last*, so the change is a genuine transition. This is
    the one shape confirmed to relay a signal through a second plain block
    (see harness.py): the mediating dust cell must directly touch the plain
    block the rule under test cares about."""
    slot.floor(feed_dx, feed_dz)
    slot.set(feed_dx, 0, feed_dz, DUST)
    settle(1.0)
    out_dx = feed_dx + (feed_dx - target_dx)
    out_dz = feed_dz + (feed_dz - target_dz)
    slot.set(out_dx, 0, out_dz, "minecraft:redstone_block")


@probe(
    "torch_inverts_when_its_support_is_genuinely_powered",
    "torch",
    "A torch inverts (goes dark) when the block it is attached to becomes "
    "powered by something else, and relights when that power is removed. "
    "This is the entire reason a torch can act as a NOT gate.",
    "src/redstone/simulator/component.rs: torch_should_be_lit "
    "(`block_power_at(world, support) == BlockPower::None`)",
)
def torch_inverts(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    # Support B at (1,0,0); standing torch on top at (1,1,0); dust reads
    # the torch directly at the torch's own height (side, confirmed
    # reliable). A second dust cell touches B on a free face at B's own
    # height and is fed by a redstone block placed last.
    slot.floor(1, 0)
    slot.set(1, 0, 0, STONE)  # B, the shared support
    slot.set(1, 1, 0, "minecraft:redstone_torch[lit=true]")
    slot.floor(2, 0, dy=0)  # floor directly under the reader dust, at (2,0,0)
    slot.set(2, 1, 0, DUST)  # same height as the torch -- reads it directly
    settle(1.0)
    r.add("baseline: torch's own dust reads power=15 (support unpowered)", True, dust_power(slot, 2, 1, 0, 15))
    _dust_mediated_source(slot, feed_dx=0, feed_dz=0, target_dx=1, target_dz=0)
    settle(1.5)
    r.add("feed dust (touching B) is powered", True, dust_power(slot, 0, 0, 0, 15))
    r.add("torch inverts off once its support is genuinely powered", True, slot.check(1, 1, 0, "minecraft:redstone_torch[lit=false]"))
    r.add("the torch's own dust reading drops to 0 once it is off", True, dust_power(slot, 2, 1, 0, 0))
    return r


@probe(
    "torch_does_not_power_its_own_support",
    "torch",
    "A torch does not power the block it is attached to -- if it did, a "
    "torch tower would feed itself and turn into a latch instead of an "
    "inverter. Mirrored pair on the same shared block: with only the "
    "standing torch touching it, a second (wall) torch on another face "
    "must stay lit; with a genuine external source also touching it "
    "(mediated through dust, the one shape confirmed to relay through a "
    "plain block), the same wall torch must invert off -- proving the "
    "sensor would have caught it if the rule were false. (A plain second "
    "dust cell will not do as the sensor here: dust-mediation was found to "
    "reach an *attached component* like a torch, but not a second, merely "
    "adjacent piece of dust -- see harness.py.)",
    "src/redstone/rules/taxonomy.rs: power_emitted_toward, Torch arm "
    "(`Facing::Down => PowerOutput::INERT`)",
)
def torch_does_not_power_support(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    # Negative control (dz=0): only the standing torch touches B.
    slot.floor(1, 0)
    slot.set(1, 0, 0, STONE)  # B
    slot.set(1, 1, 0, "minecraft:redstone_torch[lit=true]")
    slot.set(2, 0, 0, "minecraft:redstone_wall_torch[facing=east,lit=true]")  # attached to B (west of the torch)
    settle(1.5)
    r.add("negative control: the wall torch on B's other face stays lit (standing torch alone)", True, slot.check(2, 0, 0, "minecraft:redstone_wall_torch[lit=true]"))

    # Positive control (dz=3): identical, plus a dust-mediated external
    # source also touching B, proving the sensor inverts when B really is powered.
    slot.floor(1, 3)
    slot.set(1, 0, 3, STONE)
    slot.set(1, 1, 3, "minecraft:redstone_torch[lit=true]")
    slot.set(2, 0, 3, "minecraft:redstone_wall_torch[facing=east,lit=true]")
    settle(1.0)
    _dust_mediated_source(slot, feed_dx=0, feed_dz=3, target_dx=1, target_dz=3)
    settle(1.5)
    r.add("positive control: the wall torch inverts off once B is genuinely externally powered", True, slot.check(2, 0, 3, "minecraft:redstone_wall_torch[lit=false]"))
    return r


@probe(
    "wall_torch_facing_names_the_attached_block",
    "torch",
    "A wall torch's `facing` records the direction its head points; it is "
    "attached to the block on the *opposite* side (`facing.opposite()`). "
    "Mirrored pair: a facing=east torch sandwiched between two stone "
    "blocks (west and east) inverts when the west one is genuinely "
    "powered, and stays lit when the east one is.",
    "src/redstone/simulator/component.rs: torch_support_position, "
    "WallTorch arm (`pos.offset(facing.opposite())`)",
)
def wall_torch_facing(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    # Lane A (dz=0): power the west stone -- expected to invert the torch.
    slot.floor(0, 0)
    slot.floor(1, 0)
    slot.floor(2, 0)
    slot.set(0, 0, 0, STONE)  # west stone
    slot.set(1, 0, 0, "minecraft:redstone_wall_torch[facing=east,lit=true]")
    slot.set(2, 0, 0, STONE)  # east stone
    settle(1.0)
    _dust_mediated_source(slot, feed_dx=-1, feed_dz=0, target_dx=0, target_dz=0)
    settle(1.5)
    r.add("lane A: dust feed (touching the west stone) is powered", True, dust_power(slot, -1, 0, 0, 15))
    r.add("lane A: torch inverts off when the WEST stone is powered", True, slot.check(1, 0, 0, "minecraft:redstone_wall_torch[lit=false]"))

    # Lane B (dz=3): mirror -- power the east stone instead.
    slot.floor(0, 3)
    slot.floor(1, 3)
    slot.floor(2, 3)
    slot.set(0, 0, 3, STONE)
    slot.set(1, 0, 3, "minecraft:redstone_wall_torch[facing=east,lit=true]")
    slot.set(2, 0, 3, STONE)
    settle(1.0)
    _dust_mediated_source(slot, feed_dx=3, feed_dz=3, target_dx=2, target_dz=3)
    settle(1.5)
    r.add("lane B: dust feed (touching the east stone) is powered", True, dust_power(slot, 3, 0, 3, 15))
    r.add("lane B: torch stays lit when the EAST stone is powered (not its attachment)", True, slot.check(1, 0, 3, "minecraft:redstone_wall_torch[lit=true]"))
    return r


# --------------------------------------------------------------------------
# Repeater
# --------------------------------------------------------------------------


def _repeater_lane(slot: Slot, dz: int, facing: str, delay: int = 1) -> None:
    """West input cell (dx=-1), repeater at dx=0, east output dust at dx=1.
    Repeater starts pointed at air on its rear; the caller adds the source
    as a second step to get a genuine input transition."""
    slot.floor(-1, dz)
    slot.floor(0, dz)
    slot.set(0, 0, dz, f"minecraft:repeater[facing={facing},delay={delay},locked=false,powered=false]")
    slot.floor(1, dz)
    slot.set(1, 0, dz, DUST)


@probe(
    "repeater_facing_names_the_input_side",
    "repeater",
    "`facing` names the direction from the repeater's output side to its "
    "input side (input = facing, output = facing.opposite()). Mirrored "
    "pair: with a source fixed to the west and a dust reader fixed to the "
    "east, only facing=west (input=west, matching the source) should light "
    "the reader; facing=east should not.",
    "src/redstone/simulator/component.rs: repeater_input_position "
    "(`pos.offset(facing)`), cites Minecraft Wiki: \"facing: the direction "
    "from the output side to the input side of a repeater\"",
)
def repeater_facing(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    _repeater_lane(slot, 0, "east")  # wrong orientation for this fixed geometry
    _repeater_lane(slot, 3, "west")  # correct orientation
    settle(1.0)
    slot.set(-1, 0, 0, "minecraft:redstone_block")
    slot.set(-1, 0, 3, "minecraft:redstone_block")
    settle(2.5)
    r.add("facing=east (input on the far side from the source) stays dark", True, dust_power(slot, 1, 0, 0, 0))
    r.add("facing=west (input on the source side) lights the output", True, dust_power(slot, 1, 0, 3, 15))
    return r


@probe(
    "repeater_delay_is_two_to_eight_game_ticks",
    "repeater",
    "delay=1..4 (redstone ticks) means 2, 4, 6, 8 game ticks (100/200/300/400ms) "
    "before the output responds to a genuine input change.",
    "src/redstone/simulator/component.rs: repeater_delay_game_ticks "
    "(`redstone_ticks * 2`)",
)
def repeater_delay(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    for i, delay in enumerate([1, 2, 3, 4]):
        _repeater_lane(slot, i * 3, "west", delay=delay)
    settle(1.0)
    results = {}
    for i, delay in enumerate([1, 2, 3, 4]):
        dz = i * 3
        slot.set(-1, 0, dz, "minecraft:redstone_block")
        elapsed = poll_until(lambda dz=dz: dust_power(slot, 1, 0, dz, 15), timeout_s=2.0)
        results[delay] = elapsed
    for delay, elapsed in results.items():
        expected_ticks = delay * 2
        r.measure(
            f"delay={delay}: measured time from input transition to output reaching full strength",
            f"expected ~{expected_ticks} game ticks ({expected_ticks*50}ms)",
            elapsed,
        )
    return r


@probe(
    "repeater_locking",
    "repeater",
    "A powered diode facing into a repeater's side locks it: its output "
    "does not respond to its own input changing while locked.",
    "src/redstone/simulator/component.rs: repeater_is_locked",
)
def repeater_locking(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    # Locked lane (dz=0): side repeater S faces south into M's north side, S is lit.
    slot.floor(0, 0)
    slot.set(0, 0, 0, "minecraft:repeater[facing=north,delay=1,locked=false,powered=false]")
    slot.floor(0, -1)
    slot.floor(0, 1)
    slot.set(0, 0, 1, "minecraft:repeater[facing=west,delay=1,locked=false,powered=false]")
    slot.floor(1, 1)
    slot.set(1, 0, 1, DUST)
    slot.floor(-1, 1)
    settle(1.0)
    slot.set(0, 0, -1, "minecraft:redstone_block")  # S's rear -- transitions S on
    settle(1.0)
    slot.set(-1, 0, 1, "minecraft:redstone_block")  # M's rear -- transitions M's input on
    settle(2.5)
    r.add("side repeater S is lit (feeding into M's side)", True, slot.check(0, 0, 0, "minecraft:repeater[powered=true]"))
    r.add("main repeater M reports locked=true", True, slot.check(0, 0, 1, "minecraft:repeater[locked=true]"))
    r.add("M's output stays dark while locked", True, dust_power(slot, 1, 0, 1, 0))

    # Unlocked lane (dz=4): identical, but S's rear is never powered.
    slot.floor(0, 4)
    slot.set(0, 0, 4, "minecraft:repeater[facing=north,delay=1,locked=false,powered=false]")
    slot.floor(0, 3)
    slot.floor(0, 5)
    slot.set(0, 0, 5, "minecraft:repeater[facing=west,delay=1,locked=false,powered=false]")
    slot.floor(1, 5)
    slot.set(1, 0, 5, DUST)
    slot.floor(-1, 5)
    settle(1.0)
    slot.set(-1, 0, 5, "minecraft:redstone_block")
    settle(2.5)
    r.add("unlocked control: S stays dark (never fed)", True, slot.check(0, 0, 4, "minecraft:repeater[powered=false]"))
    r.add("unlocked control: M's output lights normally", True, dust_power(slot, 1, 0, 5, 15))
    return r


# --------------------------------------------------------------------------
# Comparator
# --------------------------------------------------------------------------


def _comparator_lane(slot: Slot, dz: int, facing: str, mode: str = "compare") -> None:
    slot.floor(-1, dz)
    slot.floor(0, dz)
    slot.set(0, 0, dz, f"minecraft:comparator[facing={facing},mode={mode},powered=false]")
    slot.floor(1, dz)
    slot.set(1, 0, dz, DUST)


@probe(
    "comparator_facing_names_the_input_side",
    "comparator",
    "`facing` has the same meaning as a repeater's: the direction from the "
    "output side to the (rear) input side. Same mirrored-pair shape as the "
    "repeater facing probe.",
    "src/redstone/simulator/component.rs: comparator_rear_position "
    "(`pos.offset(facing)`), cites the same Wiki convention as the repeater",
)
def comparator_facing(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    _comparator_lane(slot, 0, "east")
    _comparator_lane(slot, 3, "west")
    settle(1.0)
    slot.set(-1, 0, 0, "minecraft:redstone_block")
    slot.set(-1, 0, 3, "minecraft:redstone_block")
    settle(2.5)
    r.add("facing=east (input on the far side from the source) stays dark", True, dust_power(slot, 1, 0, 0, 0))
    r.add("facing=west (input on the source side) lights the output", True, dust_power(slot, 1, 0, 3, 15))
    return r


def _dust_chain_then_comparator(slot: Slot, dz: int, chain_len: int, mode: str) -> None:
    """redstone_block at dx=0, a `chain_len`-long dust chain at dx=1..chain_len
    (so the cell touching the comparator's rear reads power = 16-chain_len),
    comparator at dx=chain_len+1 (facing=west, rear=chain_len), output dust
    at dx=chain_len+2. Everything built left-to-right, no negative dx, so it
    can never spill into a neighbouring slot."""
    slot.floor(0, dz)
    slot.set(0, 0, dz, "minecraft:redstone_block")
    for k in range(1, chain_len + 1):
        slot.floor(k, dz)
        slot.set(k, 0, dz, DUST)
    comparator_dx = chain_len + 1
    slot.floor(comparator_dx, dz)
    slot.set(comparator_dx, 0, dz, f"minecraft:comparator[facing=west,mode={mode},powered=false]")
    slot.floor(comparator_dx + 1, dz)
    slot.set(comparator_dx + 1, 0, dz, DUST)


@probe(
    "comparator_compare_mode_passes_rear_signal",
    "comparator",
    "In compare mode, with the side unpowered, the comparator passes its "
    "rear signal through unchanged (here: rear=9 via a 7-long dust chain).",
    "src/redstone/simulator/component.rs: comparator_output, compare-mode branch",
)
def comparator_compare_mode(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    dz = 0
    chain_len = 7  # rear cell power = 16-7 = 9
    _dust_chain_then_comparator(slot, dz, chain_len, mode="compare")
    settle(2.5)
    r.add("rear dust (touching the comparator) reads power=9", True, dust_power(slot, chain_len, 0, dz, 9))
    r.add("comparator output dust reads power=9 (passthrough)", True, dust_power(slot, chain_len + 2, 0, dz, 9))
    return r


@probe(
    "comparator_subtract_mode_with_unpowered_side",
    "comparator",
    "In subtract mode with the side unpowered (0), output = rear - 0 = "
    "rear -- the same passthrough value as compare mode. (Subtracting an "
    "actual non-zero side value could not be independently verified: the "
    "side reads whether a *neighbouring block* is itself powered, which is "
    "inherently a second hop through a plain block -- the one shape "
    "confirmed to never propagate via `/setblock`; see harness.py.)",
    "src/redstone/simulator/component.rs: comparator_output, "
    "`rear.saturating_sub(side)` in the subtract-mode branch",
)
def comparator_subtract_mode(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    dz = 0
    chain_len = 7
    _dust_chain_then_comparator(slot, dz, chain_len, mode="subtract")
    settle(2.5)
    r.add("output dust reads power=9 (9 - 0 side = 9)", True, dust_power(slot, chain_len + 2, 0, dz, 9))
    return r


# --------------------------------------------------------------------------
# Lamp
# --------------------------------------------------------------------------


@probe(
    "lamp_lights_from_weak_power",
    "lamp",
    "A lamp does not distinguish weak from strong power -- dust resting on "
    "top of a lamp (which only ever weakly powers the block beneath it) "
    "still lights it.",
    "src/redstone/simulator/component.rs: lamp_should_be_lit "
    "(`block_power_at(world, pos) != BlockPower::None`, no weak/strong distinction)",
)
def lamp_weak_power(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    slot.floor(0, 0)
    slot.set(0, 0, 0, LAMP)
    slot.set(0, 1, 0, DUST)  # rests directly on top of the lamp
    slot.set(1, 1, 0, "minecraft:redstone_block")  # directly adjacent to that dust
    settle(1.5)
    r.add("dust on the lamp is powered", True, dust_power(slot, 0, 1, 0, 15))
    r.add("lamp lights from dust resting on top of it (weak power)", True, lamp_lit(slot, 0, 0, 0, True))
    return r


@probe(
    "lamp_on_off_delay",
    "lamp",
    "Our simulator applies the same delay (2 game ticks) symmetrically "
    "both turning on and turning off, as a documented simplification. Real "
    "Minecraft lights a lamp immediately and only delays turning it off by "
    "one redstone tick (2 game ticks), to avoid flicker.",
    "src/redstone/simulator/component.rs: LAMP_DELAY_GAME_TICKS and its "
    "doc comment, which states this asymmetry explicitly and why the "
    "simulator does not replicate it",
)
def lamp_delay(slot: Slot) -> ProbeResult:
    r = ProbeResult()
    slot.floor(0, 0)
    slot.set(0, 0, 0, LAMP)
    settle(1.0)
    slot.set(1, 0, 0, "minecraft:redstone_block")
    on_elapsed = poll_until(lambda: lamp_lit(slot, 0, 0, 0, True), timeout_s=1.5)
    r.measure("delay from power applied to lamp lit (\"on\" delay)", "our code assumes ~2 ticks; vanilla is documented as ~0 (instant)", on_elapsed)
    settle(1.0)
    slot.set(1, 0, 0, "minecraft:air")
    off_elapsed = poll_until(lambda: lamp_lit(slot, 0, 0, 0, False), timeout_s=1.5)
    r.measure("delay from power removed to lamp dark (\"off\" delay)", "our code and vanilla both expect ~2 ticks", off_elapsed)
    return r


# --------------------------------------------------------------------------
# Conductivity (via the dust climb mechanic, the one reliable indirect test)
# --------------------------------------------------------------------------


def _climb_probe(name: str, block: str, expected_climbs: bool, assumption: str, source: str):
    @probe(name, "conductivity", assumption, source)
    def run(slot: Slot) -> ProbeResult:
        r = ProbeResult()
        slot.floor(-1, 0)
        slot.set(-1, 0, 0, "minecraft:redstone_block")
        slot.floor(0, 0)
        slot.set(0, 0, 0, DUST)
        slot.set(1, 0, 0, block)  # the candidate block itself, as the climb step
        slot.set(1, 1, 0, DUST)
        settle(1.5)
        r.add("source dust powered", True, dust_power(slot, 0, 0, 0, 15))
        r.add(
            f"dust climbs over {block} ({'conductive' if expected_climbs else 'non-conductive'} expected)",
            expected_climbs,
            dust_power(slot, 1, 1, 0, 14),
        )
        return r

    return run


conducts_stone = _climb_probe(
    "conducts_stone",
    STONE,
    True,
    "minecraft:stone is a full, conductive block -- the baseline substrate "
    "block the compiler actually places.",
    "src/redstone/rules/java_1_20.rs: CONDUCTIVE_FULL_BLOCKS includes "
    "minecraft:stone; src/compile/mod.rs places minecraft:stone as its solid substrate",
)

conducts_glass = _climb_probe(
    "conducts_glass",
    "minecraft:glass",
    False,
    "minecraft:glass is a full block (holds dust, holds a repeater) but "
    "does not conduct.",
    "src/redstone/rules/java_1_20.rs: NON_CONDUCTIVE includes minecraft:glass",
)

conducts_soul_sand = _climb_probe(
    "conducts_soul_sand",
    "minecraft:soul_sand",
    True,
    "minecraft:soul_sand conducts despite not being a full cube -- an "
    "explicit exception, not a consequence of its shape.",
    "src/redstone/rules/java_1_20.rs: CONDUCTIVE_EXCEPTIONS includes minecraft:soul_sand",
)

conducts_honey_block = _climb_probe(
    "conducts_honey_block",
    "minecraft:honey_block",
    False,
    "minecraft:honey_block is a full cube (matches the generic `_block` "
    "suffix rule for shape) but is explicitly listed as non-conductive.",
    "src/redstone/rules/java_1_20.rs: NON_CONDUCTIVE includes "
    "minecraft:honey_block; FULL_BLOCK_SUFFIXES's `_block` suffix would "
    "otherwise misjudge its conductivity if checked before the NON_CONDUCTIVE list",
)

conducts_target = _climb_probe(
    "conducts_target",
    "minecraft:target",
    True,
    "minecraft:target conducts -- an explicit exception alongside emitting "
    "its own analog signal.",
    "src/redstone/rules/java_1_20.rs: CONDUCTIVE_EXCEPTIONS includes minecraft:target",
)

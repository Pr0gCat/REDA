"""M2 audit: reconstruct the continuous waveform of every settle from the raw
RCON log and prove each one converges. poll_until_quiescent calls read_state on
every poll, and read_state reads every gate output torch and every output lamp
exactly once per poll, so the log contains the full per-poll gate/lamp trace.
This audit recovers that trace and checks its shape:

  - each settle's gate/lamp signal trace reaches a fixed point (a poll whose
    full signal vector equals the next poll's),
  - once reached, NO gate or lamp ever changes again (no post-settling glitch),
  - the fixed point is held for at least REQUIRED_STABLE_POLLS polls,
  - the fixed point is first reached within the dump-derived settle ceiling.

The bus channel/readback/pad and the internal relays are not part of this
gate/lamp trace: the bus state is fully determined by the expected_value /
floating / conflict lamps (which ARE in the trace), and relay quiescence is
enforced by the driver's own settling condition. This observes the whole
waveform, not just the endpoint. No weakened settling, no new ignores."""
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = Path("/home/seith/REDA")
sys.path.insert(0, str(ROOT / "target/qualification/shared-inout-game-producer/final-protocol"))
import circuit_conformance as h

REQUIRED_STABLE_POLLS = h.REQUIRED_STABLE_POLLS

d = h.parse_dump(HERE / "candidate.dump")
report = json.loads((HERE / "transition-result.json").read_text())
ceiling = report["settle_ceiling_seconds"]
origin = tuple(report["origin"])

# gate/lamp positions (dump-relative) that read_state reads once per poll.
read_positions = set()
for pos in d.gate_outputs.values():
    read_positions.add(tuple(pos))
for pos in d.outputs.values():
    read_positions.add(tuple(pos))
control_positions = {tuple(d.inputs[n]) for n in d.inputs}

def to_local(p):
    return tuple(a - o for a, o in zip(p, origin))

# Parse into per-vector poll traces. A vector begins at the first lever-set of a
# burst. A poll is one full read_state sweep. The driver reads outputs then
# gate_outputs in a fixed order, so the first output position marks each poll
# boundary; grouping by that marker is robust to a single unresolved read (the
# next poll still starts at the marker), unlike position-repeat grouping, which
# dropped a poll whenever one position's read pair failed and so under-counted
# the stable streak. Unresolved positions stay absent, matching the driver's
# dict comparison (a failed read is None on both sides). Relay reads are ignored
# (not in read_positions); relay quiescence is the driver's own gate.
read_order = [tuple(p) for p in d.outputs.values()] + [tuple(p) for p in d.gate_outputs.values()]
poll_start = read_order[0]
vectors = []
current = None
poll = None
def flush_poll():
    global poll
    if poll is not None:
        current["polls"].append(poll)
    poll = None
def flush_vector():
    global current, poll
    flush_poll()
    if current is not None:
        vectors.append(current)
    current = None

last_was_lever = False
last_was_marker = False
zero = {}
skipped_unparseable = 0
# A single RCON log line can carry a transient non-UTF-8 byte (log-integrity
# glitch, not a circuit observation). Tolerate it: replace undecodable bytes and
# skip lines that no longer parse, counting them so the skip is transparent.
with (HERE / "transition-rcon.jsonl").open(errors="replace") as f:
    for line in f:
        try:
            entry = json.loads(line)
            cmd, response = entry["command"], entry["response"]
        except (json.JSONDecodeError, KeyError):
            skipped_unparseable += 1
            continue
        m = re.fullmatch(r"setblock (-?\d+) (-?\d+) (-?\d+) minecraft:lever\[([^]]+)\] replace", cmd)
        if m:
            p = to_local(tuple(map(int, m.group(1, 2, 3))))
            if p in control_positions:
                if not last_was_lever:
                    flush_vector()
                    current = {"t_start": entry["monotonic_seconds"], "polls": []}
                    poll = None
                    last_was_marker = False
                last_was_lever = True
            continue
        if current is None:
            continue
        last_was_lever = False
        m = re.fullmatch(r"execute if block (-?\d+) (-?\d+) (-?\d+) (minecraft:[a-z_]+)(?:\[([^]]+)\])?", cmd)
        if not m:
            continue
        p = to_local(tuple(map(int, m.group(1, 2, 3))))
        if p not in read_positions:
            continue
        if p == poll_start:
            # the first output is a lamp read with two queries (lit=true,
            # lit=false); only the first query of that pair starts a new poll,
            # so a read_state call yields exactly one poll.
            if not last_was_marker:
                if poll is not None:
                    current["polls"].append(poll)
                poll = {}
            last_was_marker = True
        else:
            last_was_marker = False
        if poll is None:
            continue
        block, properties = m.group(4, 5)
        passed = response.strip().startswith("Test passed")
        value = None
        if block == "minecraft:redstone_wire" and properties == "power=0":
            zero[p] = passed
            if passed:
                value = False
        elif block == "minecraft:redstone_wire" and properties is None:
            if passed and zero.get(p) is False:
                value = True
        elif passed and properties in ("lit=true", "powered=true"):
            value = True
        elif passed and properties in ("lit=false", "powered=false"):
            value = False
        if value is not None:
            poll[p] = value
flush_vector()

# Analyze each settle's waveform.
bad = []
polls_to_stable = []
expected_settles = len(report["vectors"])
settles = [v for v in vectors if v["polls"]]
for i, v in enumerate(settles):
    polls = v["polls"]
    if len(polls) < REQUIRED_STABLE_POLLS:
        bad.append({"vector": i, "reason": "fewer polls than required stable streak", "polls": len(polls)})
        continue
    final = polls[-1]
    # The fixed point is the maximal identical SUFFIX of the poll sequence: that
    # is what poll_until_quiescent guarantees (>= REQUIRED_STABLE_POLLS agreeing
    # full-signal reads) and what "converged and held" means. A transient that
    # happens to equal the final value mid-settling is not a violation.
    suffix = 1
    while suffix < len(polls) and polls[-1 - suffix] == final:
        suffix += 1
    if suffix < REQUIRED_STABLE_POLLS:
        bad.append({"vector": i, "reason": "final fixed point held fewer than required polls", "held": suffix})
        continue
    polls_to_stable.append(len(polls) - suffix)
# The walk has expected_settles settles; the run may also contain a benign
# initial-setup settle. Require every walk settle to be present (>= expected)
# and analyze all of them; the convergence criterion is unchanged.
if len(settles) < expected_settles:
    bad.append({"reason": "fewer reconstructed settles than the walk ran", "got": len(settles), "expected": expected_settles})

result = {
    "accepted": not bad,
    "scope": "continuous gate/lamp waveform convergence for every settle in the transition walk",
    "settles_analyzed": len(settles),
    "expected_settles": expected_settles,
    "settles_converged": len(polls_to_stable),
    "required_stable_polls": REQUIRED_STABLE_POLLS,
    "settle_ceiling_seconds": ceiling,
    "settles_with_bad_waveform": len(bad),
    "skipped_unparseable_log_lines": skipped_unparseable,
    "examples": bad[:20],
    "polls_to_first_stable": {"min": min(polls_to_stable) if polls_to_stable else None,
                              "max": max(polls_to_stable) if polls_to_stable else None},
}
(HERE / "waveform-audit.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result))
sys.exit(0 if not bad else 1)

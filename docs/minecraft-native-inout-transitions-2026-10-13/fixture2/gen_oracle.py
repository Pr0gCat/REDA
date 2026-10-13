"""Independent oracle for the second shared-inout fixture. Same general
multi-driver resolution reference as docs/inout-standard-oracle-2026-10-04/run.py
(Z if no active driver, X if active drivers disagree, else 0/1), but the driver
set reflects the second topology: writer a drives d0, writer b drives ~d1
(inverted), plus the external drive. Bit layout matches the first fixture:
packed_state[5:0] = {ext_value,ext_present,d1,e1,d0,e0}.

Pure Python reference model, independent of the Rust compiler. Emits
standard-oracle.csv (before,after,phase,value) over all 64x64 transitions and
states.json with the per-state resolution + checked diagnosis."""
import csv
import json
from pathlib import Path

BASE = Path(__file__).resolve().parent

def drivers(state):
    e0 = bool(state & 1); d0 = bool(state & 2)
    e1 = bool(state & 4); d1 = bool(state & 8)
    ext_p = bool(state & 16); ext_v = bool(state & 32)
    return [(e0, d0), (e1, not d1), (ext_p, ext_v)]

def reference(ds):
    active = [v for p, v in ds if p]
    if not active:
        return "z"
    if any(v != active[0] for v in active[1:]):
        return "x"
    return "1" if active[0] else "0"

def candidate(ds):
    high = any(p and v for p, v in ds)
    low = any(p and not v for p, v in ds)
    return dict(value=int(high), present=high or low, conflict=high and low,
                floating=not (high or low), valid=(high or low) and not (high and low))

rows = []
counts = {k: 0 for k in ["0", "1", "x", "z"]}
for state in range(64):
    ds = drivers(state)
    expected = reference(ds)
    counts[expected] += 1
    r = candidate(ds)
    diagnosis = "Value" if r["valid"] else "Contention" if r["conflict"] else "Floating"
    assert (expected, diagnosis) in [("0", "Value"), ("1", "Value"), ("x", "Contention"), ("z", "Floating")]
    rows.append(dict(state=state, drivers=ds, expected=expected, resolver=r, diagnosis=diagnosis))
(BASE / "states.json").write_text(json.dumps(rows, indent=2) + "\n")

with (BASE / "standard-oracle.csv").open("w", newline="") as f:
    out = csv.writer(f)
    for before in range(64):
        for after in range(64):
            for phase, state in enumerate([before, after]):
                out.writerow([before, after, phase, rows[state]["expected"]])

checked = {}
for before in range(64):
    for after in range(64):
        checked[rows[after]["diagnosis"]] = checked.get(rows[after]["diagnosis"], 0) + 1
summary = dict(states=64, ordered_transitions=4096, settled_endpoint_rows=8192,
               class_counts=counts, checked_counts_by_after_state=checked,
               model_equivalence=True, standard_simulator_evidence=False,
               topology="writer + inverted writer + external")
(BASE / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
print(json.dumps(summary))

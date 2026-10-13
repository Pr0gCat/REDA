"""Transition schedule for M1: cover every ordered (before->after) pair over
the 64 declared states (64x64 = 4096) in one walk. A complete directed graph
with self-loops is Eulerian (in-degree == out-degree == 64 at every node), so
Hierholzer yields a 4097-vertex circuit that traverses all 4096 edges exactly
once. Each vertex is a declared state; consecutive vertices are a transition.

Offline only: builds the schedule + maps each declared state to a representative
7-lever setting from expected-trace.json. No game, no server."""
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
TRACE = Path("/home/seith/REDA/target/qualification/shared-inout-game-run-current/expected-trace.json")

t = json.loads(TRACE.read_text())
control_order = t["control_order"]

# declared state -> representative controls. Use a PROTOCOL-MATCHED vector
# (caller == present&&declared) so the transition walk exercises each state in
# its real electrical class (Value/Contention/Floating), not a caller-mismatch.
rep = {}
for v in t["vectors"]:
    if v["protocol_matched"]:
        rep.setdefault(v["icarus_declared_state"], v["controls"])
assert len(rep) == 64, f"every declared state needs a matched representative, got {len(rep)}"
states = sorted(rep)
def controls_bits(s):
    # the driver maps vector bits to sorted(d.inputs), NOT control_order
    c = rep[s]
    return tuple(int(bool(c[n])) for n in sorted(control_order))

# Hierholzer on the complete digraph with self-loops over 0..63.
N = 64
adj = {u: [v for v in range(N)] for u in range(N)}  # each u -> all v (incl self)
it = {u: 0 for u in range(N)}
def edge_count():
    return sum(len(a) for a in adj.values())
total_edges = edge_count()
assert total_edges == N * N == 4096

circuit = []
stack = [0]
while stack:
    u = stack[-1]
    if it[u] < len(adj[u]):
        v = adj[u][it[u]]
        it[u] += 1
        stack.append(v)
    else:
        circuit.append(stack.pop())
circuit.reverse()
# circuit is a closed Eulerian trail: len == edges+1, starts and ends at 0.
assert len(circuit) == total_edges + 1, (len(circuit), total_edges + 1)
assert circuit[0] == circuit[-1] == 0

# verify every ordered pair appears exactly once as a consecutive edge.
edges = list(zip(circuit, circuit[1:]))
assert len(edges) == 4096
assert len(set(edges)) == 4096
assert set(edges) == {(a, b) for a in range(N) for b in range(N)}

print(f"declared states      : {N}")
print(f"transitions (edges)  : {len(edges)}")
print(f"walk length (vertices): {len(circuit)}")
print(f"all 4096 pairs covered exactly once: True")
print(f"first 12 vertices    : {circuit[:12]}")

# emit the lever schedule (one 7-bit string per vertex) for the driver.
sched = ["".join(str(b) for b in controls_bits(s)) for s in circuit]
(HERE / "transition-schedule.txt").write_text("\n".join(sched) + "\n")
print(f"wrote transition-schedule.txt ({len(sched)} vectors)")

# DFF Design H topology for Minecraft 26.2

## Decision

`GateKind::DffPosedge` will use the no-piston **Design H** implementation:
four repeaters and one redstone torch. It is a fixed macro initially; the
planner may rotate or mirror a verified macro, but may not freely rearrange
its internals.

This replaces the earlier cross-coupled-torch-latch assumption. A repeater
stores its output while a powered side repeater locks it, so the DFF's state
lives in two data repeaters. The topology must represent that control relation
explicitly.

## Positionless topology

```
D ──signal──▶ M_DATA ──signal──▶ S_DATA ──signal──▶ Q
C ──signal──▶ M_LOCK ──repeater-lock-side──▶ M_DATA
C ──signal──▶ INV_C ──signal──▶ S_LOCK ──repeater-lock-side──▶ S_DATA
```

`M_DATA`, `S_DATA`, `M_LOCK`, `S_LOCK` are repeaters; `INV_C` is a torch.
`repeater-lock-side` is not an ordinary input edge: the realiser must connect
it to a side port, never a rear data port. Support blocks, dust and absolute
coordinates remain physical-realisation choices, not topology nodes.

## Phase contract

| clock | master lock | slave lock | behaviour |
|---:|---:|---:|---|
| `C=0` | `0` | `1` | master follows D; Q holds |
| `C=1` | `1` | `0` | master holds; slave exposes captured value |

Master lock receives `C`; slave lock receives `!C` from `INV_C`. A topology
where both locking repeaters receive the same non-inverted clock is invalid:
both stages are transparent or locked together and it is not a positive-edge
DFF.

## Required proof

The simulator must trace `D,C = 0,0; C↑; C↓; D↑; C↑; D↓; C↓; C↑` and prove Q
as `?,0,0,0,1,1,1,0`. It must also prove complementary locks, no falling-edge
capture, and no high-clock D feed-through for each approved rotation.

Final certification is a **Minecraft Java 26.2 client** action: paste the
litematic and physically click D/C levers; RCON only reads Q lamps and macro
state. RCON `/setblock` cannot certify repeater transitions in 26.2.

## Sources

- [Minecraft Wiki Design H](https://minecraft.fandom.com/wiki/Redstone_circuits/Memory)
- [Minecraft Wiki repeater](https://minecraft.fandom.com/wiki/Redstone_Repeater)
- [USC master/slave D-FF lecture](https://ee.usc.edu/~redekopp/ee101/slides/EE101Lecture18.pdf)

These sources establish the physical topology and complementary-clock rule;
they do not replace the 26.2 client verification.

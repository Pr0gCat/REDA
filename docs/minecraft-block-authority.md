# Block appearance, properties and orientation: read the jar, don't reason it out

This repo has shipped the same bug three times in a row, each time because
an agent reasoned about how a Minecraft block "should" look or behave from
memory instead of reading the one place that is actually authoritative:

1. A lever's `face` property (floor/wall/ceiling) was never emitted by the
   compiler, so every lever pasted into the game popped off the wall.
2. A repeater's `facing` was inverted relative to what Minecraft actually
   does with it, silently breaking which neighbour is the input.
3. The 3D viewer's repeater model had its two torch nubs on the wrong ends
   -- the *fixed* torch is the repeater's output, the torch that *slides*
   with delay is the input, and an earlier version of this file had that
   backwards (see the "Repeater nub layout" comment in `viewer/index.html`).

None of these needed guessing. Every block's blockstate table (which model
variant to use for which combination of properties, and what rotation to
apply) and every model's exact geometry (from/to coordinates, in
sixteenths of a block) ship as plain JSON inside the game's own client jar.

## Where the jars are on this machine

`C:\Users\LTY\AppData\Roaming\.minecraft\versions\`, one subdirectory per
installed version, each containing a `<version>.jar` (plus a snapshot, if
one is installed). These are **client** jars from the Minecraft Launcher --
not the dedicated server jars under `minecraft-server/` and
`minecraft-server-26.2/` (see `docs/minecraft-server.md`), which do not
ship `assets/` at all. As of this writing there is a `1.20.1/1.20.1.jar`
(the version this repo's conformance testing targets), a `26.2/26.2.jar`
(current release), and a snapshot.

Inside a client jar, the relevant paths are:

- `assets/minecraft/blockstates/<block>.json` -- maps a block's property
  combinations (`facing=`, `powered=`, `mode=`, `face=`, ...) to a model
  and an optional rotation (`x`/`y`, in degrees) applied to that model.
- `assets/minecraft/models/block/<model>.json` -- the actual geometry:
  cuboid `elements`, each with `from`/`to` corners in a 0-16 cube (block
  space, one unit = 1/16 of a block), optionally a `parent` model to
  inherit elements from, and per-face textures/UVs.

Read these with Python's `zipfile` + `json` modules directly -- no need to
extract the jar to disk, and no need to download anything (the jars are
already on this machine).

## The utility: `tools/mc_block_info.py`

```
python tools/mc_block_info.py repeater
python tools/mc_block_info.py minecraft:comparator --version 1.20.1
python tools/mc_block_info.py lever --models-only
```

Point it at a block name and it prints that block's blockstate JSON
verbatim, then every model it references (following each model's `parent`
chain so inherited geometry is visible too), reading straight out of a
local client jar via `zipfile`. It auto-discovers a jar under
`versions/<x>/<x>.jar` (preferring an all-numeric release version name over
a snapshot) if `--jar`/`--version` are not given. See the script's own
docstring (`python tools/mc_block_info.py --help`) for the full option
list.

Use this -- not memory of "how repeaters work" -- for any future question
about a block's appearance, its valid property combinations, or which
direction a `facing`/`face` value actually points. If the script's output
disagrees with an assumption already written down somewhere in this repo
(a doc comment, a design doc, a previous agent's conclusion), the jar wins.

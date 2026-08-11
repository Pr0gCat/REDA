# Minecraft 26.2 server for redstone conformance testing

REDA's ground-truth target is **Minecraft Java 26.2**, not 1.20.1.  The
current vanilla server lives in `minecraft-server-26.2/`, runs on its bundled
JDK 25, and is the server every current fidelity or conformance result must
name. Its live `version` command reports data version **4903**.

The older `minecraft-server/` tree below is retained only as a historical
1.20.1 comparison environment. It must not be used to certify a current REDA
layout.

The target server lives under `minecraft-server-26.2/` at the repo root and
is entirely gitignored. Nothing here touches `src/`, `tests/`, or `viewer/`.

## Historical only: 1.20.1 setup

**Eclipse Temurin JDK 21** (Windows x64, `.zip`), because Minecraft 1.20.1
requires Java 17+ and the machine only had Java 8/11 installed. Resolved via
the Adoptium API (`GET
https://api.adoptium.net/v3/assets/latest/21/hotspot?architecture=x64&image_type=jdk&os=windows&vendor=eclipse`),
which returned build `jdk-21.0.12+8`:

- URL: `https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12%2B8/OpenJDK21U-jdk_x64_windows_hotspot_21.0.12_8.zip`
- SHA-256 (per Adoptium's API, matched against the download): `9ba963ee2371874a74185d18bc7bb2ab9407df7683300855ed7606e0662321d0`
- Extracted to `minecraft-server/jdk21/` (portable, not installed system-wide, not on PATH).

**Minecraft 1.20.1 `server.jar`**, resolved through Mojang's manifest chain
rather than a hardcoded URL:

1. `https://launchermeta.mojang.com/mc/game/version_manifest_v2.json` → the
   `1.20.1` entry points at
   `https://piston-meta.mojang.com/v1/packages/c2f76c955503c7143424b51e21eea87bbe5c6547/1.20.1.json`
2. That version JSON's `downloads.server` gives:
   - URL: `https://piston-data.mojang.com/v1/objects/84194a2f286ef7c14ed7ce0090dba59902951553/server.jar`
   - SHA-1: `84194a2f286ef7c14ed7ce0090dba59902951553`
3. Downloaded and verified with `sha1sum` — matched. Placed at
   `minecraft-server/server/server.jar`.

## Historical only: 1.20.1 directory layout

```
minecraft-server/           (gitignored in full)
  jdk21/                     Temurin 21, extracted, self-contained
  server/                    the runnable server (jar, world, logs, config)
  run-server.sh              starts the server with the bundled JDK (bash)
  run-server.ps1             same, for PowerShell
```

## Historical only: the one line you need to change

`minecraft-server/server/eula.txt` currently reads:

```
eula=false
```

This repo will not flip that to `true` — accepting Mojang's EULA is your
decision, not the agent's. The server refuses to start until you change that
line to `eula=true` yourself. See https://aka.ms/MinecraftEULA before you do.

## Starting and stopping the target server

```bash
./minecraft-server-26.2/run-server.sh       # git-bash / WSL
```
```powershell
.\minecraft-server-26.2\run-server.ps1      # PowerShell
```

Both scripts resolve the bundled JDK by path, so they work regardless of
what `java` is on the machine's PATH. They run the server in the foreground
(`nogui`, no interactive console window).

To stop: `Ctrl+C` in the terminal (the vanilla server has a shutdown hook
that saves and exits cleanly), or send `stop` over RCON.

## Historical only: RCON

- Enabled: `enable-rcon=true`
- Port: `25575` (`rcon.port` in `server.properties`)
- Password: `rcon.password` in `minecraft-server/server/server.properties`
  (generated once, stored in plaintext — that file is gitignored, and this is
  the standard place Minecraft itself reads the password from, so the
  harness can just parse that one file rather than needing a second secret
  store).
- The server binds to `127.0.0.1` only (`server-ip=127.0.0.1`), so RCON and
  the game port are not reachable from outside the machine.

A harness authenticates with the standard [Source RCON
protocol](https://developer.valvesoftware.com/wiki/Source_RCON_Protocol):
open a TCP socket to `127.0.0.1:25575`, send a `SERVERDATA_AUTH` packet with
the password from `server.properties`, then issue `SERVERDATA_EXECCOMMAND`
packets (`/forceload add ...`, `/setblock ...`, `/data get block ...`, etc.)
and read the responses.

## Historical only: `server.properties` choices and why

The full file is at `minecraft-server/server/server.properties`. The choices
that matter for a redstone conformance harness:

- `enable-rcon=true`, `rcon.port=25575`, `rcon.password=<generated>` — the
  harness's only way in; no player needs to log in.
- `level-type=flat`, `generate-structures=false`, `generator-settings={}` —
  a superflat world with no villages, strongholds, or terrain features nearby
  to confuse `/fill`/`/setblock` or slow down chunk generation.
- `spawn-monsters=false`, `spawn-animals=false`, `spawn-npcs=false`,
  `difficulty=peaceful` — no entities ticking near the circuit, no hostile
  mobs that could path onto redstone components or despawn/attack, no hunger
  or environmental damage to worry about if a human ever joins to look.
- `allow-nether=false` — the harness only ever needs the overworld; skips
  generating/loading a second dimension entirely.
- `spawn-protection=0` — otherwise vanilla protects a 16-block radius around
  spawn from `/setblock`/`/fill`, which would silently no-op placements near
  the origin.
- `view-distance=10`, `simulation-distance=10` — large enough that a circuit
  a human is observing keeps ticking, but note `/forceload` (per the spec)
  is what actually guarantees the circuit's chunks tick regardless of any
  player's distance from it — that's the whole point of forceloading rather
  than relying on view/simulation distance.
- `max-chained-neighbor-updates=1000000` — left at the (already generous)
  vanilla default so a circuit with many redstone updates in one tick doesn't
  get its update chain truncated.
- `online-mode=false`, `enforce-secure-profile=false` — no Mojang auth
  handshake needed for a local-only harness; avoids chat-signing errors that
  `enforce-secure-profile=true` would otherwise cause with `online-mode=false`.
- `server-ip=127.0.0.1` — binds the game port to localhost only; this server
  is not meant to be reachable from the network.
- `pvp=false`, `hardcore=false`, `gamemode=creative`, `force-gamemode=true` —
  in case a human joins to eyeball a circuit, they can fly and place/break
  blocks freely without dying or fighting anything.
- `enable-command-block=true` — convenience for manual debugging; RCON
  commands already run at full permission regardless of this setting.
- `enable-query=false`, `enable-status=false` — no reason to expose a second
  query protocol or answer server-list pings for a harness-only server.
- `level-seed=reda-conformance` — fixed, so the (irrelevant, since it's flat)
  seed is at least reproducible rather than random per generation.

## Historical only: verification performed

Confirmed, with the bundled JDK and without ever touching `eula.txt`:

1. `minecraft-server/jdk21/bin/java.exe -version` reports Temurin 21.0.12 —
   the bundled JDK runs standalone.
2. Running the server once (`java -jar server.jar nogui`) generated
   `eula.txt` (`eula=false`) and a default `server.properties`, then exited
   — this is how `eula.txt` was created, per the constraint that this repo
   would generate it rather than hand-write `eula=true`.
3. `server.properties` was then replaced with the tuned version above, and
   the server was run a second time. It loaded and re-saved
   `server.properties` with all the custom values intact (confirming the
   file parses and every key was accepted — a typo'd key would have been
   silently dropped or reset to default here), logged "You need to agree to
   the EULA in order to run the server", and exited. **No `world/` directory
   was created** — confirming the EULA gate happens before any world
   generation, RCON listener, or game-port binding.

That is as far as verification could go without accepting the EULA: **RCON
was not exercised end-to-end**, because the server never reaches the point
of opening the RCON socket while `eula=false`. Once you flip that line, the
harness's first RCON connection attempt against `127.0.0.1:25575` with the
password in `server.properties` is the next real test.

## Target server details: 26.2

`minecraft-server-26.2/` is the independent, gitignored target server. The
older `minecraft-server/` tree remains only for historical 1.20.1 comparison.

**Minecraft 26.2 `server.jar`**, resolved the same way as 1.20.1's:

1. `https://launchermeta.mojang.com/mc/game/version_manifest_v2.json` ->
   `"latest": {"release": "26.2", ...}` -> the `26.2` entry points at
   `https://piston-meta.mojang.com/v1/packages/4b74f58f68a2baae3547d5a20274079f29cafc06/26.2.json`
2. That version JSON's `downloads.server` gives:
   - URL: `https://piston-data.mojang.com/v1/objects/823e2250d24b3ddac457a60c92a6a941943fcd6a/server.jar`
   - SHA-1: `823e2250d24b3ddac457a60c92a6a941943fcd6a`
3. Downloaded and verified with `sha1sum` -- matched. Placed at
   `minecraft-server-26.2/server/server.jar`.

**JDK: a second, newer JDK was required.** The same version JSON's
`javaVersion` field reads `{"component": "java-runtime-epsilon",
"majorVersion": 25}` -- 26.2 requires Java 25, and the Temurin 21 bundled
for 1.20.1 is not new enough to run it (confirmed: 1.20.1's `javaVersion`
requirement is 17, comfortably covered by 21; 26.2's is not). Resolved
**Eclipse Temurin 25** (Windows x64, `.zip`, the current LTS) via the same
Adoptium API pattern used for JDK 21:

- URL: `https://github.com/adoptium/temurin25-binaries/releases/download/jdk-25.0.4%2B7/OpenJDK25U-jdk_x64_windows_hotspot_25.0.4_7.zip`
- SHA-256 (per Adoptium's API, matched against the download): `7caab7db43bf4b94a2e6252c699e70d90084f9aa7c943cd3414761fd540937ae`
- Extracted to `minecraft-server-26.2/jdk25/`.

Directory layout mirrors the 1.20.1 server exactly (`jdk25/`, `server/`,
`run-server.sh` / `run-server.ps1`), and `server.properties` was copied from
the tuned 1.20.1 config with the same reasoning behind every setting (see
above) -- 26.2 added a few new keys on first run (`management-server-*`,
`enable-code-of-conduct`, `accepts-transfers`, ...) which were left at their
generated defaults since none of them matter to a redstone conformance
harness.

**Current local state:** `minecraft-server-26.2/server/eula.txt` is
`eula=true`, accepted by the user after reading Mojang's EULA. The target
server has started successfully; when it is running, its RCON listener is on
localhost. The server directory is gitignored, so a fresh checkout still
requires its owner to accept the EULA before it can run.

## Conformance probe suite

`conformance/` (tracked, not gitignored -- it is ordinary project code, not
server state) holds a small RCON-driven probe suite, independent of
anything in `src/`, that asks a live server direct yes/no questions about
the redstone rules the compiler and simulator depend on, and compares the
answer against what the code assumes. See the module docstrings in
`conformance/harness.py` and `conformance/probes.py` for the methodology
(and its hard-won limitations), and `conformance/run.py --help` /
`conformance/compare.py --help` for usage.

## 26.2 RCON limitation: it cannot activate repeaters

This was measured directly on the target server on 2026-08-11.  With a
repeater placed first, then a powered dust line placed behind it, RCON reads
the dust at power 13/14 but the repeater remains `powered=false` indefinitely.
The same result occurs when the source is a real redstone block directly
behind the repeater, when an existing lever is changed from off to on, after
`/tick step`, and after removing/replacing the lever.  Conversely, forcing
the repeater block state to `powered=true` drives its output normally.

So this is a limitation of the command-placement/update path, not evidence
that REDA's router or the 26.2 redstone rule is wrong.  RCON remains useful
for static rules such as dust directionality and weak rear-block inputs, but
it cannot certify a circuit containing repeaters or comparators after it has
been built with `/setblock`.  Dynamic whole-circuit certification needs a real
26.2 client action (paste/build, then physically click the input levers); RCON
may still read the resulting lamps and block states.

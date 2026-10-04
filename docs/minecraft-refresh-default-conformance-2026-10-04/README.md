# Refresh-selected exact world: real Minecraft conformance

The existing official Minecraft Java 26.2 server completed the actual game run at origin190000,151,160000 without restart or physics/tick-rate changes. All 64 source addresses pass all 9 public occurrences, including both v/w shared-site aliases. Independent source-literal truth checks cover 576 public bits; independent evaluation of the actual 15-gate graph covers 960 observed implementation-gate values. The game driver reached terminal 0. This is stable-state conformance, not an intra-tick waveform or generic circuit claim.

The retained exact frozen-v2 world has fingerprint a4d7b9d9e82cd4f759b0d3f9d21199b183e8ee7bb5bf98c8c4d4b397e04649ba. The dump is exported directly from that world, actual graph and typed sites. This does not claim fresh current-HEAD compiler generation. Commit 8c4c03a adopted the refresh optimization separately. Typed original primary-input ownership restores the 6 exact lever positions: its source graph matches the v2 source graph, every input-boundary block matches the candidate world, and each primary-input assignment is checked against the source gate operand. No lever-coordinate ordering is guessed. dump-manifest.json records the binding/provenance proof.

Protocol files, exact input artifacts, dump, exporter, real-RCON logging wrapper and server identity were hashed before the game execution in FROZEN-SHA256SUMS. The wrapper calls the existing real RCON client and returns its exact replies; no mocked results or simulator substitute are used. The private RCON credential stays in the existing properties file and is not copied into this evidence. Official server jar SHA1 matches 823e2250d24b3ddac457a60c92a6a941943fcd6a.

```sh
python3 audit-game.py
```

Reproduction uses the existing running server and the unchanged frozen harness, with its default settling ceiling/floors. Run in a separate copy to preserve these raw artifacts:

```sh
python3 export-frozen-dump.py
python3 -u run-game.py --dump candidate.dump --properties /home/seith/REDA/minecraft-server-26.2/server/server.properties --out game-report.json --label frozen-v2-refresh-default-26.2 --origin 190000,151,160000
python3 audit-game.py
python3 audit-cleanup.py /home/seith/REDA/minecraft-server-26.2/server/server.properties
```

Preflight checked all 45 region chunks unforced. Raw game-rcon.jsonl retains all 19096 real command/reply pairs, including three successful final air-fill acknowledgements and the release acknowledgement: Unmarked 45 chunks. Independent cleanup verification then checked all 1094 original occupied positions and 8 bounding samples as Air, and all 45 region chunks unforced both before and after its own temporary loading. Its own loading was released in finally. This is complete for the original occupied positions, not an exhaustive scan of the bounding volume.

The first independent audit parser expected historical Removed release wording; 26.2 replies Unmarked 45 chunks. All truth assertions had already passed. That failed parser and the explicit correction remain retained; it was not a game failure.

#!/usr/bin/env python3
"""Print a block's real blockstate and model JSON, straight out of a local
Minecraft client jar.

Why this exists: this repo has hit the same bug three times now (lever
`face` never emitted, repeater `facing` inverted, repeater's 3D nub model
mirrored) because the fix was reasoned out from memory of "how Minecraft
probably works" instead of read off the one place that is actually
authoritative -- the game's own blockstate/model JSON, shipped inside every
client jar. See docs/minecraft-block-authority.md for where those jars live
on this machine and what counts as ground truth.

This script does no guessing: it unzips the jar, prints the blockstate file
for the requested block verbatim, then prints every distinct model that
blockstate references, following each model's `parent` chain so inherited
`elements` are visible too. Nothing here talks to the network -- the jar is
read from local disk with `zipfile`.

Usage:
    python tools/mc_block_info.py repeater
    python tools/mc_block_info.py minecraft:comparator
    python tools/mc_block_info.py redstone_wall_torch --jar "C:\\path\\to\\1.20.1.jar"
    python tools/mc_block_info.py lever --models-only   # skip the blockstate dump

By default it looks for a jar under the standard Windows launcher directory
(`%APPDATA%/.minecraft/versions/<version>/<version>.jar`) and uses the
first one found, preferring the highest-looking version string; pass --jar
to point at a specific jar (a client jar -- not the dedicated server jar
under minecraft-server/, which does not ship client assets) or --version to
pick one by directory name under that same versions/ folder.
"""

import argparse
import json
import os
import sys
import zipfile


def default_versions_dir() -> str:
    appdata = os.environ.get("APPDATA") or os.path.expanduser("~/AppData/Roaming")
    return os.path.join(appdata, ".minecraft", "versions")


def find_jar(versions_dir: str, version: str | None) -> str:
    if not os.path.isdir(versions_dir):
        raise SystemExit(f"no versions directory at {versions_dir!r} -- pass --jar explicitly")
    candidates = []
    for entry in sorted(os.listdir(versions_dir)):
        if version is not None and entry != version:
            continue
        jar_path = os.path.join(versions_dir, entry, f"{entry}.jar")
        if os.path.isfile(jar_path):
            candidates.append(jar_path)
    if not candidates:
        raise SystemExit(
            f"no client jar found under {versions_dir!r}"
            + (f" for version {version!r}" if version else "")
            + " -- pass --jar explicitly"
        )
    # Prefer a plain release-looking name (e.g. "1.20.1") over snapshots --
    # sorting puts release numbers after snapshot codes ("2Xw..") lexically
    # often enough, so just prefer the longest run of dot-separated digits.
    def score(path: str) -> tuple:
        name = os.path.splitext(os.path.basename(path))[0]
        parts = name.split(".")
        all_numeric = all(p.isdigit() for p in parts)
        return (all_numeric, name)

    candidates.sort(key=score, reverse=True)
    return candidates[0]


def strip_ns(name: str) -> str:
    return name.split(":", 1)[1] if ":" in name else name


def read_json(z: zipfile.ZipFile, path: str) -> dict:
    with z.open(path) as f:
        return json.load(f)


def blockstate_path(block: str) -> str:
    return f"assets/minecraft/blockstates/{strip_ns(block)}.json"


def model_path(model: str) -> str:
    return f"assets/minecraft/models/{strip_ns(model)}.json"


def collect_model_names(blockstate: dict) -> list[str]:
    """Every distinct `model` field referenced by a blockstate, whether it
    uses the `variants` shape or the `multipart` shape -- covers every block
    kind this repo currently cares about (repeater/comparator use variants,
    redstone_wire uses multipart)."""
    names: list[str] = []

    def note(entry):
        # A variant's value is either one apply-object or a list of them
        # (Minecraft picks one at random for visual variety); collect both.
        entries = entry if isinstance(entry, list) else [entry]
        for e in entries:
            model = e.get("model")
            if model and model not in names:
                names.append(model)

    if "variants" in blockstate:
        for entry in blockstate["variants"].values():
            note(entry)
    if "multipart" in blockstate:
        for case in blockstate["multipart"]:
            note(case["apply"])
    return names


def print_model_chain(z: zipfile.ZipFile, model_name: str, seen: set) -> None:
    name = model_name
    while name and name not in seen:
        seen.add(name)
        path = model_path(name)
        try:
            data = read_json(z, path)
        except KeyError:
            print(f"  ({path} not in jar -- built-in model, e.g. an item's "
                  f"generated/handheld base; nothing more to read)")
            return
        print(f"--- {path} ---")
        print(json.dumps(data, indent=2))
        name = data.get("parent")
        if name:
            print(f"  (parent: {name})")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("block", help="block name, with or without the 'minecraft:' prefix")
    parser.add_argument("--jar", help="path to a client jar; skips auto-discovery")
    parser.add_argument("--version", help="version directory name to use under versions/, e.g. 1.20.1")
    parser.add_argument("--versions-dir", default=default_versions_dir(), help="override the versions/ directory")
    parser.add_argument("--models-only", action="store_true", help="skip printing the blockstate itself")
    args = parser.parse_args()

    jar_path = args.jar or find_jar(args.versions_dir, args.version)
    print(f"# reading {jar_path}", file=sys.stderr)

    with zipfile.ZipFile(jar_path) as z:
        bpath = blockstate_path(args.block)
        try:
            blockstate = read_json(z, bpath)
        except KeyError:
            raise SystemExit(f"{bpath} not found in {jar_path} -- check the block name")

        if not args.models_only:
            print(f"=== {bpath} ===")
            print(json.dumps(blockstate, indent=2))
            print()

        seen: set[str] = set()
        for model_name in collect_model_names(blockstate):
            print_model_chain(z, model_name, seen)
            print()


if __name__ == "__main__":
    main()

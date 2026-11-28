# SPDX-License-Identifier: GPL-3.0-or-later
"""Build the local shader pack consumed by Vortex's native Vulkan device."""

import argparse
import hashlib
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from shader_build import shaders


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--game-dir", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    parser.add_argument("--wave64-emulate", action="store_true")
    args = parser.parse_args()
    if args.jobs < 1 or args.jobs > 256:
        parser.error("--jobs must be between 1 and 256")
    game = args.game_dir.resolve()
    work = args.work.resolve()
    for directory in (game, work):
        if any((parent / ".vortex-root").exists() for parent in [directory, *directory.parents]):
            parser.error("keep game and shader work directories outside the repository")
    if work == game or game in work.parents or work in game.parents:
        parser.error("choose separate shader work and game directories")
    with (game / "shaders.rpf").open("rb") as archive:
        fingerprint = hashlib.file_digest(archive, "sha256").hexdigest()
    work.mkdir(parents=True, exist_ok=True)
    stamp = work / "vortex-shaders.sha256"
    if stamp.exists():
        if stamp.read_text(encoding="ascii").strip() != fingerprint:
            parser.error("this work directory belongs to a different shader archive")
    else:
        if (work / "shaders").exists():
            parser.error("choose a fresh work directory for this package")
        stamp.write_text(fingerprint + "\n", encoding="ascii")
    shaders(str(game), str(work), args.jobs, emulate=args.wave64_emulate)
    print(work / "shaders" / "spirv", flush=True)


if __name__ == "__main__":
    main()

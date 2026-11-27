# SPDX-License-Identifier: GPL-3.0-or-later
"""Offline RDNA2 Commons adapter. No modules produced here are runtime-ready."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys
from datetime import datetime, timezone


def run(args, cwd=None):
    print(">", subprocess.list2cmdline([str(a) for a in args]), flush=True)
    subprocess.run([str(a) for a in args], cwd=cwd, check=True)


def digest(path):
    with path.open("rb") as source:
        h = hashlib.sha256()
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            h.update(chunk)
        return h.hexdigest()


def inspect_spirv(path):
    data = path.read_bytes()
    if len(data) < 20 or len(data) % 4:
        raise ValueError("invalid SPIR-V size")
    words = struct.unpack("<%dI" % (len(data) // 4), data)
    if words[0] != 0x07230203:
        raise ValueError("invalid SPIR-V magic")
    stages, caps = set(), set()
    i = 5
    while i < len(words):
        count, op = words[i] >> 16, words[i] & 65535
        if count == 0 or i + count > len(words):
            raise ValueError("invalid instruction length")
        if op == 15 and count >= 4:
            stages.add(words[i + 1])
        if op == 17 and count == 2:
            caps.add(words[i + 1])
        i += count
    return {"version": hex(words[1]), "execution_models": sorted(stages),
            "capabilities": sorted(caps)}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--commons", type=Path, required=True,
                   help="local RDNA2 Commons checkout (external GPL tool)")
    p.add_argument("--archive", type=Path, required=True,
                   help="local shader archive from your own game files")
    p.add_argument("--out", type=Path, required=True,
                   help="parent directory for a fresh offline diagnostic run")
    p.add_argument("--jobs", type=int, default=6)
    p.add_argument("--disassembler", choices=("llvm-mc", "python"), default="llvm-mc")
    p.add_argument("--wave64-emulate", action="store_true")
    p.add_argument("--target-env", default="vulkan1.3")
    a = p.parse_args()
    if a.jobs < 1:
        p.error("--jobs must be positive")
    commons, archive = a.commons.resolve(), a.archive.resolve()
    if not archive.is_file():
        p.error("archive does not exist")
    tools = commons / "engine" / "tools"
    required = ("shader_carve.py", "fssm_unpack.py", "shader_disasm.py", "rdna2spirv.py")
    if not all((tools / name).is_file() for name in required):
        p.error("RDNA2 Commons checkout lacks required tools")
    validator = shutil.which("spirv-val")
    if not validator:
        p.error("spirv-val required; install Vulkan SDK and update PATH")
    revision = subprocess.check_output(["git", "-C", str(commons), "rev-parse", "HEAD"],
                                       text=True).strip()
    dirty = bool(subprocess.check_output(["git", "-C", str(commons), "status",
                                         "--porcelain", "--untracked-files=no"], text=True).strip())
    root = a.out.resolve() / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S-%fZ")
    root.mkdir(parents=True, exist_ok=False)
    carved = root / "shaders"
    disasm, spv = carved / "disasm", carved / "spirv"
    carved.mkdir()
    disasm.mkdir()
    spv.mkdir()
    def tool(name, *args):
        run([sys.executable, "-I", tools / name, *args], cwd=commons / "engine")
    manifest = {"schema": 1, "producer": "rdna2-commons", "revision": revision,
                "tracked_checkout_dirty": dirty, "archive_sha256": digest(archive),
                "target_env": a.target_env, "wave64_emulate": a.wave64_emulate,
                "disassembler": a.disassembler, "guest_pipelines_ready": 0,
                "abi_verified": False, "status": "running", "modules": []}
    def save():
        target = root / "manifest.json"
        tmp = target.with_suffix(".tmp")
        tmp.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        tmp.replace(target)
    save()
    try:
        tool("shader_carve.py", archive, carved)
        tool("fssm_unpack.py", carved)
        tool("shader_disasm.py", carved / "fssm", "--out", disasm,
             "--jobs", a.jobs, "--backend", a.disassembler)
        flags = ["--wave64-emulate"] if a.wave64_emulate else []
        tool("rdna2spirv.py", disasm, "--out", spv, *flags)
        for module in sorted(spv.rglob("*.spv")):
            item = {"path": module.relative_to(root).as_posix(),
                    "sha256": digest(module), "validator_passed": False,
                    "runtime_ready": False}
            try:
                item.update(inspect_spirv(module))
                val = subprocess.run([validator, "--target-env", a.target_env, str(module)],
                                     capture_output=True, text=True, errors="replace")
                item["validator_passed"] = val.returncode == 0
                if val.returncode:
                    item["validation_error"] = (val.stdout + val.stderr)[:4000]
            except ValueError as error:
                item["validation_error"] = str(error)
            manifest["modules"].append(item)
        manifest["candidates"] = len(manifest["modules"])
        manifest["validator_passed"] = sum(x["validator_passed"] for x in manifest["modules"])
        manifest["status"] = ("validated_offline" if manifest["candidates"] and
                              manifest["validator_passed"] == manifest["candidates"]
                              else "incomplete_or_invalid")
    except (OSError, subprocess.CalledProcessError) as error:
        manifest["status"] = "failed"
        manifest["error"] = str(error)
        raise
    finally:
        save()
        print("Offline manifest:", root / "manifest.json", flush=True)
    return 0 if manifest["status"] == "validated_offline" else 1


if __name__ == "__main__":
    sys.exit(main())

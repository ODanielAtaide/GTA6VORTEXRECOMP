# SPDX-License-Identifier: GPL-3.0-or-later
"""Vortex's offline RDNA2 package preparation.

Shader build stages adapted from ProjectLeonida/mandy make_game.py at
39f52f39c435e04273bc822f258f2c4bb1b7fec0. See docs/UPSTREAM_CREDITS.md.
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time

TOOLS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "tools")
sys.path.insert(0, TOOLS)
from mc_command import mc_command, split_command
from shader_stage_manifest import shader_stems


def log(message: str) -> None:
    print(f"{time.strftime('%H:%M:%S')}  {message}", flush=True)


def fail(message: str) -> "None":
    raise SystemExit(f"ERROR: {message}")


def run(args: list, hint: str = "", check: bool = True, **kwargs) -> int:
    command = [str(arg) for arg in args]
    log("  " + " ".join(command))
    code = subprocess.run(command, **kwargs).returncode
    if code and check:
        fail(f"{os.path.basename(command[0])} exited with {code}" + (f". {hint}" if hint else ""))
    return code


def fmt_duration(seconds: float) -> str:
    """Short wall-clock remaining/elapsed text for progress lines."""
    seconds = max(0, int(round(seconds)))
    if seconds < 60:
        return f"{seconds}s"
    minutes, sec = divmod(seconds, 60)
    if minutes < 60:
        return f"{minutes}m" if sec == 0 else f"{minutes}m {sec:02d}s"
    hours, minutes = divmod(minutes, 60)
    return f"{hours}h {minutes:02d}m"


class Progress:
    """Thread-safe counter that logs percent and ETA. total 0 logs once and stays quiet."""

    def __init__(self, label: str, total: int, *, every_s: float = 5.0):
        self.label = label
        self.total = max(0, int(total))
        self.done = 0
        self.every_s = every_s
        self._t0 = time.monotonic()
        self._lock = threading.Lock()
        self._last_log = 0.0
        if self.total == 0:
            log(f"{label}: nothing to do")
        else:
            self._emit(force=True)

    def tick(self, n: int = 1, *, force: bool = False) -> None:
        with self._lock:
            self.done = min(self.total, self.done + max(0, n))
            self._emit(force=force or self.done >= self.total)

    def set(self, done: int, *, force: bool = False) -> None:
        with self._lock:
            self.done = min(self.total, max(0, int(done)))
            self._emit(force=force or self.done >= self.total)

    def _emit(self, *, force: bool) -> None:
        if self.total <= 0:
            return
        now = time.monotonic()
        if not force and (now - self._last_log) < self.every_s and self.done < self.total:
            return
        self._last_log = now
        pct = 100 * self.done // self.total
        elapsed = now - self._t0
        if 0 < self.done < self.total:
            left = elapsed * (self.total - self.done) / self.done
            eta = f", ~{fmt_duration(left)} left"
        elif self.done >= self.total and elapsed >= 1:
            eta = f", {fmt_duration(elapsed)} elapsed"
        else:
            eta = ""
        log(f"{self.label}: {self.done}/{self.total} ({pct}%){eta}")


def py(tool: str, *args) -> None:
    run([sys.executable, "-I", os.path.join(TOOLS, tool), *args])


def tool_available(cmd: str) -> bool:
    """A PATH name, a path, or a whole command line such as 'wsl -e /usr/lib/llvm-21/bin/llvm-mc'."""
    try:
        parts = split_command(cmd)
    except SystemExit:
        return False
    head = parts[0] if parts else ""
    return bool(head and (shutil.which(head) or os.path.isfile(head)))


def need(exe: str, hint: str) -> str:
    if not tool_available(exe):
        fail(f"'{exe}' was not found on PATH. {hint}")
    return exe


def read_shader_setting(path: str, default: str = "") -> str:
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read().strip()
    except OSError:
        return default


def requested_shader_mode(had: str, emulate: bool, keep_emulated: bool) -> str:
    return "on" if emulate or (had == "on" and keep_emulated) else "off"


def translator_flags_setting(fallbacks: tuple[str, ...]) -> str:
    """The recorded form of the translator fallback flags in spirv/translator_flags.txt (empty means none)."""
    return " ".join(fallbacks)


def llvm_mc_signature() -> str:
    command = mc_command()
    command[0] = shutil.which(command[0]) or command[0]
    try:
        result = subprocess.run([*command, "--version"], capture_output=True, text=True, errors="replace", timeout=15)
    except (OSError, subprocess.TimeoutExpired) as error:
        fail(f"Could not read the llvm-mc version. Check LLVM_MC: {error}")
    version = result.stdout.strip()
    if result.returncode != 0 or not version:
        fallback = f"exit code {result.returncode}" if result.returncode else "no version printed"
        detail = (result.stderr.strip().splitlines() or [fallback])[0]
        fail(f"Could not read the llvm-mc version. Check LLVM_MC: {detail}")
    return json.dumps([command, version])


def unpacked_shader_stems(dxbc: str) -> set[str] | None:
    """Return current unpacked names, allowing the unpacker to skip non-FSSM containers."""
    try:
        carved = shader_stems(os.path.join(dxbc, "index.json"), "file", ".dxbc")
        unpacked = shader_stems(os.path.join(dxbc, "fssm_index.json"), "name")
    except (OSError, ValueError):
        return None
    return unpacked if unpacked and unpacked <= carved else None


def disassembly_listings(dxbc: str) -> set[str] | None:
    try:
        with os.scandir(os.path.join(dxbc, "disasm")) as entries:
            listings = set()
            for entry in entries:
                if not entry.name.endswith(".s"):
                    continue
                if entry.is_symlink():
                    return None
                if entry.is_file() and entry.stat().st_size > 0:
                    listings.add(entry.name[:-2])
            return listings
    except OSError:
        return None


def disassembly_status(dxbc: str, signature: str) -> tuple[bool, bool, set[str]]:
    """Report readiness, whether existing listings can be reused, and any repair inventory."""
    done = read_shader_setting(os.path.join(dxbc, "disasm.done"))
    marker = done or read_shader_setting(os.path.join(dxbc, "disasm.started"))
    inactive = (False, False, set())
    if not marker:
        return inactive
    active = unpacked_shader_stems(dxbc)
    listings = disassembly_listings(dxbc)
    if active is None or listings is None or not listings <= active:
        return inactive
    if marker == signature:
        # Older completion markers recorded only LLVM. Require a listing for every input.
        return (listings == active, False, set()) if done else (False, True, set())
    try:
        metadata = json.loads(marker)
    except ValueError:
        return inactive
    if not isinstance(metadata, dict) or metadata.get("llvm") != signature:
        return inactive
    recorded = metadata.get("listings")
    if not isinstance(recorded, list) or not recorded or not all(isinstance(name, str) for name in recorded):
        return inactive
    expected = set(recorded)
    if not listings <= expected <= active:
        return inactive
    return bool(done) and listings == expected, True, expected


def prepare_shader_disassembly(work: str, disasm: str, resume: bool) -> None:
    """Check the output path and remove old listings before starting a new run."""
    expected = os.path.join(os.path.realpath(work), "shaders", "dxbc", "disasm")
    if os.path.normcase(os.path.realpath(disasm)) != os.path.normcase(expected):
        fail(f"The shader disassembly folder must be inside the work folder: {disasm}")
    if not resume and os.path.isdir(disasm):
        with os.scandir(disasm) as entries:
            for entry in entries:
                if entry.name.endswith(".s") and (entry.is_file(follow_symlinks=False) or entry.is_symlink()):
                    os.unlink(entry.path)


def translated_index_is_empty(index: str) -> bool:
    """Whether a saved translated index is the `{}` that rdna2spirv.py --merge publishes when it translated
    nothing, so the next run must not keep it as done. A missing, unreadable or non-mapping file is a different
    problem and is not this: only a whole-file empty mapping came from a merge that ran and produced nothing."""
    try:
        with open(index, encoding="utf-8") as f:
            return json.load(f) == {}
    except (OSError, ValueError):
        return False


def shaders(game: str, work: str, jobs: int, emulate: bool = False, keep_emulated: bool = False,
            fallbacks: tuple[str, ...] = ()) -> None:
    src = os.path.join(game, "shaders.rpf")
    sh = os.path.join(work, "shaders")
    dxbc = os.path.join(sh, "dxbc")
    spirv = os.path.join(sh, "spirv")
    # Each stage counts as finished only when the file it writes last is there, never because its folder exists
    # or holds many files: a run stopped part-way leaves both. Re-running a stage drops the later stages' proof.
    carved = os.path.join(dxbc, "index.json")             # shader_carve.py, renamed into place at the end
    unpacked = os.path.join(dxbc, "fssm_index.json")      # fssm_unpack.py, the same
    disassembly_started = os.path.join(dxbc, "disasm.started")
    disassembled = os.path.join(dxbc, "disasm.done")      # records LLVM and the listings produced
    translated = os.path.join(spirv, "index.json")        # rdna2spirv.py --merge, renamed into place
    shard_plan = os.path.join(spirv, SHARD_PLAN)          # lets translate() keep finished shards; see there
    signature = llvm_mc_signature()
    if not os.path.isfile(carved):
        log("shaders: carving shaders.rpf")
        drop(translated, shard_plan, disassembled, disassembly_started, unpacked)
        py("shader_carve.py", src, dxbc)
    if unpacked_shader_stems(dxbc) is None:
        log("shaders: unpacking native code")
        drop(translated, shard_plan, disassembled, disassembly_started)
        py("fssm_unpack.py", dxbc, "--carve-index", carved)
    # rdna2spirv.py finds index.json, the .dxbc files and fssm/ in the parent of the .s files' folder, so the
    # disassembly must sit inside the carve folder: next to it, every stage reads as "?" and is rejected.
    disasm = os.path.join(dxbc, "disasm")
    complete, resume, expected = disassembly_status(dxbc, signature)
    if not complete:
        if os.path.isfile(disassembled) or os.path.isfile(translated):
            log("shaders: llvm-mc changed or the saved disassembly could not be verified")
        log("shaders: resuming disassembly (gfx1013)" if resume else "shaders: disassembling (gfx1013)")
        drop(translated, shard_plan)
        if not resume:
            drop(disassembly_started, disassembled)
        prepare_shader_disassembly(work, disasm, resume)
        if not os.path.isfile(disassembly_started) or os.path.isfile(disassembled):
            marker = disassembly_started + ".tmp"
            with open(marker, "w", encoding="utf-8", newline="\n") as f:
                start = json.dumps({"llvm": signature, "listings": sorted(expected)}) if expected else signature
                f.write(start + "\n")
            os.replace(marker, disassembly_started)
        drop(disassembled)
        resume_args = ("--skip-existing",) if resume else ()
        py("shader_disasm.py", os.path.join(dxbc, "fssm"), "--unpack-index", unpacked,
           "--out", disasm, "--mcpu", "gfx1013", "--jobs", jobs, *resume_args)
        listings = disassembly_listings(dxbc)
        if not listings or not expected <= listings:
            fail("Shader disassembly did not restore all expected listings")
        marker = disassembled + ".tmp"
        with open(marker, "w", encoding="utf-8", newline="\n") as f:
            json.dump({"llvm": signature, "listings": sorted(listings)}, f)
            f.write("\n")
        os.replace(marker, disassembled)
        drop(disassembly_started)
    # The translation also depends on the translator's own files, so their digest is kept beside it. A folder without
    # it (from before the file existed), with an unreadable one or with another one is translated again; carving, unpacking and
    # disassembly stay current.
    sources_file = os.path.join(spirv, "translation_sources.sha256")
    sources = translation_sources_digest()
    if os.path.isfile(translated) and read_shader_setting(sources_file) != sources:
        log("shaders: no matching translator fingerprint for the existing shaders: translating again")
        drop(translated)
    # The translation depends on --wave64-emulate, so the mode it was made with is kept beside it. A folder from
    # before the file existed was translated without emulation.
    mode_file = os.path.join(spirv, "wave64_emulate.txt")
    had = read_shader_setting(mode_file, default="off")
    want = requested_shader_mode(had, emulate, keep_emulated)
    if want == "on" and not emulate:
        # auto answers differently from run to run (a GPU asleep or hidden, vulkaninfo missing); emulated shaders are
        # exact on every GPU, so they are kept rather than translating the whole corpus again. When they are
        # translated again for another reason, or a stopped translation is retried, they stay emulated.
        log("shaders: the shaders were translated with wave64 emulation and it stays on (exact on every GPU); "
            "--wave64-emulate off translates them natively")
        emulate = True
    elif had != want and os.path.isfile(translated):
        log(f"shaders: wave64 emulation is now {want} (the shaders were translated with it {had}): "
            "translating again")
        drop(translated)
    # The translation also depends on the fallback flags (--no-float-controls, --bary-fallback,
    # --texel-offset-fallback, --mesh-compute), so they are kept beside it like the wave64 mode. A folder from
    # before the file existed was translated without them.
    flags_file = os.path.join(spirv, "translator_flags.txt")
    want_flags = translator_flags_setting(fallbacks)
    had_flags = read_shader_setting(flags_file)
    if had_flags != want_flags and os.path.isfile(translated):
        log(f"shaders: the fallback flags are now {want_flags or 'none'} (the shaders were translated with "
            f"{had_flags or 'none'}): translating again")
        drop(translated)
    if os.path.isfile(translated) and translated_index_is_empty(translated):
        # An empty merge, or an index that cannot be read, is not a finished translation. The shard plan goes with it:
        # its parts are what the empty merge read, so a retry has to translate them again rather than reuse them.
        log("shaders: the saved translated index names no shaders: translating again")
        drop(translated, shard_plan)
    if not os.path.isfile(translated):
        need("spirv-as", "Install the Vulkan SDK (it includes spirv-as and spirv-val) and reopen the terminal.")
        need("spirv-val", "Install the Vulkan SDK and reopen the terminal.")
        if "--compact-spirv" in fallbacks:
            need("spirv-opt", "Install SPIRV-Tools with spirv-opt to use --compact-spirv.")
        log(f"shaders: translating RDNA2 to SPIR-V on {jobs} processes (the longest step)")
        # The mode and the translator's digest go down before the translation starts, while there is no index.json:
        # a stop at any point then leaves either no index (translated again) or an index with the right mode and
        # digest, never a guessed one. rdna2spirv.py --merge writes index.json last, after the stubs.
        os.makedirs(spirv, exist_ok=True)
        with open(mode_file, "w", encoding="utf-8", newline="\n") as f:
            f.write(want + "\n")
        with open(flags_file, "w", encoding="utf-8", newline="\n") as f:
            f.write(want_flags + "\n")
        with open(sources_file, "w", encoding="utf-8", newline="\n") as f:
            f.write(sources + "\n")
        translate(disasm, spirv, jobs, emulate=emulate, fallbacks=fallbacks)
    verify_translation_completeness(dxbc, spirv)
    native_pso_hashes(dxbc, spirv)
    fspx_shaders(game, work, signature, emulate=emulate, fallbacks=fallbacks)
    bvh_shaders(game, work, signature, jobs, emulate=emulate, fallbacks=fallbacks)
    library_shaders(game, work, signature, jobs, emulate=emulate, fallbacks=fallbacks)
    exe_shaders(game, work, signature, jobs, emulate=emulate, fallbacks=fallbacks)
    log("shaders: done")


def native_pso_hashes(dxbc: str, spirv: str) -> None:
    """Index the carved containers named by native graphics PSOs, including on a resumed translation."""
    import native_pso_hashes as tool

    index = os.path.join(spirv, "index.json")
    if not os.path.isfile(index) or not os.path.isdir(dxbc):
        return
    output = os.path.join(spirv, tool.OUT_NAME)
    if os.path.isfile(output):
        stamp = os.stat(output).st_mtime_ns
        inputs = (index, dxbc, os.path.join(dxbc, "index.json"), tool.__file__,
                  os.path.join(os.path.dirname(tool.__file__), "shader_carve.py"))
        if all(not os.path.exists(p) or os.stat(p).st_mtime_ns <= stamp for p in inputs):
            with os.scandir(dxbc) as files:
                if all(not f.name.endswith(".dxbc") or f.stat().st_mtime_ns <= stamp for f in files):
                    return
    try:
        table = tool.build(dxbc, spirv)
        tool.write(spirv, table)
    except (OSError, ValueError) as error:
        fail(f"shaders: could not index native graphics pipelines: {error}")
    log(f"shaders: indexed {len(table)} native shader digests")


FSPX_DLL = "amd_fidelityfx_upscaler_xbox.dll"


FSPX_TOOLS = ("fspx_unpack.py", "shader_disasm.py", "rdna2_decode.py", "rdna2_isa_tables.py", "mc_command.py")


def file_sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fspx_settings(dll: str, mc_signature: str, emulate: bool, fallbacks: tuple[str, ...]) -> str:
    """What the FSPX outputs depend on, as one line: the DLL, the unpacking and translating tools, llvm-mc, the
    wave64 mode and the fallback flags."""
    tools = hashlib.sha256()
    for name in FSPX_TOOLS:
        tools.update(f"{name}\0".encode() + bytes.fromhex(file_sha256(os.path.join(TOOLS, name))))
    return json.dumps({"dll": file_sha256(dll), "tools": tools.hexdigest(), "translator": translation_sources_digest(),
                       "llvm_mc": mc_signature, "wave64_emulate": bool(emulate), "fallbacks": list(fallbacks)},
                      sort_keys=True)


def fspx_cache_complete(spirv: str) -> bool:
    """Require a runtime-addressable compute program and all of the modules in the saved translation index."""
    try:
        index = json.loads(read_shader_setting(os.path.join(spirv, "index.json")))
    except ValueError:
        return False
    if not isinstance(index, dict):
        return False
    # The translator can write a module with an error-only metadata row. Such a module has no runtime lookup key.
    has_compute_key = any(
        isinstance(meta, dict) and meta.get("stage") == "cs"
        and isinstance(meta.get("fspx_code_sha256"), str) and re.fullmatch(r"[0-9a-f]{64}", meta["fspx_code_sha256"])
        for meta in index.values()
    )
    return has_compute_key and all(os.path.isfile(os.path.join(spirv, name + ".spv")) for name in index)


def fspx_shaders(game: str, work: str, mc_signature: str, emulate: bool = False,
                 fallbacks: tuple[str, ...] = ()) -> None:
    """The FidelityFX DLL's compute programs (docs/fspx.md): tools/fspx_unpack.py unpacks and disassembles them into
    <work>/shaders/fspx, rdna2spirv.py translates them into <work>/shaders/spirv/fspx, where the runtime's shader
    library finds them by fspx_code_sha256. The game's own shaders neither feed nor depend on this step.

    The 29 programs take seconds, so the step runs whole. Reuse requires the translation index, its named modules
    and a matching fspx_build.json (fspx_settings)."""
    dll = os.path.join(game, FSPX_DLL)
    unpacked = os.path.join(work, "shaders", "fspx")
    spirv = os.path.join(work, "shaders", "spirv", "fspx")
    translated = os.path.join(spirv, "index.json")
    settings_file = os.path.join(spirv, "fspx_build.json")
    if not os.path.isfile(dll):
        log(f"shaders: no {FSPX_DLL} in the game folder, so no FSPX programs to translate")
        return
    settings = fspx_settings(dll, mc_signature, emulate, fallbacks)
    if read_shader_setting(settings_file) == settings and fspx_cache_complete(spirv):
        return
    log(f"shaders: unpacking and translating the FSPX programs of {FSPX_DLL}")
    drop(translated, settings_file)
    # A program that no longer translates must not keep a module from the run before.
    for folder, suffixes in ((os.path.join(unpacked, "disasm"), (".s",)), (spirv, (".spv", ".spvasm"))):
        if os.path.isdir(folder):
            with os.scandir(folder) as entries:
                for entry in entries:
                    if entry.name.endswith(suffixes) and entry.is_file(follow_symlinks=False):
                        os.unlink(entry.path)
    py("fspx_unpack.py", dll, "--out", unpacked, "--disasm")
    extra = (["--wave64-emulate"] if emulate else []) + list(fallbacks)
    py("rdna2spirv.py", os.path.join(unpacked, "disasm"), "--out", spirv, "--no-asm", *extra)
    verify_fspx_key_clash(spirv, translated, "FSPX")
    with open(settings_file, "w", encoding="utf-8", newline="\n") as f:
        f.write(settings + "\n")


LIBRARY_ARCHIVE = "shaders.rpf"


LIBRARY_TOOLS = ("shader_library.py", "shader_carve.py", "shader_carve_accel.py",
                 "fspx_unpack.py", "shader_disasm.py", "rdna2_decode.py", "rdna2_isa_tables.py", "mc_command.py")


def library_settings(archive: str, mc_signature: str, emulate: bool, fallbacks: tuple[str, ...]) -> str:
    """What the pipeline library's compute translations depend on, as one line: shaders.rpf, the unpacking and
    translating tools, llvm-mc, the wave64 mode and the fallback flags."""
    return json.dumps({"archive": file_sha256(archive), "tools": sources_digest(TOOLS, LIBRARY_TOOLS),
                       "translator": translation_sources_digest(), "llvm_mc": mc_signature,
                       "wave64_emulate": bool(emulate), "fallbacks": list(fallbacks)}, sort_keys=True)


def library_shaders(game: str, work: str, mc_signature: str, jobs: int, emulate: bool = False,
                    fallbacks: tuple[str, ...] = ()) -> None:
    """The compute pipelines of the pipeline library in shaders.rpf (docs/shader_library.md). The executable reads
    the library through DirectStorage and creates each compute pipeline with Xbox device slot 67
    (DeserializeComputePipelineStateX), passing the record's four parts as FSPX kinds 0, 1, 6 and 7 (docs/fspx.md).
    tools/shader_library.py unpacks the compute programs with their kind-0 state into <work>/shaders/library, and
    translate() runs rdna2spirv.py across jobs processes into <work>/shaders/spirv/library, where the runtime's
    shader library finds them by fspx_code_sha256 and fspx_state_sha256 as it finds the FidelityFX DLL's. Graphics
    pipelines are not translated here. The game's own shaders neither feed nor depend on this step, apart from the
    carve that names each program's source container (the translator reads its signatures).

    The step runs whole. Reuse requires the translation index, its named modules and a matching library_build.json
    (library_settings)."""
    archive = os.path.join(game, LIBRARY_ARCHIVE)
    unpacked = os.path.join(work, "shaders", "library")
    spirv = os.path.join(work, "shaders", "spirv", "library")
    translated = os.path.join(spirv, "index.json")
    shard_plan = os.path.join(spirv, SHARD_PLAN)
    settings_file = os.path.join(spirv, "library_build.json")
    if not os.path.isfile(archive):
        drop(translated, settings_file, shard_plan)
        log(f"shaders: no {LIBRARY_ARCHIVE} in the game folder, so no pipeline library to translate")
        return
    settings = library_settings(archive, mc_signature, emulate, fallbacks)
    if read_shader_setting(settings_file) == settings and fspx_cache_complete(spirv):
        return
    log(f"shaders: unpacking and translating the compute pipelines of the pipeline library in {LIBRARY_ARCHIVE}")
    drop(translated, settings_file, shard_plan)
    # A program that no longer translates or lost its carve container must not keep outputs from the run before.
    for folder, suffixes in ((unpacked, (".dxbc",)),
                             (os.path.join(unpacked, "fspx"), (".code", ".state", ".json")),
                             (os.path.join(unpacked, "disasm"), (".s",)),
                             (spirv, (".spv", ".spvasm"))):
        if os.path.isdir(folder):
            with os.scandir(folder) as entries:
                for entry in entries:
                    if entry.name.endswith(suffixes) and entry.is_file(follow_symlinks=False):
                        os.unlink(entry.path)
    carve = os.path.join(work, "shaders", "dxbc")
    py("shader_library.py", archive, "--out", unpacked, "--stages", "cs", "--disasm", "--jobs", str(jobs),
       *(["--carve", carve] if os.path.isdir(carve) else []))
    translate(os.path.join(unpacked, "disasm"), spirv, jobs, emulate=emulate, fallbacks=fallbacks)
    verify_fspx_key_clash(spirv, translated, "pipeline library")
    with open(settings_file, "w", encoding="utf-8", newline="\n") as f:
        f.write(settings + "\n")


def verify_fspx_key_clash(spirv: str, translated: str, label: str) -> None:
    """Reject an unreadable translation index or one whose kind-6 and kind-0 keys pick between different modules."""
    try:
        clash = fspx_key_clash(spirv)
    except (OSError, ValueError, TypeError) as error:
        drop(translated)
        fail(f"shaders: {label} translation wrote an unreadable index "
             f"({getattr(error, 'strerror', None) or error}); run the shaders stage again")
    if clash:
        drop(translated)   # the runtime must not load an index whose keys pick between different modules
        fail(f"shaders: {label} programs {clash[0]} and {clash[1]} have the same kind-6 and kind-0 bytes "
             "(fspx_code_sha256 and fspx_state_sha256) but different translated modules, so the runtime could not "
             "tell which one a pipeline means (docs/fspx.md, \"Build and run-time lookup\")")


def fspx_key_clash(spirv: str):
    """Two entries of <spirv>/index.json with the same fspx_code_sha256 and fspx_state_sha256 whose .spv files
    differ, as (name, name), or None. The same kind-6 bytes with different kind-0 entries is allowed: the run time
    picks by both keys when the bytecode carries kind 0, and refuses a bare kind-6 program whose modules differ."""
    with open(os.path.join(spirv, "index.json"), encoding="utf-8") as f:
        index = json.load(f)
    if not isinstance(index, dict):
        raise ValueError("index.json must contain a JSON object")
    first = {}
    for name in sorted(index):
        meta = index[name]
        if not isinstance(meta, dict) or "fspx_code_sha256" not in meta:
            continue
        key = (meta["fspx_code_sha256"], meta.get("fspx_state_sha256"))
        if key not in first:
            first[key] = name
            continue
        modules = []
        for n in (first[key], name):
            try:
                with open(os.path.join(spirv, n + ".spv"), "rb") as f:
                    modules.append(f.read())
            except OSError:
                modules.append(None)
        if modules[0] is None or modules[0] != modules[1]:
            return first[key], name
    return None


BVH_DLL = "XboxBvhBuilder.dll"


BVH_TOOLS = ("pe_shader_containers.py", "fssm_unpack.py", "shader_disasm.py",   # as FSPX_TOOLS, for the builder:
             "rdna2_decode.py", "rdna2_isa_tables.py", "mc_command.py")          # what shader_disasm.py decodes with


BVH_MAX_SHARED_MEMORY = 32768


def bvh_settings(dll: str, mc_signature: str, emulate: bool, fallbacks: tuple[str, ...]) -> str:
    """What the builder's outputs depend on, as one line: the DLL, the extracting, unpacking and translating tools,
    llvm-mc, the wave64 mode, the fallback flags and the workgroup-memory cap."""
    return json.dumps({"dll": file_sha256(dll), "tools": sources_digest(TOOLS, BVH_TOOLS),
                       "translator": translation_sources_digest(), "llvm_mc": mc_signature,
                       "wave64_emulate": bool(emulate), "fallbacks": list(fallbacks),
                       "max_shared_memory": BVH_MAX_SHARED_MEMORY}, sort_keys=True)


def bvh_programs(unpacked: str, spirv: str) -> "tuple[list[str], list[str]] | None":
    """The programs pe_shader_containers.py extracted, by file stem, and the ones among them the runtime cannot
    find: no entry with an fssm_sha256 in <spirv>/index.json, or no module beside it. None when either index is
    missing or unreadable, or names a module that is not there."""
    try:
        extracted = sorted(shader_stems(os.path.join(unpacked, "index.json"), "file", ".dxbc"))
        with open(os.path.join(spirv, "index.json"), encoding="utf-8") as f:
            index = json.load(f)
    except (OSError, ValueError):
        return None
    if not isinstance(index, dict) or not all(os.path.isfile(os.path.join(spirv, name + ".spv")) for name in index):
        return None
    found = {name for name, meta in index.items()
             if isinstance(meta, dict) and isinstance(meta.get("fssm_sha256"), str)}
    return extracted, [name for name in extracted if name not in found]


def bvh_report(programs: "tuple[list[str], list[str]]") -> None:
    """One line with the counts, and one that names every program left without a module. It is printed on every
    run, also when the step itself is reused: a program the translator has no rule for is never dropped quietly."""
    extracted, missing = programs
    log(f"shaders: {len(extracted) - len(missing)} of {len(extracted)} {BVH_DLL} programs translated")
    if missing:
        log(f"shaders: NOT translated: {', '.join(missing)}. The runtime has no module for "
            f"{'it' if len(missing) == 1 else 'them'}, so that compute pipeline is refused (E_INVALIDARG) and the "
            "builder's constructor, which creates all of them, gives up there. When the step runs, rdna2spirv.py "
            "prints the instruction each one stops at (docs/embedded_shaders.md, section 4)")


def bvh_shaders(game: str, work: str, mc_signature: str, jobs: int = 1, emulate: bool = False,
                fallbacks: tuple[str, ...] = ()) -> None:
    """XboxBvhBuilder.dll's compute programs (docs/embedded_shaders.md): pe_shader_containers.py extracts the DLL's
    DXBC containers into <work>/shaders/bvh, fssm_unpack.py and shader_disasm.py unpack and disassemble their FSSM
    programs there, as for a shader from shaders.rpf, and rdna2spirv.py translates them into
    <work>/shaders/spirv/bvh, where the runtime's shader library finds them by fssm_sha256, the key of the game's
    own shaders. Those neither feed nor depend on this step.

    The 52 programs take seconds, so the step runs whole. Reuse requires both indexes, the translated modules and a
    matching bvh_build.json (bvh_settings)."""
    dll = os.path.join(game, BVH_DLL)
    unpacked = os.path.join(work, "shaders", "bvh")
    spirv = os.path.join(work, "shaders", "spirv", "bvh")
    extracted = os.path.join(unpacked, "index.json")
    unpack_index = os.path.join(unpacked, "fssm_index.json")
    translated = os.path.join(spirv, "index.json")
    settings_file = os.path.join(spirv, "bvh_build.json")
    if not os.path.isfile(dll):
        # A work folder used before with a package that had the DLL still holds that run's index, and the runtime
        # loads <shaders>/bvh/index.json whenever it is there. Without the index the old modules are not found.
        drop(translated, settings_file)
        log(f"shaders: no {BVH_DLL} in the game folder, so no BVH builder programs to translate")
        return
    settings = bvh_settings(dll, mc_signature, emulate, fallbacks)
    programs = bvh_programs(unpacked, spirv) if read_shader_setting(settings_file) == settings else None
    if programs:
        bvh_report(programs)
        return
    log(f"shaders: extracting and translating the compute programs of {BVH_DLL}")
    drop(translated, settings_file, extracted, unpack_index)
    # pe_shader_containers.py never overwrites a container (a second run would write <module>_NN.dxbc beside the
    # first), and a program that no longer translates must not keep a module from the run before.
    for folder, suffixes in ((unpacked, (".dxbc",)), (os.path.join(unpacked, "fssm"), (".fssm",)),
                             (os.path.join(unpacked, "disasm"), (".s",)), (spirv, (".spv", ".spvasm"))):
        if os.path.isdir(folder):
            with os.scandir(folder) as entries:
                for entry in entries:
                    if entry.name.endswith(suffixes) and entry.is_file(follow_symlinks=False):
                        os.unlink(entry.path)
    py("pe_shader_containers.py", dll, "--extract", unpacked)
    py("fssm_unpack.py", unpacked, "--carve-index", extracted)
    # rdna2spirv.py reads each program's stage from index.json and its FSSM blob from fssm/, both in the parent of
    # the listings' folder, so the disassembly sits inside the extraction folder as it does for shaders.rpf.
    disasm = os.path.join(unpacked, "disasm")
    py("shader_disasm.py", os.path.join(unpacked, "fssm"), "--unpack-index", unpack_index, "--out", disasm,
       "--mcpu", "gfx1013", "--jobs", jobs)
    extra = (["--wave64-emulate"] if emulate else []) + list(fallbacks)
    py("rdna2spirv.py", disasm, "--out", spirv, "--no-asm", "--max-shared-memory", BVH_MAX_SHARED_MEMORY, *extra)
    programs = bvh_programs(unpacked, spirv)
    if not programs or len(programs[1]) == len(programs[0]):
        drop(translated)   # an empty or unreadable index would read as a finished step
        fail(f"shaders: none of the {BVH_DLL} programs translated, so their index is not kept. Run the shaders "
             "stage again; if it happens again, the tool output above is why.")
    bvh_report(programs)
    with open(settings_file, "w", encoding="utf-8", newline="\n") as f:
        f.write(settings + "\n")


EXE_IMAGE = "game_scarlett_master.exe"


EXE_TOOLS = ("pe_shader_containers.py", "fssm_unpack.py", "shader_disasm.py")   # as BVH_TOOLS, for the executable


def exe_settings(image: str, mc_signature: str, emulate: bool, fallbacks: tuple[str, ...]) -> str:
    """What the executable's shader outputs depend on, as one line: the image, the extracting, unpacking and
    translating tools, llvm-mc, the wave64 mode and the fallback flags."""
    return json.dumps({"image": file_sha256(image), "tools": sources_digest(TOOLS, EXE_TOOLS),
                       "translator": translation_sources_digest(), "llvm_mc": mc_signature,
                       "wave64_emulate": bool(emulate), "fallbacks": list(fallbacks)}, sort_keys=True)


def exe_programs(unpacked: str, spirv: str) -> "tuple[list[str], list[str]] | None":
    """The programs pe_shader_containers.py extracted from the executable, by file stem, and the ones among them
    the runtime cannot find: no entry with an fssm_sha256 in <spirv>/index.json, or no module beside it. None when
    either index is missing or unreadable, or names a module that is not there."""
    try:
        extracted = sorted(shader_stems(os.path.join(unpacked, "index.json"), "file", ".dxbc"))
        with open(os.path.join(spirv, "index.json"), encoding="utf-8") as f:
            index = json.load(f)
    except (OSError, ValueError):
        return None
    if not isinstance(index, dict) or not all(os.path.isfile(os.path.join(spirv, name + ".spv")) for name in index):
        return None
    found = {name for name, meta in index.items()
             if isinstance(meta, dict) and isinstance(meta.get("fssm_sha256"), str)}
    return extracted, [name for name in extracted if name not in found]


def exe_report(programs: "tuple[list[str], list[str]]") -> None:
    """One line with the counts, and one that names every program left without a module. It is printed on every
    run, also when the step itself is reused: a program the translator has no rule for is never dropped quietly."""
    extracted, missing = programs
    log(f"shaders: {len(extracted) - len(missing)} of {len(extracted)} {EXE_IMAGE} programs translated")
    if missing:
        log(f"shaders: NOT translated: {', '.join(missing)}. The runtime has no module for "
            f"{'it' if len(missing) == 1 else 'them'}, so every graphics pipeline that names "
            f"{'it' if len(missing) == 1 else 'them'} is refused (E_INVALIDARG). When the step runs, rdna2spirv.py "
            "prints the instruction each one stops at")


def exe_shaders(game: str, work: str, mc_signature: str, jobs: int = 1, emulate: bool = False,
                fallbacks: tuple[str, ...] = ()) -> None:
    """game_scarlett_master.exe's own DXBC containers (docs/embedded_shaders.md, "Retail executable"): the
    executable passes them inline to CreatePipelineState, so no shader archive holds them. pe_shader_containers.py
    extracts its plain and packed programs into <work>/shaders/exe, fssm_unpack.py and shader_disasm.py unpack and disassemble
    their FSSM programs there, and rdna2spirv.py translates them into <work>/shaders/spirv/exe, where the runtime's
    shader library finds them by fssm_sha256, the key of the game's own shaders. Those neither feed nor depend on
    this step.

    The programs take seconds, so the step runs whole. Reuse requires both indexes, the translated modules and
    a matching exe_build.json (exe_settings)."""
    image = os.path.join(game, EXE_IMAGE)
    unpacked = os.path.join(work, "shaders", "exe")
    spirv = os.path.join(work, "shaders", "spirv", "exe")
    extracted = os.path.join(unpacked, "index.json")
    unpack_index = os.path.join(unpacked, "fssm_index.json")
    translated = os.path.join(spirv, "index.json")
    settings_file = os.path.join(spirv, "exe_build.json")
    if not os.path.isfile(image):
        # A work folder used before with a package that had the image still holds that run's index, and the runtime
        # loads <shaders>/exe/index.json whenever it is there. Without the index the old modules are not found.
        drop(translated, settings_file)
        log(f"shaders: no {EXE_IMAGE} in the game folder, so no executable shader programs to translate")
        return
    settings = exe_settings(image, mc_signature, emulate, fallbacks)
    programs = exe_programs(unpacked, spirv) if read_shader_setting(settings_file) == settings else None
    if programs and len(programs[1]) < len(programs[0]):
        exe_report(programs)
        return
    log(f"shaders: extracting and translating the graphics programs of {EXE_IMAGE}")
    drop(translated, settings_file, extracted, unpack_index)
    # pe_shader_containers.py never overwrites a container (a second run would write <module>_NN.dxbc beside the
    # first), and a program that no longer translates must not keep a module from the run before.
    for folder, suffixes in ((unpacked, (".dxbc",)), (os.path.join(unpacked, "fssm"), (".fssm",)),
                             (os.path.join(unpacked, "disasm"), (".s",)), (spirv, (".spv", ".spvasm"))):
        if os.path.isdir(folder):
            with os.scandir(folder) as entries:
                for entry in entries:
                    if entry.name.endswith(suffixes) and entry.is_file(follow_symlinks=False):
                        os.unlink(entry.path)
    py("pe_shader_containers.py", image, "--extract", unpacked)
    py("fssm_unpack.py", unpacked, "--carve-index", extracted)
    # rdna2spirv.py reads each program's stage from index.json and its FSSM blob from fssm/, both in the parent of
    # the listings' folder, so the disassembly sits inside the extraction folder as it does for shaders.rpf.
    disasm = os.path.join(unpacked, "disasm")
    py("shader_disasm.py", os.path.join(unpacked, "fssm"), "--unpack-index", unpack_index, "--out", disasm,
       "--mcpu", "gfx1013", "--jobs", jobs)
    extra = (["--wave64-emulate"] if emulate else []) + list(fallbacks)
    py("rdna2spirv.py", disasm, "--out", spirv, "--no-asm", *extra)
    programs = exe_programs(unpacked, spirv)
    if not programs or len(programs[1]) == len(programs[0]):
        drop(translated)   # an empty or unreadable index would read as a finished step
        fail(f"shaders: none of {EXE_IMAGE}'s programs translated, so their index is not kept. Run the shaders "
             "stage again; if it happens again, the tool output above is why.")
    exe_report(programs)
    with open(settings_file, "w", encoding="utf-8", newline="\n") as f:
        f.write(settings + "\n")


TRANSLATION_SOURCES = ("rdna2spirv.py", "shader_ir.py", "cfg_structure.py", "file_mode.py", "fssm_header.py", "lane_masks.py", "wave64_halves.py",
                       "wave64_emulate_opt.py", "spirv_loop_guard.py", "shader_meta.py", "spirv_stubs.py", "spirv_asm.py", "spirv_asm_tables.py",
                       "spirv_opt.py", "report_paths.py", "replace_retry.py", "fssm_unpack.py", "shader_stage_manifest.py",
                       "rdna2_decode.py", "rdna2_isa_tables.py", "gfx10_formats.json",
                       "native_pso_hashes.py", "shader_carve.py", "shader_carve_accel.py")


def sources_digest(folder: str, names, lead: str = "") -> str:
    """SHA-256 over lead, then the name and exact bytes of each named file in folder."""
    h = hashlib.sha256(lead.encode())
    for name in names:
        with open(os.path.join(folder, name), "rb") as f:
            data = f.read()
        h.update(f"{name}\0{len(data)}\0".encode())
        h.update(data)
    return h.hexdigest()


def translation_sources_digest() -> str:
    """SHA-256 over the name and exact bytes of each TRANSLATION_SOURCES file."""
    return sources_digest(TOOLS, TRANSLATION_SOURCES)


def drop(*paths: str) -> None:
    """Remove the files that mark later stages finished, so they run again on this stage's new output."""
    for p in paths:
        if os.path.isfile(p):
            os.remove(p)


SHARD_PLAN = "shards.json"   # beside the index parts: the --jobs, mode and translator source that made them


def drop_index(index: str) -> str:
    """Remove a translated index the next run must not take as finished; "" once it is gone.

    A remove that fails returns a sentence for the stage's failure message instead of raising: on Windows the
    lock that stopped the index being read or merged (an antivirus scan, the search indexer) usually stops this
    too, and the PermissionError traceback would hide why the stage failed. The reason is the strerror, not
    str(error), which quotes the path a second time through repr() (every Windows backslash doubled)."""
    try:
        os.remove(index)
    except FileNotFoundError:
        pass
    except OSError as error:
        return f" {index} could not be removed ({error.strerror or error}): delete it before running the shaders stage again."
    return ""


def translator_digest() -> str:
    """SHA-256 over every file directly in tools/: rdna2spirv.py, the modules it imports and the data it reads
    (gfx10_formats.json). Any change there counts, so parts made by other code are never merged with new ones."""
    import hashlib
    h = hashlib.sha256()
    for name in sorted(os.listdir(TOOLS)):
        path = os.path.join(TOOLS, name)
        if os.path.isfile(path):
            with open(path, "rb") as f:
                h.update(name.encode("utf-8") + b"\0" + f.read() + b"\0")
    return h.hexdigest()


def _stop_procs(procs: list) -> None:
    """Terminate and reap any child processes still running after an interrupted or failed launch."""
    for p in procs:
        if p.poll() is None:
            try:
                p.terminate()
            except OSError:
                pass
    for p in procs:
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                p.kill()
            except OSError:
                pass
            try:
                p.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                pass
        except OSError:
            pass


def translate(disasm: str, spirv: str, jobs: int, emulate: bool = False,
              fallbacks: tuple[str, ...] = ()) -> None:
    """rdna2spirv.py runs on one core: start one --shard per job side by side, then merge their index parts.

    A shard publishes shard<i>.json and then index.part<i>.json. Both are needed to reuse it. A run stopped
    part-way, or during the merge, keeps those parts when it resumes with the same --jobs, wave64 mode, fallback
    flags and translator source, and starts only the shards that have none. shaders() drops the plan whenever an
    earlier step reruns."""
    os.makedirs(spirv, exist_ok=True)
    plan_path = os.path.join(spirv, SHARD_PLAN)
    plan = {"jobs": jobs, "wave64_emulate": bool(emulate), "fallbacks": list(fallbacks),
            "translator": translator_digest()}
    try:
        with open(plan_path, encoding="utf-8") as f:
            same = json.load(f) == plan
    except (OSError, ValueError):
        same = False
    if not same:
        # Parts from another --jobs would be merged in too, and parts from another mode, flags or translator are
        # wrong. They go before the new plan is written: a stop in between must not leave old parts under a new plan.
        for f in os.listdir(spirv):
            if (f.startswith("index.part") and f.endswith(".json")) or re.fullmatch(r"shard\d+\.json", f):
                os.remove(os.path.join(spirv, f))
        with open(plan_path, "w", encoding="utf-8", newline="\n") as f:
            json.dump(plan, f)
    todo = [
        i for i in range(jobs)
        if not (os.path.isfile(os.path.join(spirv, f"index.part{i}.json"))
                and os.path.isfile(os.path.join(spirv, f"shard{i}.json")))
    ]
    if len(todo) < jobs:
        log(f"  keeping {jobs - len(todo)} of {jobs} shards finished by the run before")
    tool = os.path.join(TOOLS, "rdna2spirv.py")
    extra = ["--wave64-emulate"] if emulate else []
    extra.extend(fallbacks)
    if todo:
        which = f"0..{jobs - 1}" if len(todo) == jobs else ",".join(str(i) for i in todo)
        log(f"  rdna2spirv.py {disasm} --out {spirv} --shard {which}/{jobs}" + "".join(" " + e for e in extra))
    # --no-asm: keep only the .spv modules; the .spvasm text would add ~35 GB for the corpus
    procs: dict[int, object] = {}
    bad = []
    try:
        for i in todo:
            procs[i] = subprocess.Popen(
                [sys.executable, "-I", tool, disasm, "--out", spirv, "--shard", f"{i}/{jobs}",
                 "--no-asm", *extra])
        progress = Progress("shaders: translate", jobs, every_s=10.0)
        progress.set(jobs - len(todo), force=True)
        while procs:
            for i, p in list(procs.items()):
                code = p.poll()
                if code is None:
                    continue
                del procs[i]
                if code != 0:
                    bad.append(f"shard {i} exited with {code}")
                progress.tick(force=True)
            if procs:
                time.sleep(1.0)
    except BaseException:
        _stop_procs(list(procs.values()))
        raise
    if bad:
        # An older index describes the corpus from the run before: keeping it would make the next run take the
        # failed translation as finished. The next successful --merge writes it again.
        fail("rdna2spirv.py: " + ", ".join(bad) + "." + drop_index(os.path.join(spirv, "index.json")))
    py("rdna2spirv.py", disasm, "--out", spirv, "--merge")
    index = os.path.join(spirv, "index.json")
    if os.path.isfile(index):
        # Present but empty or unreadable: the merge published nothing, and the next run would read that as finished.
        try:
            with open(index, encoding="utf-8") as f:
                merged = json.load(f)
        except (OSError, ValueError) as e:
            fail(f"rdna2spirv.py --merge wrote an unreadable {index}: {getattr(e, 'strerror', None) or e}."
                 + drop_index(index))
        if not merged:
            # A marker left behind would look like a finished translation to the next run; drop it with the failure.
            left = drop_index(index)
            # The plan would make the retry reuse the same empty parts even after the disassembly was repaired.
            try:
                os.remove(plan_path)
            except OSError:
                pass
            fail("rdna2spirv.py translated 0 shaders, so the empty index is not kept. Run the shaders stage again; "
                 "if it happens again, the disassembly logged above is why." + left)


def verify_translation_completeness(dxbc: str, spirv: str) -> None:
    """Every carved shader must be in the merged translation index before the stage may call itself done.

    rdna2spirv.py drops a shader that is refused or whose module fails spirv-val (the module never reaches the
    final path) and still exits 0, and nothing later in the build compares the counts: the merge already refuses
    an empty index, and a translation that did not run fails the stage before this point, so those cases are left
    to the paths that own them. What this adds is the silent one, a readable index naming fewer shaders than the
    carve: a port assembled from it would lack the difference."""
    try:
        with open(os.path.join(dxbc, "index.json"), encoding="utf-8") as f:
            carved = json.load(f)
        with open(os.path.join(spirv, "index.json"), encoding="utf-8") as f:
            index = json.load(f)
    except (OSError, ValueError):
        return
    if not isinstance(carved, list) or not isinstance(index, dict):
        fail("shaders: the carve index is not a list or the translation index is not an object; "
             "run the shaders stage again")
    if not index:
        return   # an empty index is #3328's to refuse or translate again, not a partial one to compare
    if len(index) != len(carved):
        fail(f"shaders: the translation index holds {len(index)} of the {len(carved)} carved shaders "
             f"({len(carved) - len(index)} were refused or failed spirv-val, listed in this stage's output): "
             "a port built from it would lack them. Fix the translator for that class and translate again")

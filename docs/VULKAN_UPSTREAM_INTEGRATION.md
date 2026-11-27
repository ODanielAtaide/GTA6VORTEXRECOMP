# Vulkan upstream integration — 2026-11-27

Goal: reuse public compatibility and compiler research in Vortex while developing a Vulkan/SPIR-V GPU backend. Keep main unchanged. No commercial executable, extracted shaders, assets, binary patches or generated proprietary caches are imported. Use original or otherwise authorized fixtures for validation.

## Imported in this round

All three source files are from the GPL-3.0-or-later workspace [Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD), built on [Marvisak/VIctory](https://github.com/Marvisak/VIctory) and its contributors. Preserve source notices and GPL licensing; upstream Git history remains the authoritative record of authorship. The source blob IDs below identify the exact retrieved content, not commit IDs.

| Source under crates/victory-shader/src | Source blob | Vortex integration |
|---|---|---|
| [structure.rs](https://github.com/Belobug/VIctory-Belobug-AMD/blob/main/crates/victory-shader/src/structure.rs) | 719805d84a9ded2a488cdaea5ba9b7496f2e8d19 | d14b4c48107533f596bcc16646f12ac1841e5163 — reuse guard analysis for consecutive instructions sharing EXEC; upstream test retained |
| [io.rs](https://github.com/Belobug/VIctory-Belobug-AMD/blob/main/crates/victory-shader/src/io.rs) | ba53eb0591d04fbf7bea9a77421d7beaef88d1e7 | 9e7d0ab2fb28446ac81d3cae408b9ae7fd72e808 — preserve partial interpolation values used through Phi and standalone FP16 interpolation; upstream tests retained |
| [ir.rs](https://github.com/Belobug/VIctory-Belobug-AMD/blob/main/crates/victory-shader/src/ir.rs) | 410d97fb89f91effec66111b9599fbfc672cbbf0 | decc102823864e8296176a35817fdab4e0fb0713 — selected redundant branch elimination, new local regression |

All imports are pending local compilation and regression testing. These passes are backend-neutral, but the experimental SPIR-V emitter does not yet consume the full structured IR or stage IO. They do not establish any frame-rate improvement.

## Upstream comparison

- Belobug README and [findings](https://github.com/Belobug/VIctory-Belobug-AMD/blob/main/docs/findings.md) describe major CPU-side costs in descriptor scanning, memory synchronization, submission preparation and command recording. Findings source blob 6ad9b32b11b8bc1b9e827fb22c09829f807bdac5; README blob ef201af2d3a86258ef591b127efe6b683567a897. Its 44.9 ms frame on an RX 7900 XTX, i9-12900K, 64 GB RAM is a reported D3D12 measurement, not a Vulkan benchmark. Reported private memory ~30 GB and VRAM ~10.5 GB exceed the original Vortex test configuration.
- [Marvisak/VIctory](https://github.com/Marvisak/VIctory): existing ancestry; reuse frontend and non-graphics runtime rather than reconstructing them.
- [DexNom/VIctory](https://github.com/DexNom/VIctory): its README blob c6fb6fce224daf75ba780bf703e6ddff42fac42c describes texture streaming, sparse textures and lighting work. DirectX-specific rendering/upscaling is not a Vulkan import.
- [fxceless/victory-fork](https://github.com/fxceless/victory-fork): README blob f5749eeed86d7bdc8d4b8bc0f2a0831a653a6435 says that as of 2026-11-27 it switched its base to VIctory-NVIDIA (959dcaf). Study relevant algorithms, do not merge an NVIDIA-specific backend.
- [VKD3D-Proton](https://github.com/HansKristian-Work/vkd3d-proton) is a candidate reference for Vulkan resource and synchronization mapping and its associated shader compiler, not an approved runtime dependency. Check exact component licenses and interfaces before importing.

## Next integration gates

1. Compare the GFX10 frontend as a coherent change set: decoder name caching, operand decoding, IR unspill/SSA improvements, resource analysis, native metadata and their tests. Do not overwrite local work blindly. Opcode table and DXIL parser blobs matched between the two inspected trees.
2. Establish a general typed SPIR-V lowering of the existing IR, including stage IO, resource operations, true subgroup masks and structured CFG. Reuse semantic analysis; do not translate the DXIL writer line by line. Evaluate existing public compiler libraries separately.
3. Define Vulkan resource/descriptor ABI and command submission. Use device-local allocations, cached staging, explicit lifetime tracking, timeline semaphores and synchronization2. Adapt selective dirty tracking, descriptor publication, and asynchronous preparation as independent changes. Do not skip required barriers, readbacks or rendering.
4. Introduce Vulkan backend under a distinct feature/entry point, retaining D3D12 only as a reference during migration. Test original workloads in both paths. No D3D12 runtime requirement for the final Vulkan path.
5. Establish memory budgets and benchmark actual scenes only with authorized input. Measure CPU/GPU timings, p95 frame time, committed RAM/VRAM, and output correctness under equivalent settings. Do not infer FPS from synthetic compute timing.

This is an initial selective import, not a full upstream merge or a complete Vulkan port. Current branch still includes the legacy Direct3D 12 backend.

## Second import — SSA performance and register lifetime analysis

The maintainer's local regression run after the first import (commit 0161767) reported 37 passed tests and zero failures: structure 13, IO 7, IR 8, SPIR-V 9. This is CPU-side validation, not a GPU benchmark.

From the same Belobug IR source blob `410d97fb89f91effec66111b9599fbfc672cbbf0`:

- `707a0a9927107bac0ba8294f9e8130d1d4686a9f`: iterate live registers by set bits, compress SSA Phi replacement paths, retain the upstream 4096-block stress test.
- `8290601d79ec59177822d853dc891619a0db75ae`: import more precise unspill analysis for reused lanes and register lifetimes, including control-flow tracking and pairing. Adapt the instruction construction to the existing Vortex API instead of changing the public decoder struct.

The second import is pending local validation. The new lowering is still a diagnostic subset; these optimizations are compiler-front-end work, not a replacement for Vulkan command submission. Decoder name caching and native/resource metadata changes remain separate candidates because they have wider dependencies. No FPS claim is made.

## Third import — control flow and scalar propagation

At commit dc213dc the maintainer ran the complete vortex-shader library test suite: 224 passed, 0 failed, 2 ignored, 27.42 seconds. The 4096-block stress test passed. This includes legacy emitter tests; it is not evidence of a complete Vulkan backend.

- c12229757810d7a813a1c3791e9ec4087dc741a4: imported the upstream control-flow analysis for long-distance early-outs and scalar copy propagation across Phi joins, from Belobug IR blob 410d97fb89f91effec66111b9599fbfc672cbbf0. Added a local regression distinguishing identical from different scalar sources. Pending local compilation and full regression.

The imported passes retain their existing call sites and assumptions about masked effects; they must not be enabled in a future Vulkan lowering until its effect and subgroup semantics are verified. No runtime backend or FPS improvement is claimed.

## Vulkan infrastructure validation and persistent recording

Maintainer-reported local results (2026-11-27): shader library 225 passed, 0 failed, 2 ignored after f162a19; dirty upload planner 6 passed after ee0e2dd; Vulkan transfer regression at acef899 verified 16 MiB after full, 192 KiB and 64 KiB uploads (6.125, 2.842, 2.881 ms including full readback and wait); descriptor regression at 80af77b verified 4, 2 and 0 writes with expected shader outputs; parallel recording at 7505d76 verified all 16 MiB in eight rounds of 4096 fills. Median recording time was 0.658 ms serial versus 0.652 ms with transient threads. The latter difference is too small to establish a meaningful speedup.

Commit 61187495d71e7effba7a8d7834181c296fb63351 changes the synthetic recording benchmark to compare three modes in the same run: serial, four transient threads, and four persistent workers. Six rounds per mode, one warm-up excluded. Persistent worker startup is outside the timed recording loop; channel dispatch, recording and acknowledgement are inside. Each worker uses an independent command pool; CPU recording may overlap, but submission remains ordered on one queue and each round waits before pool reuse. All bytes are checked against an epoch-dependent reference. The new commit is pending local validation. No FPS, game compatibility or completed runtime integration claim follows from these synthetic measurements.

## Shared Vulkan host library

Maintainer validated persistent recording (0.651 ms serial, 0.234 ms permanent workers), async pipeline (5.845 ms depth one, 3.371 ms depth three, 96 verified rounds), and sparse runtime bindless descriptors (4/2/0 writes, exact shader outputs). These are synthetic results, not game FPS.

The validated device, compute, graphics, descriptor, transfer, cache and diagnostic SPIR-V source modules now live in the workspace crate `vortex-vulkan`. The standalone probe is a compatibility facade using the same library. The main CLI optionally links it with `--features vulkan` and exposes `vulkan-check`. The Vulkan host remains independent of D3D12. The legacy main application still links legacy HLE modules. Title execution with `run --backend vulkan` explicitly refuses rather than silently falling back to D3D12; native title command and shader coverage are not yet implemented. The lock adds ash 0.38.0+1.3.281 and libloading 0.8.9 with checksums verified against the crates.io index. Compilation and runtime regression are pending maintainer validation.

## Advanced native CFG experiment

Maintainer confirmed all earlier private-record MUBUF x2/x3/x4 tests on logical wave32 and wave64. New original module `spirv_cfg.rs` translates decoded native CFG edges into a structured SPIR-V loop/switch dispatcher with mutable register slots, bounded to 4096 block visits. It adds scalar branches/loops, actual physical subgroup EXEC voting and per-lane masked effects, bounded dynamic dword indexing, atomic add/unsigned min/max/exchange/compare-exchange, and 64-word workgroup LDS with a real workgroup barrier. LDS/barriers are rejected if the shader has EXEC mutations or subgroup-dependent branches. Ordinary stores are restricted to the private invocation index; dynamic reads require caller-provided race-free programs. The fixed host ABI maps s[4:7] to one storage descriptor, v0 to global ID, v1 to local ID and v2 to physical subgroup size. No general guest resource mapping or bindless lowering is claimed. Previous bindless tests remain separate. VCC/saveexec, arbitrary memory addressing, full control-flow coverage, graphics-stage lowering and title integration remain pending. The bounded dispatcher is a correctness-first implementation, not optimized structured lowering; hitting the budget can leave partial output. Twenty-four original modules (12 fixtures for each physical wave size) are emitted via `vulkan-advanced --emit-only <dir>` for spirv-val before GPU execution. This new code is pending build and GPU validation.

### VCC / SAVEEXEC and native resource mapping

The maintainer reported "TODOS OS TESTES APROVADOS" after running the previous combined test script. This is confirmation of the preceding test suite, not a game FPS measurement.

The next experimental revision adds physical subgroup ballot masks, native unsigned VCC comparisons, VCCZ/VCCNZ branches, AND/OR/XOR/ANDN2 SAVEEXEC and register-based EXEC restoration, plus an explicit two-buffer ABI (s[4:7] -> binding 0, s[8:11] -> binding 1). Each descriptor has independent bounds; immutable sets retain both backing resources and reject duplicate buffer IDs. The two-resource regression reads a shorter independent source and verifies out-of-range reads return zero. The VCC/SAVEEXEC fixture checks masked writes, wave-wide control and restoration on wave32 and wave64. All 28 emitted modules must pass spirv-val and GPU comparison before the new revision is marked validated.

This is still a fixed mapping, not general guest descriptor discovery or dynamic bindless indexing. Full control flow, general SALU mask arithmetic, wave-dependent LDS synchronization, unbounded execution, graphics-stage shader translation, and title integration remain unsupported. No new game execution or FPS claim. The previous 4096-block execution cap remains.

### Initial native vertex and pixel stages

The maintainer reported "TODOS OS TESTES APROVADOS" for the 28-module VCC/SAVEEXEC and two-resource suite. The next original module, `spirv_graphics.rs`, lowers decoded GFX10 straight-line full-EXEC VALU and uncompressed EXP instructions into actual SPIR-V Vertex/Fragment entry points. The explicit diagnostic ABI maps vertex xyz/rgba attributes to v0..v6, and the pixel interpolated rgba to v0..v3. POS0, PARAM0 and MRT0 are required, and native FP32 arithmetic is emitted. The session creates both native stages and executes indexed draws, checking color/depth rejection and nonconstant varying interpolation against CPU references. `vulkan-native-graphics --emit-only <dir>` writes two modules for spirv-val; `vulkan-native-graphics` runs the GPU tests.

Pending compilation and GPU validation. This does not translate general fetch shaders or native interpolation instructions; the host provides already mapped attributes and interpolated varyings. No textures/samplers, graphics resource descriptors, arbitrary graphics control flow, compressed exports, multi-render-target output or guest runtime linkage in this stage. No FPS or title execution claim.

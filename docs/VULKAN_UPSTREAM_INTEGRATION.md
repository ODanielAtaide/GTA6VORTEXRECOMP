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

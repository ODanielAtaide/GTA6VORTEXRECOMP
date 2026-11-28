# Initial live Vulkan runtime

## Leonida runtime integration

`vortex run --backend vulkan --execute --vulkan-runtime <directory>` starts the native Xbox runtime from Leonida.
The runtime keeps the Xbox loader, system shims, shader library, pipeline construction, resource management,
command execution and Vulkan presenter in one process. The Vortex integration compiles the Xbox graphics factory
with Vulkan as its only backend and checks for that build before launching the title. File writes use the `overlay`
directory beside the prepared runtime. [Pinned Leonida source](https://github.com/ProjectLeonida/mandy/tree/39f52f39c435e04273bc822f258f2c4bb1b7fec0).

On Windows x64, prepare a checkout of the linked revision with the Visual Studio 2022 C++ tools, CMake, Python
3.12 or newer and the Vulkan SDK. Put the SDK's `spirv-as` and `spirv-val` on `PATH` and set `LLVM_MC` to an LLVM
21 or newer `llvm-mc` with the AMDGPU target. The preparation script uses the local game package to build and
validate its shader library. Keep the runtime output and work directories outside both source checkouts.

```powershell
.\tools\build-leonida-vulkan.ps1 -LeonidaSource $source -GameDirectory $game `
    -OutputDirectory $runtime -WorkDirectory $work
cargo build --release --locked -p vortex --features vulkan,diagnostics

$env:LEONIDA_ARENA_MB = '2048'
$env:LEONIDA_PIPELINE_COMPILE = 'on-demand'
$env:LEONIDA_COMPUTE_WARMUP = '0'
$env:LEONIDA_ASYNC_PIPELINES = '0'
$env:LEONIDA_FRAME_PACING = '1'
$env:LEONIDA_KEYBOARD_PAD = '1'
$env:LEONIDA_XMEM_BACKING_DIR = Join-Path $runtime 'xmem'
New-Item -ItemType Directory -Path $env:LEONIDA_XMEM_BACKING_DIR -Force | Out-Null
.\target\release\vortex.exe run $exe --console series-s --backend vulkan --execute --vulkan-runtime $runtime
```

This is the Series S profile verified on Windows with the Radeon RX 6600 XT. Enter maps to Xbox A, the
arrow keys move the selection, Esc opens Settings when offered, and Q and E change its tabs. The
keyboard pad only reads input while the game window has focus. Required pipelines are built before use.

Leonida remains a separate GPL-3.0-or-later source checkout with its upstream license and third-party notices.
The build uses no game-derived inputs from this repository. The `--backend vulkan` route without
`--vulkan-runtime` uses the experimental in-process implementation described below.

The runtime build also patches a recognized cache write-back loop in the mapped executable on x64 processors
that lack CLWB. The PE loader selects CLFLUSHOPT or CLFLUSH according to CPUID. The replacement writes back
dirty data and invalidates the cache line while preserving the operand and loop control flow. The package
on disk is unchanged. [Intel instruction reference](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html).

The branch started 236 commits ahead of `main` at `4e5e0b7`. The main branch uses a Windows D3D12 device and DXGI
presentation. The branch at `31474b8` initializes Vulkan, records the retail texture barrier and stops before
presentation at an unsupported render-target layout. Its serialized PSO handlers return `E_NOTIMPL`. Leonida
implements both Xbox PSO slots, `0x1c8` for graphics and `0x218` for compute, in the device proxy that also creates
the real Vulkan pipelines. RDNA2 Commons imports that engine and keeps the title build profile upstream.
[Branch comparison](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/compare/main...experimental/amd-vulkan-spirv).
[Leonida Xbox device](https://github.com/ProjectLeonida/mandy/blob/39f52f39c435e04273bc822f258f2c4bb1b7fec0/runtime/shims/d3d12_xs_device.cpp).
[RDNA2 Commons engine lineage](https://github.com/rdna2-commons/rdna2-commons/blob/main/engine/README.md).

OpenScarlett has a direct Vulkan command route and an independent Xbox device proxy. Scarlight's shipped route uses
Proton and vkd3d-proton, and XCL's shader path targets DXIL. The shadPS4 texture cache uses the PS4 memory model.
Rebox integrates the AnyPS5 shader recompiler with a modified vkd3d-proton. These implementations informed the
comparison of command, shader and texture paths.
[OpenScarlett command list](https://github.com/californiaAlmonds/OpenScarlett/blob/48a98fcd31a9f5d6c8ba344592215c1539d6e7dc/src/gpu/d3d12_xs/command_list_x.h).
[Scarlight architecture](https://github.com/pablogonz12/Scarlight#how-it-works).
[XCL shader path](https://github.com/xcl-dev/xcl/blob/6089122a1c96dff16e93b6fdf04c8ed20cfb6102/crates/xcl/src/gpu/shader.rs).
[shadPS4 texture cache](https://github.com/shadps4-emu/shadPS4/blob/main/src/video_core/texture_cache/texture_cache.cpp).
[Rebox architecture](https://github.com/puffnfresh/rebox).

### Local runtime verification

The Xbox package 1.0.2.1 mapped four modules with zero unresolved imports. The Vulkan swapchain presented
the account prompt, intro, first-run options and full Settings screen. Controller input changed the navigation
preview and switched the selected Settings tab from Control Scheme to Controls. The Settings window ran at
about 19–22 frames per second in the measured run. The screenshots are local. The first integrated run stalled
after account selection with cache write-back loops repeatedly entering the illegal-instruction handler. On the
same CPU, the patched build ran five loops with CLFLUSHOPT and advanced to the menus.

The main pack validated 26,503 SPIR-V modules. Separately, 2,683 of the 2,684 serialized library compute programs
validated. The remaining program uses `s_sethalt`, which this translator cannot lower. The Windows AMD driver
allows requesting a subgroup width for compute only, and the runtime logged the limitation for some pixel shaders.
It also reported an unsupported Xbox UAV for format 56. After the terms screens, the running game presented a
loading indicator and logged Xbox queue waits that timed out. The main screen after that load and gameplay were
outside the verified result.

## Guest buffer address provenance foundation
Live resource registration now retains checked linear-buffer spans, removing them at destruction. Root-view diagnostics distinguish null, unmapped, ambiguous/aliased and uniquely registered addresses with remaining byte extent. The registry never dereferences shader-provided addresses and does not fabricate device addresses. The new range regression checks overflow, end boundaries and overlaps. This is CPU-side provenance for a future Vulkan storage-address ABI, not shader pointer translation, buffer residency or completed GPU dispatch. Unknown/aliased addresses remain unsupported. Windows compilation and tests remain pending.

## Follow-up survey: instruction-local and module-level coverage
The compute emitter permits ordinary VGPR0 destinations (including bounded LDS loads), while ordinary global stores still require the original input SSA value and zero-offset v0 address. A separate symbolic instruction probe collects every locally failing compute instruction, including those hidden behind earlier CFG failures; it generates no executable shader and does not imply full-program support. The offline scanner now reports these counts and writes modules.txt, distinguishing source modules and stage coverage. Guest pointers and real pipeline ABI are still unresolved. Build and validation of this revision are pending.

## Offline coverage-driven compiler increment
The initial offline scan decoded every discovered native program into IR but no candidate SPIR-V was emitted. The experimental compute emitter now supports the full u8 VGPR numbering range, with separate bounded block/instruction/register limit diagnostics, and a restricted unmodified integer VOP3 arithmetic subset (shift-add/or, add-shift, and-or, three-input add/or/xor, low multiply, unsigned min/max and population count). The unchanged global-index write restriction and guest-dispatch prohibition remain. Increased compile-time limits do not alter the runtime loop watchdog. This change does not implement raw guest pointers, real resource bindings, float-mode semantics or graphics input mapping. Use `--code-only` (PowerShell `-CodeOnly`) to explicitly restrict a follow-up offline survey to EXE/DLL/SGD5 inputs; all other files are excluded and counted, not silently reported as scanned. Compilation, validation and live execution of this increment remain pending.

## Offline precompile coverage survey
`vulkan-precompile --game-dir <directory> --workers 6 --validate` scans regular local files in 8 MiB blocks, without modifying the input. Recognized unprotected DXBC containers and supported standalone SGD5 files up to 128 MiB are analyzed; opaque archives are not decrypted or unpacked. Unique containers are deduplicated by full BLAKE3 digest. Each is decoded, lowered to IR, surveyed for structured flow, tracked resources, stage I/O and diagnostic SPIR-V emission. Optional spirv-val verifies the diagnostic module's structure only. Cache keys include the running executable hash and validator version. Only aggregate metadata is persisted; temporary validation modules are removed. The configured cache must be outside the game and source trees. Reports include files.tsv, containers.tsv, analysis.toml and report.txt, ranked by failure reason and opcode frequency. The scanner reports zero guest-ready pipelines: there is no safe general guest SPIR-V ABI or complete pipeline state translation yet. Use tools/precompile-vulkan.ps1 to back up, update, build and scan; optional -Launch starts the experimental runtime after analysis. No local build or scan of this revision has been verified.

## Descriptor heap bindings and lane mask population count
The latest local run compiled and reached SetDescriptorHeaps (0x100). It also reported at least 8192 unsupported 0x1c8 pipeline requests, so the optional failure contract is not evidence of renderer coverage. The live list now retains registered shader-visible view/sampler heaps, rejects duplicate types, invalidates root descriptor tables on heap changes and reports table handle validity. It does not yet publish guest descriptors into Vulkan sets. The experimental CFG emitter now lowers MBCNT low/high to subgroup lane prefix masks and OpBitCount, with EXEC ballot support and unchanged-global-index validation for non-atomic storage writes. This does not solve the real guest ABI or permit title dispatch. Local compilation and tests are pending. Pass the local executable as -Exe to tools/run-live-vulkan.ps1; the script no longer contains a machine-specific path.

## Optional PSO return contract and real compute opcode diagnostics

Live execution reached ComputeX description creation, native shader IR and DirectStorage queue setup, then stopped at device slot 0x218. The original implementation of slots 0x1c8 and 0x218 returns E_NOTIMPL and null (the guest can omit those unsupported pipelines). The live table now uses the same explicit failed-creation contract, with sampled warnings. These methods are NOT implemented; no native pipeline is created for them. Other unsupported methods still stop. The first real compute shader reported local size [32,1,1] and an unsupported operation at offset 0x1c; the new log names the exact opcode and registers, plus hardware input metadata. The fixture compiler is not used to execute guest work. Build and live execution remain unverified. Use tools/run-live-vulkan.ps1 for the backed-up live launch.

## Pipeline description revision
The owner confirmed actual device initialization, recorded guest synchronization and version 1.1 root signature serialization. The new live device handles Compute/ComputeX, graphics and known pipeline-stream creation by retaining guest descriptions, root metadata and owned shader containers. Pipeline binding and root parameter state are retained separately for graphics/compute. First pipeline creation and first use inspect native GFX10 metadata and build IR. Candidate SPIR-V emitted by the fixture-specific compiler is NOT used for dispatch. Unknown extension contents and actual guest resource/system register mappings remain unimplemented. Nonzero draw/dispatch still stops before execution; no dummy VkPipeline, no host D3D12 path and no claim of a rendered frame. Compilation and GPU execution of this revision are pending. Logs identify native shader ABI and lowering gaps without exporting the original shaders.

This revision removes the unconditional CLI rejection of `run --backend vulkan --execute` on Windows builds with the Vulkan feature. The guest creation interface selects a separate device table and creates the AMD Vulkan 1.3 device and worker at the real device request. The guest interface is retained for ABI compatibility. Host D3D12 device creation, legacy command list access, and root signature host-library conversion are blocked in this mode.

Supported live operations: CPU guest heap/resource and descriptor bookkeeping, list recording/closing/reset, ordered synchronous buffer copies through Vulkan staging/device-local/readback (chunked; retains guest references), committed single-level nonaliased RGBA8/D32 target clears, full image copies between supported targets, and a Win32 Vulkan swapchain blit/present of an actual guest-produced color target. Unsupported commands stop and identify the slot or reason. The presenter does not manufacture a frame or initialize an unknown present source. A successful swapchain present may be a clear-only image, not a rendered scene.

Guest pipeline creation, shader signatures/resource mapping, draw and dispatch state, native root signature conversion, enhanced barriers, general placed/tiled images and pending cross-queue waits are not yet connected. The title is expected to encounter a blocker before a complete rendered frame. No build, title or GPU execution of this revision has been verified here. The legacy renderer remains unchanged in normal mode. No claim of game compatibility or FPS.

Run on a local authorized executable:
```powershell
cargo build --release --locked -p vortex --features "vulkan,diagnostics"
.\target\release\vortex.exe run "PATH_TO_EXECUTABLE" --backend vulkan --execute
```

Milestones: LIVE_VULKAN_REQUESTED (CLI selection), VULKAN_GUEST_DEVICE_READY (actual native device), VULKAN_GUEST_COPY_OK (live guest buffer copy), FIRST_VULKAN_GUEST_PRESENT_OK (actual accepted Vulkan present), VULKAN_RUNTIME_BLOCKER (first unported call or Vulkan error). Only the last two indicate presentation or its blocker; a successful device is not a game frame.

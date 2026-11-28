# Vortex live Vulkan

The title is loaded by Vortex's Rust PE loader and runs in the Vortex process. The Xbox imports are resolved
by Vortex HLE. The graphics device is built statically into that executable from the repository. Xbox COM
interfaces remain visible to the title, including the format and method names in those interfaces.
The graphics implementation issues Vulkan commands.

The experimental branch started from `31474b8`, which opened a Vulkan device but stopped before its first
present. A previous integration at `97c1fa0` opened Settings with an external runtime, which is historical
reference evidence. Its commit is recoverable at `restore/pre-native-vulkan-97c1fa0` in the development checkout.

## Build and run

Use Windows x64, the pinned Rust toolchain, Visual Studio 2022 C++ Build Tools, Windows SDK, CMake, Python
3.12 or newer, and the Vulkan SDK. For shader preparation, put `spirv-as` and `spirv-val` on PATH and set
`LLVM_MC` to an LLVM 21+ `llvm-mc` with AMDGPU support. Set `VORTEX_PYTHON` or `CMAKE` if the defaults
do not resolve to the installed tools. The CMake component generates the COM declarations from the Windows
SDK and links the graphics component into Vortex. A source checkout is used for shader preparation.

```powershell
cargo build -p vortex --release --locked --features diagnostics
.\target\release\vortex.exe compile-title-shaders --game-dir $game --work $work --jobs 4
.\target\release\vortex.exe run $exe --console series-s --execute --shader-pack (Join-Path $work 'shaders/spirv')
```

Keep the title, shader outputs, caches and captures outside the repository. The offline compiler reads the
user's package, extracts RDNA2 from console shader containers, uses the AMDGPU disassembler and emits SPIR-V.
The application does not emit DXIL in this path. The driver cache is stored under
`<VORTEX_CACHE_DIR>/vulkan-pipelines`. The work directory binds its cache to the package shader archive's
SHA-256 fingerprint and checks the translator fingerprint before reusing translated files.

## Validate the Vulkan output

When the Windows session has an active display, launch with the command above.
The renderer logs the first surface or swapchain error. The Vulkan SDK's
vulkaninfo can independently check Win32 surface support.

For diagnostics on a machine without an output, explicitly set
VORTEX_CAPTURE_OFFSCREEN to an absolute directory outside the repository. The same
Vulkan composition and blit paths then target a device-local image, copied to
binary PPM files after the Vulkan completion fence. This mode reports zero
window presents. Use VORTEX_CAPTURE_EVERY (default 30) and VORTEX_CAPTURE_LIMIT
(default 24) to control disk output. In a diagnostics build, set
VORTEX_KEYS_FILE to an empty text file for one run. Append one key per line
to feed that run's existing diagnostic GameInput device, using names such
as Enter, Back, Up or Down.

Run Rust regression tests with cargo test --workspace --release --locked
and check the source-only publication policy with cargo xtask policy.
The in-tree graphics probe can also be built by setting
VORTEX_NATIVE_GRAPHICS_TESTS=ON in the native CMake project and run with CTest.

## Frame timing

Set VORTEX_BENCHMARK_CSV to a path to append timing samples. Each row has a
run identifier, a monotonic elapsed time and interval in microseconds, and
the Vulkan output extent. The app records separate output classes for an
accepted window present and an offscreen submission. Neither field measures
display scanout, and capture diagnostics are not a gameplay benchmark.
Run vortex benchmark with the CSV path to summarize each run with its average,
minimum, 1% low and 0.1% low event rates.

Set VORTEX_TITLE_HUD=1 to display accepted Vulkan presents, host memory,
Vulkan VRAM budget and available pipeline creation counts in the window
title bar. VRAM is unavailable when the driver does not report a budget.

## Local runtime evidence

On the Radeon RX 6600 XT with AMD driver 26.11.2, the Vortex executable loaded
the local Xbox title, accepted its serialized pipeline descriptions and
submitted the title's graphics work to Vulkan. Connecting the guest XMem
ranges to the graphics device enabled the first visible title UI. The
1424×771 offscreen capture includes the Portuguese account and player prompts.
Diagnostic controller presses reached the title's “Experiência de navegação”,
text-size, captions, and brightness settings, then “Avisos legais”. The
headings and choice controls rendered. Wiring XG layout registration into the
Vulkan renderer restored the linear video planes and glyph atlases. The next
run rendered the opening animation and the navigation menu with readable
body text and the expected background. The runtime audit found the
Vulkan loader and AMD driver, zero child processes, and no external title
runtime or Direct3D 12 module. Windows also loaded DXGI and D3D11 interop
modules. On-screen presentation remains pending because the Win32 surface
query returned VK_ERROR_UNKNOWN on this system, including in vulkaninfo.
One other run stopped with an access violation on a title CMPXCHG16B that
received an address aligned to eight bytes. That case remains unresolved.
The title rendered the Terms of Service and accepted the confirmation through
its Xbox input path. In a later launch, after the storage writes and texture
dirty notifications were connected, the title displayed the story-loading
artwork and the storage log recorded successful reads and BCPack decoding
while the graphics queue remapped pages. That run ended on a guest access
violation while following a noncanonical pointer. Another run reached the story
artwork with the revised GPU mappings and write-back ordering. The AMD driver
returned VK_ERROR_OUT_OF_DEVICE_MEMORY for streamed textures before the title
faulted while writing to a null address. The captures and game files remain
local. Gameplay, the in-game performance overlay and 30 FPS
remain unverified.

## Components and provenance

- `vortex-loader` owns executable mapping and guest CPU instruction handling.
- `vortex-hle` owns Xbox platform imports and the CPU page map used by the native graphics device.
- `vortex-gpu/native` contains the statically linked Vulkan implementation, including Xbox PSO slots,
  resource and descriptor handling, command replay and multi-plane presentation. Its ABI bridges to
  Vortex's allocator, page mapping, guest module registry and private root-signature encoder.
  The graphics device distinguishes GPU reservations backed by their CPU peer
  from GPU reservations mapped by the graphics queue. It receives dirty
  notifications for title texture copies and storage writes to mapped physical pages.
  GPU shadow writes become visible to the title after the Vulkan submission retires.
  The Xbox resource path backs writable 32-bit depth with a separate Vulkan
  color image and synchronizes it on transitions. The sampled depth aspect of
  a 16-bit depth and stencil image retains its normalized GFX10 format.
- `vortex-shader/native` contains the offline package shader compiler used by the Vulkan device.
  Its tool sources are distinct from the smaller Rust ISA coverage and differential tests.
- Rendering and compiler source lineage and licenses are in [Upstream credits](UPSTREAM_CREDITS.md).

Test results from the in-process build are tracked separately from the old external-runtime experiment.
A successful build, an accepted PSO or a Vulkan present alone does not verify gameplay or 30 FPS.

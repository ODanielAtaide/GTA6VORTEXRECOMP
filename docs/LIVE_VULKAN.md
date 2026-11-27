# Initial live Vulkan runtime

This revision removes the unconditional CLI rejection of `run --backend vulkan --execute` on Windows builds with the Vulkan feature. The guest creation interface selects a separate device table and creates the AMD Vulkan 1.3 device and worker at the real device request. The guest interface is retained for ABI compatibility. Host D3D12 device creation, legacy command list access, and root signature host-library conversion are blocked in this mode.

Supported live operations: CPU guest heap/resource and descriptor bookkeeping, list recording/closing/reset, ordered synchronous buffer copies through Vulkan staging/device-local/readback (chunked; retains guest references), committed single-level nonaliased RGBA8/D32 target clears, full image copies between supported targets, and a Win32 Vulkan swapchain blit/present of an actual guest-produced color target. Unsupported commands stop and identify the slot or reason. The presenter does not manufacture a frame or initialize an unknown present source. A successful swapchain present may be a clear-only image, not a rendered scene.

Guest pipeline creation, shader signatures/resource mapping, draw and dispatch state, native root signature conversion, enhanced barriers, general placed/tiled images and pending cross-queue waits are not yet connected. The title is expected to encounter a blocker before a complete rendered frame. No build, title or GPU execution of this revision has been verified here. The legacy renderer remains unchanged in normal mode. No claim of game compatibility or FPS.

Run on a local authorized executable:
```powershell
cargo build --release --locked -p vortex --features "vulkan,diagnostics"
.\target\release\vortex.exe run "PATH_TO_EXECUTABLE" --backend vulkan --execute
```

Milestones: LIVE_VULKAN_REQUESTED (CLI selection), VULKAN_GUEST_DEVICE_READY (actual native device), VULKAN_GUEST_COPY_OK (live guest buffer copy), FIRST_VULKAN_GUEST_PRESENT_OK (actual accepted Vulkan present), VULKAN_RUNTIME_BLOCKER (first unported call or Vulkan error). Only the last two indicate presentation or its blocker; a successful device is not a game frame.

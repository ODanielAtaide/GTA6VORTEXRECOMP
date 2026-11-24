# External architecture references

Vortex executes native CPU code and translates graphics interfaces. External projects are references, not interchangeable backends. No third-party code was imported by this change.

| Reference | Evaluated concept | Decision |
|---|---|---|
| [UnleashedRecomp](https://github.com/hedge-dev/UnleashedRecomp) (GPL-3.0) | Pipeline compilation before first use | Maintain independent runtime prewarm catalogue; actual resource-loading integration would require a defined generic API and workload |
| [Direct3D 12 pipeline library](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device1-createpipelinelibrary) | Driver-dependent cache support and rejection of incompatible blobs | Keep native library optional; publish catalogue even when driver cache is unsupported or fails |
| [AMD D3D12MA](https://gpuopen.com/d3d12-memory-allocator/) | Heap allocation and memory budget | Assess against existing allocator before adding a dependency |
| [DXGI memory budget](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiadapter3-queryvideomemoryinfo) | Local and non-local memory usage | Existing host video_memory queries both segments; evaluate residency policy on synthetic workloads |

## Validation

The catalogue publication regression uses a temporary directory and requires no proprietary assets or driver library. Driver cache and runtime catalogue have independent dirty state. No FPS improvement is claimed. Test and profile on repeatable, authorized workloads before changing defaults.

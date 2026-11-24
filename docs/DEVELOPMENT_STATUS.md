# Vortex development report

**Snapshot: November 24, 2026.** This records the maintainer's local observations and the public Git history. No raw private logs or proprietary binary content are included.

## Objective

Investigate Xbox Series/Windows compatibility using native x86-64 execution and an open-source runtime, translate graphics and shaders to host Direct3D 12, and pursue correct, stable rendering at 30 FPS (33.33 ms per frame). This is a research target, not a guaranteed performance claim.

## Work completed

| Change | Commit | Result |
|---|---|---|
| Initial PE metadata inspector and tests | [9ae394a](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/9ae394a), [f778a45](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/f778a45) | Independent static inspection |
| Pinned public upstream, metrics and synthetic CI | [96add98](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/96add98) | Reproducible starting point |
| Windows runtime CI and benchmark comparison | [a945749](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/a945749) | Build and regression comparison |
| Verified editable source import | [75bbb7c](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/75bbb7c) | Upstream source snapshot imported |
| BCPack table provenance | [3c2f2d8](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/3c2f2d8) | Blob identity recorded; permission asserted by maintainer, not independently verified |
| PE directory bounds validation | [d59fbb7](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/d59fbb7) | Reject malformed directory metadata |
| TLS directory and callback array bounds validation | [299834d](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/299834d) | Reject truncated or unbounded callback metadata |
| Root workspace migration and upstream history merge | [70339ca](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/70339ca), [7c6f8d4](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/7c6f8d4) | Public upstream ancestry retained through 90228a7 |
| Public Vortex CLI and experimental performance profile | [576eb16](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/576eb16) | Vortex executable; optional ThinLTO profile |
| Packed linear texture copy coalescing | [ee0e755](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/ee0e755) | Fewer row-copy calls when layouts are tightly packed; preserves padded and volume cases |
| Streaming shader build fingerprint and concurrent cache publication | [b1c4159](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/b1c4159) | Bounded temporary hashing memory; cache writer race handling |
| CI per-revision validation | [8b1982f](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/8b1982f) | Early cache regressions, distinct revision runs |
| Bounded texture queue selection and remap coalescing | [4041393](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/4041393) | Select only necessary priority entries; preserve ordering |
| Remap pressure handling and span lookup reduction | [d2b1c9b](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/commit/d2b1c9b) | Compact redundant notifications when possible, avoid extra lookups |

Commit links above refer to the repository's public history. All changes must be reviewed in context; no proprietary workload-specific content is required to evaluate the general algorithms.

## Local validation

Windows 11 x64, Intel Core i5-9400, AMD Radeon RX 6600 XT (8 GB dedicated VRAM), 16 GB system RAM. Driver reported: 32.0.21045.29003. Direct3D feature level 12_2 reported by dxdiag.

The release build succeeded. Local test output from before the latest texture-queue changes and namespace migration (suite names below use the current naming):

| Suite | Passed | Failed | Ignored |
|---|---:|---:|---:|
| vortex-gpu | 111 | 0 | 1 |
| vortex-formats | 27 | 0 | 4 |
| vortex-pipeline | 5 | 0 | 0 |
| vortex-shader | 210 | 0 | 2 |
| Explicit ignored texture-check benchmark | 1 | 0 | — |
| Total successful invocations | 354 | 0 | — |

Some host-dependent tests may skip internally if a feature/device is unavailable. This is not proof that every hardware path was exercised. The separately executed synthetic upstream texture hash benchmark reduced 576 checks in 648.3383 ms to 48 checks in 62.2312 ms (about 10.4x faster in that isolated benchmark). **That optimization belongs to upstream, not Vortex.** It does not establish a game FPS improvement.

The Python diagnostics suite reached 17 tests. [Baseline Windows CI](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/actions/runs/45484355752) passed. Diagnostics workflows for [4041393](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/actions/runs/45491068106) and [d2b1c9b](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/actions/runs/45491159457) passed. Latest Windows CI was still running when this report was prepared; [current result](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/actions/runs/45491159448).

## Preliminary runtime telemetry

The maintainer reported reaching a menu and entering a complex 3D scene. A local log recorded the following windows. The workload's provenance and authorization have not been independently established; this is historical telemetry, not a reproducible benchmark or a compatibility endorsement.

| Window | FPS | Frame time | Preparation | GPU time | Copyback |
|---|---:|---:|---:|---:|---:|
| Menu | 30.0 | 33.3 ms | 3.6 ms | 5.8 ms | 0.0 ms |
| 3D scene | 1.0 | 1025.8 ms | 181.0 ms | 158.1 ms | 74.8 ms |
| Later 3D scene | 0.8 | 1313.4 ms | 271.5 ms | 200.9 ms | 121.7 ms |

The final sampled window also reported:
- Estimated GPU activity by the runtime's own queue clock: 23%; approximately 1016.5 ms/frame without queued work from the runtime. This is not an independent GPU utilization reading.
- Texture refresh work across threads: 88.9 ms/frame, including 62.0 ms waiting for stable memory mappings.
- CPU comparisons and snapshots of title buffer copies: 273 MB/frame, 249 MB in copies of at least 1 MiB.
- 24,841 pending indexed texture slots, 23,798 due; 1,264 pipelines being made.
- 1,258 draws dropped without a host pipeline and 1,756 indirect operations reported dropped. These are serious correctness limitations.
- Reported display mode 3840×2160; fitted window 1440×810. Internal rendering resolution is not established.

Timings may overlap and must not be added as mutually exclusive frame stages. The log does not establish whether compilation, memory pressure, CPU stalls or driver work dominates across repeated runs. There is no post-patch local FPS measurement yet. Rendering completeness, stability, audio, input coverage and ray-traced lighting are unverified.

## Next work

See the [roadmap](ROADMAP.md). Focus on correct rendering, repeatable synthetic workloads, memory safety, reduced submission stalls, pipeline completion and texture scheduling. Compare equivalent scenes with identical settings and warm/cold cache state. A frame that skips work must not be counted as an optimization.

## Credits and development process

Maintained by [@ODanielAtaide](https://github.com/ODanielAtaide), with a multi-model AI-assisted workflow: OpenAI ChatGPT/GPT, Anthropic Claude, Meta Llama and other locally hosted LLMs, as reported by the maintainer. ChatGPT assistance in code, testing, diagnosis and documentation is recorded in this development history; model-specific roles beyond those documented are not independently audited. The maintainer directs the work and runs Windows validation locally. GitHub's automated contributor graph includes inherited upstream authors; see [AUTHORS.md](../AUTHORS.md) for the distinction between direct project leadership and upstream attribution. Preserve all upstream authorship and licensing.

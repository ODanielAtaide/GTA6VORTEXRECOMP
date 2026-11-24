# Vortex

Experimental Windows compatibility and graphics research. Based on the public [VIctory](https://github.com/Marvisak/VIctory) project and [victory-gta6](https://github.com/fxceless/victory-gta6). Original authors, commit history, and GPL-3.0-or-later license are preserved. See [upstream credits and historical technical notes](docs/UPSTREAM_CREDITS.md). BCPack tables have [separate provenance](docs/BCPACK_PROVENANCE.md).

## Build and test

Windows x64, the pinned Rust toolchain, MSVC build tools and Windows SDK are required.

```powershell
cargo test --workspace --locked
cargo build --release -p vortex --locked
.\target\release\vortex.exe --help
```

Optional performance build (ThinLTO and one codegen unit):
```powershell
cargo build --profile performance -p vortex --locked
```

The performance profile is experimental; no FPS improvement is claimed without measurements. The internal crate names, configuration file names, log targets and VICTORY_* environment variables remain stable for compatibility with upstream tools.

## Benchmarking

Compare equivalent original or authorized workloads on the same hardware and driver. Use `python vortex_metrics.py benchmark.log` for sampled frame-time budget metrics and `python vortex_compare.py baseline.log candidate.log` for paired comparisons. A 30 FPS target permits about 33.33 ms per frame. Pipeline drops and incomplete rendering cannot be counted as successful optimization.

No commercial-game performance, rendering correctness, or full compatibility has been independently established. Do not publish proprietary content or private logs.

See [changes](docs/CHANGES.md), [architecture](docs/architecture.md), [benchmarks](docs/BENCHMARKS.md), and [license](LICENSE).

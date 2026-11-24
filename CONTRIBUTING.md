# Contributing to Vortex

Welcome! Vortex is open-source compatibility and graphics research. Contributions from experienced graphics engineers and first-time contributors alike are appreciated. You can help without any commercial game files.

## Areas where help is needed

- Rust runtime and loader safety; malformed input regression tests.
- Direct3D 12 resource management, descriptors and command submission.
- CPU/GPU synchronization, allocation pressure and memory-copy reduction.
- Shader compiler correctness, synthetic shader fixtures and differential tests.
- Texture layouts, scheduling, cache correctness and concurrency.
- Hardware compatibility testing with original or authorized workloads, documentation and reproducibility.

## Workflow

1. Open an issue describing a specific problem or proposal, or choose an existing issue.
2. Fork the repository and make a focused branch.
3. Add a regression test or a small reproducible example. Prefer synthetic fixtures that may be freely shared.
4. Run relevant tests and build checks on Windows. For performance changes, provide before/after measurements on the same workload, machine and settings.
5. Open a pull request with the problem, changes, evidence, expected tradeoffs and limitations.

```powershell
cargo test --workspace --locked
cargo build --release -p vortex --locked
```

Follow repository conventions and preserve existing copyright/license notices. New code should be compatible with the repository's GPL-3.0-or-later license. Clearly identify third-party dependencies and their licenses. AI-assisted contributions are welcome when reviewed and understood by their submitter; disclose assistance where relevant. Do not present unmeasured results as established facts.

## Performance acceptance

The goal is correct rendering and frame times within 33.33 ms, not dropping work to inflate FPS. Report missing draws, visual regressions, crashes and memory usage alongside performance. Distinguish local observations from independently reproducible results. See [BENCHMARKS.md](docs/BENCHMARKS.md).

## Responsible research

Do not attach proprietary executables, SDK files, commercial assets, unauthorized builds, keys, credentials or private logs. Use original synthetic test cases or materials you are authorized to share. Redact usernames, absolute paths and identifiers from diagnostics. Do not ask for game downloads or ways to bypass access controls. Keep issue discussion respectful and focused on the engineering problem.

Thank you for helping improve open-source compatibility research.
